//! Ingest end to end, through the real router: the conductor checks who signed, sites — a stand-in
//! here — is told by a Source assertion and has the last word.

use axum::{
	Router,
	body::{Body, to_bytes},
	http::{HeaderValue, Request, StatusCode, header},
	response::IntoResponse,
	routing::{get, post},
};
use jiff::Timestamp;
use sa_auth::{Principal, Service, SourceKind};
use sa_core::ids::BrandId;
use sa_panel::testing::{Signed, TestDb, admin, panel, sign, sign_get};
use sa_server::{
	http::Limits,
	testing::{Log, app, app_with, upstream},
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Sites as ingest reaches it: a verdict per event, a 409 with `Retry-After` for a batch naming
/// `"defer"`, and a lead found by its ref.
async fn sites() -> (String, Log) {
	upstream(
		Router::new()
			.route(
				"/api/ingest/v1/events",
				post(|body: axum::body::Bytes| async move {
					if body.windows(5).any(|w| w == b"defer") {
						let mut res = (StatusCode::CONFLICT, axum::Json(json!({"results": [{"index": 0, "status": "deferred"}]}))).into_response();
						res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
						return res;
					}
					(StatusCode::MULTI_STATUS, axum::Json(json!({"results": [{"index": 0, "status": "accepted"}]}))).into_response()
				}),
			)
			.route("/api/ingest/v1/leads/by-ref/{brand}/{message_ref}", get(|| async { axum::Json(json!({"lead_id": "L-1"})) })),
	)
	.await
}

async fn with_source(db: &TestDb) -> (sa_panel::Panel, String) {
	let panel = panel(db).await;
	let brands = [BrandId::parse("aquafix").unwrap(), BrandId::parse("vifnet").unwrap()].into();
	let secret = panel
		.add_source("aquafix-site", SourceKind::Site, brands, Some(sa_core::ids::SiteOrigin::parse("https://aquafix.top").unwrap()))
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	(panel, secret)
}

async fn send(app: &Router, uri: &str, signed: &Signed, all: bool) -> (StatusCode, Option<HeaderValue>, Value) {
	let mut req = Request::post(uri)
		.header("content-type", "application/json")
		.header("x-sa-key-id", &signed.key_id)
		.header("x-sa-timestamp", &signed.timestamp);
	if all {
		req = req.header("x-sa-signature", &signed.signature);
	}
	let res = app.clone().oneshot(req.body(Body::from(signed.body.clone())).unwrap()).await.unwrap();
	let (status, retry) = (res.status(), res.headers().get(header::RETRY_AFTER).cloned());
	let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
	(status, retry, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

const EVENTS: &str = "/api/ingest/v1/events";

#[tokio::test]
async fn a_signed_batch_reaches_sites_with_its_grant_and_comes_back_as_sites_answered() {
	let db = TestDb::create().await;
	let (sites, seen) = sites().await;
	let (panel, secret) = with_source(&db).await;
	let app = app(panel, admin(), &sites);
	let now = Timestamp::now();
	let body = br#"{"events":[{"id":"x"}]}"#;

	let (status, _, answer) = send(&app, EVENTS, &sign("aquafix-site", &secret, body, now), true).await;
	assert_eq!((status, answer["results"][0]["status"].as_str()), (StatusCode::MULTI_STATUS, Some("accepted")));
	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!((got.path_and_query.as_str(), got.body.as_ref()), (EVENTS, body.as_slice()), "the body as it came");
	assert_eq!(got.headers["content-type"], "application/json");
	let Principal::Source(source) = got.assertion(Service::Sites, Some(&got.body)).principal else {
		panic!("a key's batch is a source's")
	};
	assert_eq!((source.key_id.as_str(), source.kind), ("aquafix-site", SourceKind::Site));
	assert_eq!(source.brands, ["aquafix".to_owned(), "vifnet".to_owned()].into());

	let deferred = sign("aquafix-site", &secret, br#"{"events":[{"id":"defer"}]}"#, now);
	let (status, retry, answer) = send(&app, EVENTS, &deferred, true).await;
	assert_eq!((status, retry.as_ref().map(|r| r.to_str().unwrap())), (StatusCode::CONFLICT, Some("30")), "{answer}");
}

#[tokio::test]
async fn a_refused_signature_never_reaches_sites() {
	let db = TestDb::create().await;
	let (sites, seen) = sites().await;
	let (panel, secret) = with_source(&db).await;
	let app = app(panel.clone(), admin(), &sites);
	let now = Timestamp::now();
	let body = br#"{"events":[]}"#;

	let (status, _, _) = send(&app, EVENTS, &sign("aquafix-site", &secret, body, now), false).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED, "no signature");
	let (s1, _, b1) = send(&app, EVENTS, &sign("aquafix-site", &"f".repeat(64), body, now), true).await;
	let (s2, _, b2) = send(&app, EVENTS, &sign("nobody", &secret, body, now), true).await;
	assert_eq!((s1, s2), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
	assert_eq!(b1, b2, "an unknown key and a bad signature read the same");
	let tampered = Signed {
		body: br#"{"events":[{}]}"#.to_vec(),
		..sign("aquafix-site", &secret, body, now)
	};
	assert_eq!(send(&app, EVENTS, &tampered, true).await.0, StatusCode::UNAUTHORIZED, "another body");
	let stale = sign("aquafix-site", &secret, body, now - jiff::SignedDuration::from_mins(10));
	let (status, _, answer) = send(&app, EVENTS, &stale, true).await;
	assert_eq!((status, answer["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("timestamp outside the replay window")));
	assert!(panel.revoke_source("aquafix-site").await.unwrap());
	assert_eq!(send(&app, EVENTS, &sign("aquafix-site", &secret, body, now), true).await.0, StatusCode::UNAUTHORIZED, "revoked");
	let huge = Signed {
		body: vec![b' '; 4 * 1024 * 1024 + 1],
		..sign("aquafix-site", &secret, body, now)
	};
	assert_eq!(send(&app, EVENTS, &huge, true).await.0, StatusCode::PAYLOAD_TOO_LARGE);
	assert!(seen.lock().unwrap().is_empty(), "nothing reached sites");
}

#[tokio::test]
async fn a_bots_lookup_reaches_sites_as_its_source_over_no_body() {
	let db = TestDb::create().await;
	let (sites, seen) = sites().await;
	let panel = panel(&db).await;
	let secret = panel
		.add_source("aquafix-bot", SourceKind::Bot, [BrandId::parse("aquafix").unwrap()].into(), None)
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let app = app(panel, admin(), &sites);
	let path = "/api/ingest/v1/leads/by-ref/aquafix/AQ-7K3F?fresh=1";
	let look = |signed: Signed| {
		let app = app.clone();
		async move {
			let req = Request::get(path)
				.header("x-sa-key-id", &signed.key_id)
				.header("x-sa-timestamp", &signed.timestamp)
				.header("x-sa-signature", &signed.signature)
				.body(Body::empty())
				.unwrap();
			let res = app.oneshot(req).await.unwrap();
			let status = res.status();
			(status, to_bytes(res.into_body(), usize::MAX).await.unwrap())
		}
	};
	let now = Timestamp::now();
	let (status, body) = look(sign_get("aquafix-bot", &secret, path, now)).await;
	assert_eq!((status, body.as_ref()), (StatusCode::OK, br#"{"lead_id":"L-1"}"#.as_slice()));
	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!(got.path_and_query, path);
	let Principal::Source(source) = got.assertion(Service::Sites, Some(b"")).principal else {
		panic!("a lookup is a source's")
	};
	assert_eq!((source.key_id.as_str(), source.kind), ("aquafix-bot", SourceKind::Bot));

	let other_path = sign_get("aquafix-bot", &secret, "/api/ingest/v1/leads/by-ref/aquafix/AQ-0000", now);
	assert_eq!(look(other_path).await.0, StatusCode::UNAUTHORIZED, "a signature opens its one lookup");
	assert!(seen.lock().unwrap().is_empty());
}

/// A body that never arrives.
fn stalled() -> Body {
	Body::from_stream(futures::stream::pending::<Result<Vec<u8>, std::io::Error>>())
}

fn request(key_id: &str, timestamp: &str, body: Body) -> Request<Body> {
	Request::post(EVENTS)
		.header("x-sa-key-id", key_id)
		.header("x-sa-timestamp", timestamp)
		.header("x-sa-signature", "00")
		.body(body)
		.unwrap()
}

#[tokio::test]
async fn costly_requests_are_refused_before_the_body_is_read() {
	let db = TestDb::create().await;
	let (sites, _) = sites().await;
	let panel = panel(&db).await;
	let now = Timestamp::now().as_second().to_string();
	let stale = (Timestamp::now().as_second() - 600).to_string();
	let quick = std::time::Duration::from_secs(5);

	// Both would hang on the stalled body if it were read first.
	let app = app(panel.clone(), admin(), &sites);
	for (key_id, ts) in [("aquafix-site", stale.as_str()), ("Not a key id", now.as_str())] {
		let res = tokio::time::timeout(quick, app.clone().oneshot(request(key_id, ts, stalled())))
			.await
			.expect("answered without the body")
			.unwrap();
		assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{key_id} {ts}");
	}

	let limits = Limits {
		max_concurrent: 1,
		body_timeout: std::time::Duration::from_millis(200),
		request_timeout: std::time::Duration::from_secs(5),
		..Default::default()
	};
	let app = app_with(panel, admin(), "dev@localhost", &sites, limits);
	let res = tokio::time::timeout(quick, app.clone().oneshot(request("aquafix-site", &now, stalled()))).await.unwrap().unwrap();
	assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT, "a body too slow to arrive");

	// One slow request holds the only slot; the next is shed, not queued.
	let (tx, rx) = tokio::sync::oneshot::channel::<()>();
	let held = futures::stream::once(async move {
		rx.await.expect("sent below");
		Ok::<_, std::io::Error>(b"{}".to_vec())
	});
	let first = tokio::spawn(app.clone().oneshot(request("aquafix-site", &now, Body::from_stream(held))));
	tokio::time::sleep(std::time::Duration::from_millis(50)).await;
	let res = app.clone().oneshot(request("aquafix-site", &now, Body::from("{}"))).await.unwrap();
	assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
	tx.send(()).unwrap();
	assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::UNAUTHORIZED, "the held one goes on to be judged");

	let res = app.oneshot(Request::get("/health").body(Body::empty()).unwrap()).await.unwrap();
	assert_eq!(res.status(), StatusCode::OK);
}

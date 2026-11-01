//! The HTTP ingest end to end, through the real router, against a real Postgres.

use axum::{
	body::{Body, to_bytes},
	http::{Request, StatusCode},
};
use jiff::Timestamp;
use panel::testing::{Signed, TestDb, event, messenger_lead, panel, sign, sign_get};
use panel_core::ids::BrandId;
use sa_auth::SourceKind;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn send(app: axum::Router, signed: &Signed, headers: &[&str]) -> (StatusCode, Value) {
	let mut req = Request::post("/api/ingest/v1/events").header("content-type", "application/json");
	for h in headers {
		req = match *h {
			"x-sa-key-id" => req.header(*h, &signed.key_id),
			"x-sa-timestamp" => req.header(*h, &signed.timestamp),
			"x-sa-signature" => req.header(*h, &signed.signature),
			other => panic!("{other}"),
		};
	}
	let res = app.oneshot(req.body(Body::from(signed.body.clone())).unwrap()).await.unwrap();
	let status = res.status();
	let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
	(status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

const ALL: [&str; 3] = ["x-sa-key-id", "x-sa-timestamp", "x-sa-signature"];

#[tokio::test]
async fn ingest_over_http() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = panel
		.add_source("aquafix-site", SourceKind::Site, [BrandId::parse("aquafix").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let app = panel_server::http::router(panel);
	let now = Timestamp::now();
	let lead = json!({"brandId": "aquafix", "leadId": "L-1"});
	let created = event("lead.created", now, "site", lead.clone(), json!({"channel": "form"}));
	let events = [
		created.clone(),
		created,
		event("lead.created", now, "site", json!({"brandId": "vifnet", "leadId": "V-1"}), json!({"channel": "form"})),
		event("review.new", now, "site", lead, json!({})),
	];

	let good = sign("aquafix-site", &secret, &events, now);
	let (status, body) = send(app.clone(), &good, &ALL).await;
	assert_eq!(status, StatusCode::MULTI_STATUS, "{body}");
	let results = body["results"].as_array().unwrap();
	let statuses: Vec<&str> = results.iter().map(|r| r["status"].as_str().unwrap()).collect();
	assert_eq!(statuses, ["accepted", "duplicate", "rejected", "accepted"]);
	assert_eq!(results[2]["reason"], "this key may not write for brand vifnet");
	assert_eq!(results[2]["index"], 2);
	assert_eq!(results[0]["id"], events[0]["id"]);
	assert!(results[3]["reason"].as_str().unwrap().contains("not registered"));

	let (status, body) = send(app.clone(), &good, &ALL[..2]).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

	let forged = sign("aquafix-site", &"f".repeat(64), &events, now);
	let unknown = sign("nobody", &secret, &events, now);
	let (s1, b1) = send(app.clone(), &forged, &ALL).await;
	let (s2, b2) = send(app.clone(), &unknown, &ALL).await;
	assert_eq!((s1, s2), (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED));
	assert_eq!(b1, b2, "an unknown key and a bad signature read the same");

	let stale = sign("aquafix-site", &secret, &events, now - jiff::SignedDuration::from_mins(10));
	let (status, body) = send(app.clone(), &stale, &ALL).await;
	assert_eq!((status, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("timestamp outside the replay window")));

	let empty = sign("aquafix-site", &secret, &[], now);
	let (status, body) = send(app.clone(), &empty, &ALL).await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

	let huge = Signed {
		body: vec![b' '; panel_server::http::MAX_BODY + 1],
		..sign("aquafix-site", &secret, &events, now)
	};
	let (status, _) = send(app.clone(), &huge, &ALL).await;
	assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

	let res = app.oneshot(Request::get("/health").body(Body::empty()).unwrap()).await.unwrap();
	assert_eq!(res.status(), StatusCode::OK);
}

/// A body that never arrives.
fn stalled() -> Body {
	Body::from_stream(futures::stream::pending::<Result<Vec<u8>, std::io::Error>>())
}

fn request(key_id: &str, timestamp: &str, body: Body) -> Request<Body> {
	Request::post("/api/ingest/v1/events")
		.header("x-sa-key-id", key_id)
		.header("x-sa-timestamp", timestamp)
		.header("x-sa-signature", "00")
		.body(body)
		.unwrap()
}

#[tokio::test]
async fn costly_requests_are_refused_before_the_body_is_read() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let now = Timestamp::now().as_second().to_string();
	let stale = (Timestamp::now().as_second() - 600).to_string();
	let quick = std::time::Duration::from_secs(5);

	// Both would hang on the stalled body if it were read first.
	let app = panel_server::http::router(panel.clone());
	for (key_id, ts) in [("aquafix-site", stale.as_str()), ("Not a key id", now.as_str())] {
		let res = tokio::time::timeout(quick, app.clone().oneshot(request(key_id, ts, stalled())))
			.await
			.expect("answered without the body")
			.unwrap();
		assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{key_id} {ts}");
	}

	let limits = panel_server::http::Limits {
		max_concurrent: 1,
		body_timeout: std::time::Duration::from_millis(200),
		request_timeout: std::time::Duration::from_secs(5),
		..Default::default()
	};
	let app = panel_server::http::router_with(panel, limits);
	let res = tokio::time::timeout(quick, app.clone().oneshot(request("aquafix-site", &now, stalled()))).await.unwrap().unwrap();
	assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT, "a body too slow to arrive");

	// One slow request holds the only slot; the next is shed, not queued.
	let (tx, rx) = tokio::sync::oneshot::channel::<()>();
	let held = futures::stream::once(async move {
		let _ = rx.await;
		Ok::<_, std::io::Error>(b"{}".to_vec())
	});
	let first = tokio::spawn(app.clone().oneshot(request("aquafix-site", &now, Body::from_stream(held))));
	tokio::time::sleep(std::time::Duration::from_millis(50)).await;
	let res = app.clone().oneshot(request("aquafix-site", &now, Body::from("{}"))).await.unwrap();
	assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
	tx.send(()).unwrap();
	assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::UNAUTHORIZED, "the held one goes on to be judged");
}

async fn look_up(app: &axum::Router, uri: &str, signed: &Signed) -> (StatusCode, Value) {
	let req = Request::get(uri)
		.header("x-sa-key-id", &signed.key_id)
		.header("x-sa-timestamp", &signed.timestamp)
		.header("x-sa-signature", &signed.signature)
		.body(Body::empty())
		.unwrap();
	let res = app.clone().oneshot(req).await.unwrap();
	let status = res.status();
	let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
	(status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// A bot looks a lead up by the ref the customer brought: signed as ingest is, over `GET <path>`
/// in place of a body; only its own brand's, only a bot's key; what the customer asked for, never who they are.
#[tokio::test]
async fn a_bot_looks_a_lead_up_by_its_ref() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let aquafix = || [BrandId::parse("aquafix").unwrap()].into();
	let site = panel.add_source("aquafix-site", SourceKind::Site, aquafix()).await.unwrap().unwrap().secret.to_string();
	let bot = panel.add_source("aquafix-tg", SourceKind::Bot, aquafix()).await.unwrap().unwrap().secret.to_string();
	let now = Timestamp::now();
	let earlier = now - jiff::SignedDuration::from_mins(5);
	let mut priced = messenger_lead(now, "aquafix", "L-2", "telegram", "AQ-7K3F");
	priced["properties"]["flow"] = json!("fixed");
	priced["properties"]["quotedCents"] = json!(9900);
	priced["properties"]["pricingValidFrom"] = json!("2026-10-01");
	let leads = [messenger_lead(earlier, "aquafix", "L-1", "whatsapp", "AQ-7K3F"), priced];
	let got = panel.ingest(sign("aquafix-site", &site, &leads, now).batch(), now).await.unwrap();
	assert!(got.iter().all(|v| v.outcome == panel::Outcome::Accepted { unregistered: false }), "{got:?}");
	let app = panel_server::http::router(panel);
	let uri = "/api/ingest/v1/leads/by-ref/aquafix/AQ-7K3F";

	let (status, body) = look_up(&app, uri, &sign_get("aquafix-tg", &bot, uri, now)).await;
	assert_eq!(status, StatusCode::OK, "{body}");
	let created = body["created_at"].as_str().unwrap().to_owned();
	assert_eq!(
		body,
		json!({
			"lead_id": "L-2", "channel": "telegram", "stage": "created", "created_at": created, "message_ref": "AQ-7K3F",
			"need": "fuite sous l'évier", "locality": "Royat", "quoted_cents": 9900, "flow": "fixed",
		}),
		"the newest lead with the ref; no phone, no name"
	);
	let dump = body.to_string();
	assert!(!dump.contains("+336") && !dump.contains("Dupont"), "{dump}");

	let (status, body) = look_up(
		&app,
		"/api/ingest/v1/leads/by-ref/aquafix/AQ-0000",
		&sign_get("aquafix-tg", &bot, "/api/ingest/v1/leads/by-ref/aquafix/AQ-0000", now),
	)
	.await;
	assert_eq!((status, body["error"].as_str()), (StatusCode::NOT_FOUND, Some("no lead of the brand carries that ref")));
	let (status, _) = look_up(
		&app,
		"/api/ingest/v1/leads/by-ref/aquafix/aq-7k3f",
		&sign_get("aquafix-tg", &bot, "/api/ingest/v1/leads/by-ref/aquafix/aq-7k3f", now),
	)
	.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "a ref is as the landing made it");
	let (status, body) = look_up(
		&app,
		"/api/ingest/v1/leads/by-ref/vifnet/AQ-7K3F",
		&sign_get("aquafix-tg", &bot, "/api/ingest/v1/leads/by-ref/vifnet/AQ-7K3F", now),
	)
	.await;
	assert_eq!((status, body["error"].as_str()), (StatusCode::FORBIDDEN, Some("this key is not for that brand")));
	let (status, body) = look_up(&app, uri, &sign_get("aquafix-site", &site, uri, now)).await;
	assert_eq!((status, body["error"].as_str()), (StatusCode::FORBIDDEN, Some("only a bot's key looks a lead up by its ref")));
	let (status, body) = look_up(&app, uri, &sign_get("aquafix-tg", &"0".repeat(64), uri, now)).await;
	assert_eq!((status, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalid key or signature")));
	let (status, _) = look_up(&app, uri, &sign_get("aquafix-tg", &bot, uri, now - jiff::SignedDuration::from_mins(10))).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED, "the replay window holds");
	let other = "/api/ingest/v1/leads/by-ref/aquafix/AQ-0000";
	let (status, _) = look_up(&app, other, &sign_get("aquafix-tg", &bot, uri, now)).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED, "a signature opens the one path it was made for");
	let ts = now.as_second().to_string();
	let empty = Signed {
		key_id: "aquafix-tg".into(),
		signature: panel_core::signature::sign(bot.as_bytes(), &ts, b""),
		timestamp: ts,
		body: Vec::new(),
	};
	let (status, _) = look_up(&app, uri, &empty).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED, "nor does a MAC over an empty body");
	let with_query = format!("{uri}?locale=fr");
	let (status, _) = look_up(&app, &with_query, &sign_get("aquafix-tg", &bot, &with_query, now)).await;
	assert_eq!(status, StatusCode::OK, "the query is signed with the path");
	let (status, _) = look_up(&app, &with_query, &sign_get("aquafix-tg", &bot, uri, now)).await;
	assert_eq!(status, StatusCode::UNAUTHORIZED, "and cannot be added after");
	let unsigned = Request::get(uri).body(Body::empty()).unwrap();
	assert_eq!(app.clone().oneshot(unsigned).await.unwrap().status(), StatusCode::UNAUTHORIZED);

	// What it said, once the customer wrote.
	let wrote = event(
		"lead.messaged",
		now,
		"bot",
		json!({"brandId": "aquafix"}),
		json!({"channel": "telegram", "messageRef": "AQ-7K3F"}),
	);
	let batch = sign("aquafix-tg", &bot, &[wrote], now);
	let (status, body) = send(app.clone(), &batch, &ALL).await;
	assert_eq!((status, body["results"][0]["status"].as_str()), (StatusCode::MULTI_STATUS, Some("accepted")), "{body}");
	let (_, body) = look_up(&app, uri, &sign_get("aquafix-tg", &bot, uri, now)).await;
	assert_eq!(body["messaged_channel"], "telegram");
	assert!(body["messaged_at"].is_string());
}

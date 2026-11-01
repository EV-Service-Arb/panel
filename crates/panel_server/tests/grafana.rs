//! Grafana (docs/ARCHITECTURE.md, Grafana): what reaches it from a viewer's browser, what its
//! fetches reach the services as, and the dashboards it is given.

use std::{collections::BTreeSet, path::Path, time::Duration};

use axum::{
	Router,
	body::Body,
	extract::Request,
	http::{HeaderMap, Method, StatusCode, header},
	response::IntoResponse,
	routing::get,
};
use panel::testing::{TestDb, admin, panel};
use panel_server::{
	grafana::VIEWER_HEADER,
	testing::{Browser, Log, app_with_grafana, upstream},
};
use sa_auth::{Principal, Service};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A request as it reached the fake Grafana, held there until the test answers it.
struct AtGrafana {
	method: Method,
	uri: String,
	headers: HeaderMap,
	body: Value,
	answer: tokio::sync::oneshot::Sender<()>,
}

/// A fake Grafana on `<dir>/grafana.sock`: every request it gets is sent on, and answered
/// `answered`, with a cookie of its own, once the test lets it.
fn fake_grafana(dir: &Path) -> tokio::sync::mpsc::UnboundedReceiver<AtGrafana> {
	let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
	let grafana = Router::new().fallback(move |req: Request| {
		let tx = tx.clone();
		async move {
			let (parts, body) = req.into_parts();
			let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
			let (answer, answered) = tokio::sync::oneshot::channel();
			tx.send(AtGrafana {
				method: parts.method,
				uri: parts.uri.to_string(),
				headers: parts.headers,
				body: if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() },
				answer,
			})
			.unwrap();
			answered.await.unwrap();
			([(header::SET_COOKIE, "grafana_session=1")], "answered")
		}
	});
	let listener = tokio::net::UnixListener::bind(dir.join("grafana.sock")).unwrap();
	tokio::spawn(async move { axum::serve(listener, grafana).await.unwrap() });
	rx
}

async fn setup(db: &TestDb, dir: &Path, routes: Router) -> (Router, Router, panel_server::grafana::Grafana, Log) {
	let (base, seen) = upstream(routes).await;
	let (app, loopback, grafana) = app_with_grafana(panel(db).await, admin(), "dev-admin@localhost", &base, dir);
	(app, loopback, grafana, seen)
}

#[tokio::test]
async fn grafana_is_reached_stripped_of_its_prefix_and_of_everyones_credentials() {
	let db = TestDb::create().await;
	let dir = tempfile::tempdir().unwrap();
	let mut at_grafana = fake_grafana(dir.path());
	let (app, _, _, _) = setup(&db, dir.path(), Router::new()).await;
	let mut b = Browser::default();

	assert_eq!(b.send(&app, Method::GET, "/grafana/", &[], false).await.0, StatusCode::UNAUTHORIZED, "no session");
	b.sign_in(&app).await;

	for (method, ours, theirs) in [
		(Method::GET, "/grafana/", "/"),
		(Method::GET, "/grafana/d/ra-overview/x?orgId=1&kiosk", "/d/ra-overview/x?orgId=1&kiosk"),
		(Method::POST, "/grafana/api/frontend-metrics", "/api/frontend-metrics"),
	] {
		let sent = {
			let (app, ours) = (app.clone(), ours.to_owned());
			let mut b = Browser { jar: b.jar.clone() };
			let extra = [(sa_auth::HEADER, "forged"), (VIEWER_HEADER, "forged"), ("x-sa-csrf", "x"), ("accept", "text/html")];
			tokio::spawn(async move { b.send(&app, method, &ours, &extra, false).await })
		};
		let held = at_grafana.recv().await.unwrap();
		assert_eq!(held.uri, theirs);
		for h in ["cookie", sa_auth::HEADER, VIEWER_HEADER, "x-sa-csrf"] {
			assert!(!held.headers.contains_key(h), "{ours}: {h} never reaches Grafana");
		}
		assert_eq!(held.headers["accept"], "text/html", "{ours}: the browser's own headers pass");
		held.answer.send(()).unwrap();
		let (status, headers) = sent.await.unwrap();
		assert_eq!(status, StatusCode::OK, "{ours}: a POST is a read, no CSRF");
		assert!(!headers.contains_key(header::SET_COOKIE), "{ours}: Grafana sets no cookie on the conductor's origin");
		assert!(
			!headers.contains_key(header::CONTENT_SECURITY_POLICY),
			"{ours}: the pages' frame-ancestors 'none' would break the frame"
		);
	}
}

/// Each value `header` has in a query's Infinity headers.
fn query_headers(query: &Value, header: &str) -> Vec<String> {
	query["url_options"]["headers"]
		.as_array()
		.unwrap()
		.iter()
		.filter(|h| h["key"].as_str().unwrap().eq_ignore_ascii_case(header))
		.map(|h| h["value"].as_str().unwrap().to_owned())
		.collect()
}

/// Grafana holds no identity: its fetches reach a service as the viewer whose browser asked,
/// through a token written into each query on a service's datasource, good for GETs on the
/// loopback listener while that query is open. Nothing else carries anyone.
#[tokio::test]
async fn grafanas_queries_reach_the_services_as_the_viewer_who_asked() {
	let db = TestDb::create().await;
	let dir = tempfile::tempdir().unwrap();
	let mut at_grafana = fake_grafana(dir.path());
	let (app, loopback, _, seen) = setup(&db, dir.path(), Router::new()).await;
	let mut b = Browser::default();
	b.sign_in(&app).await;

	let infinity = json!({
		"datasource": {"type": "yesoreyeram-infinity-datasource", "uid": "review_archive"},
		"url": "/me/overview",
		"url_options": {"method": "GET", "headers": [{"key": "x-member", "value": "3"}, {"key": VIEWER_HEADER.to_uppercase(), "value": "forged"}]},
	});
	let bare = json!({"datasource": {"type": "yesoreyeram-infinity-datasource", "uid": "sites"}, "url": "/leads"});
	let elsewhere = json!({"datasource": {"uid": "-- Dashboard --"}, "panelId": 1});
	let asked = {
		let (app, mut b) = (app.clone(), Browser { jar: b.jar.clone() });
		let body = json!({"queries": [infinity, bare, elsewhere], "from": "now-6h", "to": "now"}).to_string();
		tokio::spawn(async move {
			b.send_body(&app, Method::POST, "/grafana/api/ds/query?ds_type=x", &[("content-type", "application/json")], false, body.into())
				.await
		})
	};
	let held = at_grafana.recv().await.unwrap();
	assert_eq!((held.method, held.uri.as_str()), (Method::POST, "/api/ds/query?ds_type=x"));
	let queries = held.body["queries"].as_array().unwrap();
	let token = query_headers(&queries[0], VIEWER_HEADER).remove(0);
	assert_ne!(token, "forged");
	for q in &queries[..2] {
		assert_eq!(
			query_headers(q, VIEWER_HEADER),
			std::slice::from_ref(&token),
			"one token in every service's query, the browser's overwritten"
		);
	}
	assert_eq!(query_headers(&queries[0], "x-member"), ["3"], "the dashboard's own headers stay");
	assert_eq!(queries[2], elsewhere, "what does not query a service is not touched");

	let fetch = |method: Method, path: &str, token: Option<&str>| {
		let mut req = Request::builder().method(method).uri(path);
		if let Some(t) = token {
			req = req.header(VIEWER_HEADER, t);
		}
		loopback.clone().oneshot(req.body(Body::empty()).unwrap())
	};
	let res = fetch(Method::GET, "/api/review_archive/me/overview?x=1", Some(&token)).await.unwrap();
	assert_eq!(res.status(), StatusCode::OK);
	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!(got.path_and_query, "/me/overview?x=1");
	assert!(!got.headers.contains_key(VIEWER_HEADER), "the token is the conductor's");
	let Principal::User(user) = got.assertion(Service::ReviewArchive, None).principal else {
		panic!("a viewer is a user")
	};
	assert_eq!(user.email, "dev-admin@localhost");
	assert!(user.permissions.iter().all(|p| p.starts_with("sa:review_archive:")), "{:?}", user.permissions);

	fetch(Method::GET, "/api/sites/leads", Some(&token)).await.unwrap();
	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!(got.path_and_query, "/api/v1/leads", "the forward's own route");
	assert!(matches!(got.assertion(Service::Sites, None).principal, Principal::User(_)));

	for (method, path, token, why) in [
		(Method::POST, "/api/review_archive/me/tokens/claim", Some(token.as_str()), "a viewer's token reads only"),
		(Method::GET, "/api/review_archive/me", None, "no token, no one"),
		(Method::GET, "/api/review_archive/me", Some("forged"), "a token the conductor did not mint"),
	] {
		assert_eq!(fetch(method, path, token).await.unwrap().status(), StatusCode::UNAUTHORIZED, "{why}");
	}
	assert_eq!(
		fetch(Method::GET, "/api/hooks/booking/x", Some(&token)).await.unwrap().status(),
		StatusCode::NOT_FOUND,
		"a service's API only"
	);
	assert_eq!(
		Browser::default().send(&app, Method::GET, "/api/review_archive/me", &[(VIEWER_HEADER, &token)], false).await.0,
		StatusCode::UNAUTHORIZED,
		"the public listener takes no token"
	);
	assert!(seen.lock().unwrap().is_empty());

	held.answer.send(()).unwrap();
	let (status, _, body) = asked.await.unwrap();
	assert_eq!((status, &body[..]), (StatusCode::OK, &b"answered"[..]));
	assert_eq!(
		fetch(Method::GET, "/api/review_archive/me", Some(&token)).await.unwrap().status(),
		StatusCode::UNAUTHORIZED,
		"dead once Grafana answered"
	);
}

/// A server that only says it was started, with what: Grafana as `run` sees it.
fn fake_server(dir: &Path) -> std::path::PathBuf {
	use std::os::unix::fs::PermissionsExt;
	let path = dir.join("grafana-server");
	std::fs::write(
		&path,
		"#!/bin/sh\nenv > \"$GF_PATHS_DATA/env.part\" && mv \"$GF_PATHS_DATA/env.part\" \"$GF_PATHS_DATA/env\"\nexec sleep 600\n",
	)
	.unwrap();
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
	path
}

/// `run` on `dir` until the server started, the dashboards pulled before it; `during` with it running.
async fn ran<F: Future<Output = ()>>(grafana: panel_server::grafana::Grafana, dir: &Path, during: impl FnOnce() -> F) {
	let server = fake_server(&dir.join("bin"));
	let (stop, stopped) = tokio::sync::watch::channel(false);
	let test = async {
		let env = dir.join("grafana/env");
		tokio::time::timeout(Duration::from_secs(10), async {
			while !env.exists() {
				tokio::time::sleep(Duration::from_millis(20)).await;
			}
		})
		.await
		.expect("the server started");
		during().await;
		stop.send(true).unwrap();
	};
	let ((), ()) = tokio::join!(grafana.run(&server, "https://sa.test/grafana/", stopped), test);
}

/// The files under `<dir>/grafana/dashboards` after one run with `answer` at `GET /dashboards`
/// and `before` there already.
async fn pulled(answer: Router, before: &[&str]) -> BTreeSet<String> {
	let db = TestDb::create().await;
	let dir = tempfile::tempdir().unwrap();
	std::fs::create_dir_all(dir.path().join("bin")).unwrap();
	let dashboards = dir.path().join("grafana/dashboards");
	for file in before {
		let path = dashboards.join(file);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, "{}").unwrap();
	}
	let (_, _, grafana, seen) = setup(&db, &dir.path().join("grafana"), answer).await;
	ran(grafana, dir.path(), || async {}).await;
	let asked = seen.lock().unwrap().iter().find(|s| s.path_and_query == "/dashboards").cloned().expect("asked");
	assert_eq!(asked.assertion(Service::ReviewArchive, None).principal, Principal::Conductor);
	let mut files = BTreeSet::new();
	for service in std::fs::read_dir(&dashboards).unwrap() {
		let service = service.unwrap();
		for file in std::fs::read_dir(service.path()).unwrap() {
			files.insert(format!("{}/{}", service.file_name().to_str().unwrap(), file.unwrap().file_name().to_str().unwrap()));
		}
	}
	files
}

fn serving(dashboards: Value) -> Router {
	Router::new().route(
		"/dashboards",
		get(move |headers: HeaderMap| {
			let dashboards = dashboards.clone();
			async move {
				match headers.get(header::IF_NONE_MATCH) {
					Some(_) => StatusCode::NOT_MODIFIED.into_response(),
					None => ([(header::ETAG, "\"1\"")], axum::Json(dashboards)).into_response(),
				}
			}
		}),
	)
}

#[tokio::test]
async fn a_services_dashboards_land_in_its_folder_and_replace_what_it_served_before() {
	let two = json!([{"uid": "ra-overview", "title": "Overview"}, {"uid": "ra-location", "title": "Location"}]);
	let set = |files: &[&str]| files.iter().map(|f| f.to_string()).collect::<BTreeSet<_>>();
	assert_eq!(
		pulled(serving(two.clone()), &["review_archive/old.json"]).await,
		set(&["review_archive/ra-location.json", "review_archive/ra-overview.json"])
	);
	for (answer, before, why) in [
		(
			Router::new().route("/dashboards", get(|| async { StatusCode::NOT_FOUND })),
			&["review_archive/old.json"][..],
			"a missing endpoint is not \"no dashboards\"",
		),
		(serving(two.clone()), &["review_archive/old.json", "sites/ra-location.json"][..], "a uid another service serves"),
		(
			serving(json!([{"uid": "ra-overview"}, {"uid": "ra-overview"}])),
			&["review_archive/old.json"][..],
			"a uid served twice",
		),
		(serving(json!([{"uid": "../ra-overview"}])), &["review_archive/old.json"][..], "a uid Grafana refuses"),
		(serving(json!([{"title": "no uid"}])), &["review_archive/old.json"][..], "no uid"),
	] {
		assert_eq!(pulled(answer, before).await, set(before), "{why}: what was there stays");
	}
}

#[tokio::test]
async fn grafana_is_provisioned_with_a_datasource_per_services_api_on_its_loopback_listener() {
	let db = TestDb::create().await;
	let dir = tempfile::tempdir().unwrap();
	std::fs::create_dir_all(dir.path().join("bin")).unwrap();
	let (_, _, grafana, _) = setup(&db, &dir.path().join("grafana"), serving(json!([]))).await;
	ran(grafana, dir.path(), || async {
		let env = std::fs::read_to_string(dir.path().join("grafana/env")).unwrap();
		assert!(env.lines().any(|l| l == "GF_SERVER_ROOT_URL=https://sa.test/grafana/"), "{env}");
		let provisioned: Value = serde_json::from_slice(&std::fs::read(dir.path().join("grafana/provisioning/datasources/services.yaml")).unwrap()).unwrap();
		let datasources = provisioned["datasources"].as_array().unwrap();
		assert_eq!(datasources.iter().map(|d| d["uid"].as_str().unwrap()).collect::<Vec<_>>(), ["sites", "review_archive"]);
		for d in datasources {
			let url = d["url"].as_str().unwrap();
			assert_eq!(url, format!("{}/api/{}", url.split("/api/").next().unwrap(), d["uid"].as_str().unwrap()));
			assert_eq!(d["jsonData"]["allowedHosts"], json!([format!("{url}/")]), "no other service's path");
			// Infinity joins the url and the dashboards' `/me/overview` as strings
			let res = reqwest::get(format!("{url}/me/overview")).await.unwrap();
			assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "the loopback listener answers, and wants a token");
		}
	})
	.await;
}

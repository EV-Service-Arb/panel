//! A brand's experiments over HTTP, end to end on the real router: a landing's declaration
//! ingested through a site key, the screens' list, an admin's change (operators refused), the
//! sites' read under `/api/internal`, a retired experiment, and the rebuild landing on the same.

use std::collections::HashMap;

use axum::{
	Router,
	body::{Body, to_bytes},
	http::{Method, Request, StatusCode, header},
};
use jiff::{SignedDuration, Timestamp};
use panel::{
	Panel,
	posthog::PosthogProject,
	testing::{TestDb, event, panel, sign},
};
use panel_core::ids::BrandId;
use panel_server::{
	concierge::{Concierge, DevIdentity},
	http,
	signin::{SignIn, SignInConfig},
};
use sa_auth::SourceKind;
use serde_json::{Value, json};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:59120";
const LIVE: &str = "/api/internal/brands/aquafix/experiments";

fn app(panel: Panel, alias: &str) -> Router {
	let who = DevIdentity {
		permissions: sa_auth::Catalog::collect("sa", 0).aliases[alias].iter().cloned().collect(),
		email: format!("dev-{}@localhost", alias.trim_start_matches("sa:")),
	};
	let config = SignInConfig {
		panel_origin: ORIGIN.to_owned(),
		concierge_origin: ORIGIN.to_owned(),
	};
	http::app(SignIn::new(panel, Concierge::dev(who), config))
}

#[derive(Default)]
struct Browser {
	jar: HashMap<String, String>,
}

impl Browser {
	async fn send(&mut self, app: &Router, method: Method, uri: &str, body: Option<Value>, csrf: bool) -> (StatusCode, Value) {
		let mut req = Request::builder().method(method).uri(uri);
		if !self.jar.is_empty() {
			req = req.header(header::COOKIE, self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "));
		}
		if csrf && let Some(t) = self.jar.get("sa_csrf") {
			req = req.header("x-sa-csrf", t);
		}
		let req = match body {
			Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
			None => req.body(Body::empty()),
		};
		let res = app.clone().oneshot(req.unwrap()).await.unwrap();
		for c in res.headers().get_all(header::SET_COOKIE) {
			let c = c.to_str().unwrap();
			let (pair, attrs) = c.split_once(';').unwrap_or((c, ""));
			let (k, v) = pair.split_once('=').unwrap();
			if attrs.contains("Max-Age=0") {
				self.jar.remove(k);
			} else {
				self.jar.insert(k.to_owned(), v.to_owned());
			}
		}
		let status = res.status();
		let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
		(status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
	}

	async fn get(&mut self, app: &Router, uri: &str) -> (StatusCode, Value) {
		self.send(app, Method::GET, uri, None, false).await
	}

	async fn write(&mut self, app: &Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
		self.send(app, method, uri, Some(body), true).await
	}

	async fn signed_in(app: &Router) -> Self {
		let mut b = Self::default();
		let login = app.clone().oneshot(Request::get("/auth/login").body(Body::empty()).unwrap()).await.unwrap();
		let location = login.headers()[header::LOCATION].to_str().unwrap().to_owned();
		for c in login.headers().get_all(header::SET_COOKIE) {
			let (pair, _) = c.to_str().unwrap().split_once(';').unwrap();
			let (k, v) = pair.split_once('=').unwrap();
			b.jar.insert(k.to_owned(), v.to_owned());
		}
		let (status, _) = b.get(app, location.strip_prefix(ORIGIN).unwrap()).await;
		assert_eq!(status, StatusCode::SEE_OTHER);
		assert!(b.jar.contains_key("sa_session"));
		b
	}
}

/// The landing of aquafix declaring `experiments` at `at`.
async fn declare(panel: &Panel, secret: &str, experiments: Value, at: Timestamp) {
	let e = event("experiments.declared", at, "site", json!({"brandId": "aquafix"}), json!({ "experiments": experiments }));
	let got = panel.ingest(sign("aquafix-site", secret, &[e], at).batch(), at).await.unwrap();
	assert_eq!(got[0].outcome, panel::Outcome::Accepted { unregistered: false }, "{got:?}");
}

async fn site_key(panel: &Panel) -> String {
	let brands = [BrandId::parse("aquafix").unwrap()].into();
	panel.add_source("aquafix-site", SourceKind::Site, brands).await.unwrap().unwrap().secret.to_string()
}

#[tokio::test]
async fn a_declaration_an_override_and_what_the_site_is_told() {
	let db = TestDb::create().await;
	let project = PosthogProject {
		app_host: "https://us.posthog.com".into(),
		project_id: "614067".into(),
	};
	let panel = panel(&db).await.with_posthog_project(Some(project));
	let secret = site_key(&panel).await;
	let admin = app(panel.clone(), "sa:admin");
	let mut b = Browser::signed_in(&admin).await;

	assert_eq!(b.get(&admin, LIVE).await, (StatusCode::OK, json!({"experiments": {}})), "nothing declared yet");
	assert_eq!(b.get(&admin, "/api/internal/brands/Not-A-Brand/experiments").await, (StatusCode::OK, json!({"experiments": {}})));

	let t0 = Timestamp::now() - SignedDuration::from_mins(10);
	declare(
		&panel,
		&secret,
		json!([
			{"key": "lead_layout", "variants": ["a", "b"], "weights": [1, 1], "enabled": true, "summary": "One step converts better"},
			{"key": "hero", "variants": ["a", "b", "c"], "weights": [1, 1, 1], "enabled": true, "holdout": 0.1}
		]),
		t0,
	)
	.await;
	let (status, list) = b.get(&admin, "/api/v1/experiments?brand=aquafix").await;
	assert_eq!(status, StatusCode::OK, "{list}");
	let item = |list: &Value, key: &str| list["experiments"].as_array().unwrap().iter().find(|e| e["key"] == key).unwrap().clone();
	let layout = item(&list, "lead_layout");
	assert_eq!(layout["declared"]["summary"], "One step converts better");
	assert_eq!(layout["override"], Value::Null);
	assert_eq!(layout["effective"], json!({"weights": [1.0, 1.0], "enabled": true, "holdout": null}));
	assert_eq!((layout["retired"].clone(), layout["weights_changed_at"].clone()), (json!(false), Value::Null));
	let url = layout["posthog_url"].as_str().unwrap();
	assert!(
		url.starts_with("https://us.posthog.com/project/614067/insights/new#q=%7B%22kind%22%3A%22InsightVizNode%22"),
		"{url}"
	);
	assert_eq!(
		b.get(&admin, "/api/v1/experiments").await.1["experiments"].as_array().unwrap().len(),
		2,
		"every brand without ?brand"
	);

	// The funnel opens PostHog's, from the visit, on the same brand and days.
	let funnel = b.get(&admin, "/api/v1/funnel?brand=aquafix&from=2026-09-01&to=2026-09-30").await.1;
	let url = funnel["posthog_url"].as_str().unwrap();
	assert!(url.starts_with("https://us.posthog.com/project/614067/insights/new#q="), "{url}");
	for needle in ["location_page_view", "sa_payment_received", "%22aquafix%22", "2026-09-01", "2026-09-30"] {
		assert!(url.contains(needle), "{needle} in {url}");
	}
	let every = b.get(&admin, "/api/v1/funnel").await.1;
	assert!(every["posthog_url"].as_str().unwrap().contains("breakdownFilter"), "every brand's, side by side");

	// An admin's change: weights and the kill switch; null puts a field back, 0 is a holdout.
	let path = "/api/v1/experiments/aquafix/hero";
	let (status, hero) = b.write(&admin, Method::PUT, path, json!({"weights": [2, 1, 1], "holdout": 0})).await;
	assert_eq!(status, StatusCode::OK, "{hero}");
	assert_eq!(hero["effective"], json!({"weights": [2.0, 1.0, 1.0], "enabled": true, "holdout": 0.0}));
	assert_eq!(hero["override"]["changed_by"], "dev-admin@localhost", "named as a place's or a price list's history names them");
	let source: String = sqlx::query_scalar("SELECT source_id FROM events WHERE type = 'experiment.configured'")
		.fetch_one(panel.store().pool())
		.await
		.unwrap();
	let admin_id = DevIdentity {
		permissions: panel::testing::admin(),
		email: "dev-admin@localhost".to_owned(),
	}
	.user_id();
	assert_eq!(source, admin_id.to_string(), "the source is the admin's id");
	assert!(hero["weights_changed_at"].is_string());
	let (_, hero) = b.write(&admin, Method::PUT, path, json!({"holdout": null, "enabled": false})).await;
	assert_eq!(hero["effective"], json!({"weights": [2.0, 1.0, 1.0], "enabled": false, "holdout": 0.1}));
	assert_eq!(b.get(&admin, LIVE).await.1, json!({"experiments": {"hero": {"enabled": false, "weights": [2.0, 1.0, 1.0]}}}));

	for (body, status) in [
		(json!({"weights": [1, 1]}), StatusCode::BAD_REQUEST),
		(json!({"weights": [0, 0, 0]}), StatusCode::BAD_REQUEST),
		(json!({"holdout": 1}), StatusCode::BAD_REQUEST),
		(json!({"variants": ["a"]}), StatusCode::BAD_REQUEST),
	] {
		let (got, err) = b.write(&admin, Method::PUT, path, body.clone()).await;
		assert_eq!(got, status, "{body}: {err}");
		assert!(err["error"].is_string(), "{err}");
	}
	assert_eq!(
		b.write(&admin, Method::PUT, "/api/v1/experiments/aquafix/nope", json!({"enabled": false})).await.0,
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		b.write(&admin, Method::PUT, "/api/v1/experiments/vifnet/hero", json!({"enabled": false})).await.0,
		StatusCode::NOT_FOUND
	);

	// The landing drops `hero`: retired, shown, not changed, and nothing for the site.
	declare(
		&panel,
		&secret,
		json!([{"key": "lead_layout", "variants": ["a", "b"], "weights": [1, 1], "enabled": true}]),
		t0 + SignedDuration::from_mins(1),
	)
	.await;
	let list = b.get(&admin, "/api/v1/experiments?brand=aquafix").await.1;
	assert_eq!(item(&list, "hero")["retired"], true);
	assert_eq!(b.write(&admin, Method::PUT, path, json!({"enabled": true})).await.0, StatusCode::NOT_FOUND);
	assert_eq!(b.get(&admin, LIVE).await.1, json!({"experiments": {}}));

	let before = b.get(&admin, "/api/v1/experiments").await.1;
	panel.rebuild_projections().await.unwrap();
	assert_eq!(b.get(&admin, "/api/v1/experiments").await.1, before, "the rebuild lands on the same");

	// An operator reads, and changes nothing.
	let operator = app(panel.clone(), "sa:operator");
	let mut o = Browser::signed_in(&operator).await;
	assert_eq!(o.get(&operator, "/api/v1/experiments").await.0, StatusCode::OK);
	let refused = o.write(&operator, Method::PUT, "/api/v1/experiments/aquafix/lead_layout", json!({"enabled": false})).await;
	assert_eq!(refused.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_override_that_no_longer_fits_is_not_sent() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = site_key(&panel).await;
	let admin = app(panel.clone(), "sa:admin");
	let mut b = Browser::signed_in(&admin).await;
	let t0 = Timestamp::now() - SignedDuration::from_mins(10);
	declare(&panel, &secret, json!([{"key": "hero", "variants": ["a", "b"], "weights": [1, 1], "enabled": true}]), t0).await;
	let (status, _) = b.write(&admin, Method::PUT, "/api/v1/experiments/aquafix/hero", json!({"weights": [3, 1]})).await;
	assert_eq!(status, StatusCode::OK);
	declare(
		&panel,
		&secret,
		json!([{"key": "hero", "variants": ["a", "b", "c"], "weights": [1, 1, 1], "enabled": true}]),
		t0 + SignedDuration::from_mins(1),
	)
	.await;
	assert_eq!(
		b.get(&admin, LIVE).await.1,
		json!({"experiments": {}}),
		"two weights for three variants: the site keeps its code's"
	);
	let hero = &b.get(&admin, "/api/v1/experiments").await.1["experiments"][0];
	assert_eq!(hero["override"]["weights"], json!([3.0, 1.0]), "kept as set");
	assert_eq!(hero["effective"]["weights"], json!([1.0, 1.0, 1.0]));
	assert_eq!(hero["posthog_url"], Value::Null, "no project configured");
	assert_eq!(b.get(&admin, "/api/v1/funnel").await.1["posthog_url"], Value::Null);
}

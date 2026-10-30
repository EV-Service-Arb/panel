//! `PANEL_DEV_SIGN_IN`: the binary refuses it at start in production and off loopback, and in
//! development the sign-in runs end to end without concierge — a real session, the gate, the
//! permissions.

use std::{collections::HashMap, process::Command};

use axum::{
	Router,
	body::{Body, to_bytes},
	http::{Method, Request, StatusCode, header},
};
use panel::testing::{TestDb, admin, operator, panel};
use panel_server::{
	concierge::{Concierge, DEV_CODE, DevIdentity},
	http,
	signin::{SignIn, SignInConfig},
};
use sa_auth::PermissionSet;
use serde_json::Value;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:59120";

// ── the guard, on the real binary ────────────────────────────────────────────────────────

/// `panel migrate` under exactly `vars`; its exit code and stderr. `migrate` is the cheapest
/// command, and the guard runs before any command does.
fn panel_migrate(vars: &[(&str, &str)]) -> (Option<i32>, String) {
	let out = Command::new(env!("CARGO_BIN_EXE_panel"))
		.arg("migrate")
		.env_clear()
		.envs(vars.iter().copied())
		.output()
		.expect("running the panel binary");
	(out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Everything production requires, so that the refusal is the dev sign-in's and not a missing
/// variable's.
fn production(db: &str) -> Vec<(&'static str, String)> {
	vec![
		("APP_ENV", "production".to_owned()),
		("PANEL_DB_PATH", db.to_owned()),
		("PANEL_DATA_KEY", "0".repeat(64)),
		("PANEL_PUBLIC_ORIGIN", ORIGIN.to_owned()),
		("CONCIERGE_PUBLIC_ORIGIN", "https://evinvest.ltd".to_owned()),
		("CONCIERGE_GRPC_ADDR", "http://concierge:55670".to_owned()),
		("RP_CLIENT_SECRET_SA", "s".repeat(40)),
		("PANEL_BUILD_EPOCH", "1791100000".to_owned()),
		("PANEL_ASSERTION_KEY", "k1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".to_owned()),
		("PANEL_REVIEW_ARCHIVE_URL", "http://review-archive:59110".to_owned()),
		("PANEL_PLAYBOOK_URL", "http://playbook-web.personal:59082".to_owned()),
	]
}

#[tokio::test]
async fn the_binary_refuses_dev_sign_in_in_production() {
	let db = TestDb::create().await;
	let path = db.path().display().to_string();
	let mut vars = production(&path);
	let (code, stderr) = panel_migrate(&vars.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>());
	assert_eq!(code, Some(0), "the production environment alone boots: {stderr}");

	vars.push(("PANEL_DEV_SIGN_IN", "sa:admin".to_owned()));
	let (code, stderr) = panel_migrate(&vars.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>());
	assert_eq!(code, Some(78), "EX_CONFIG: {stderr}");
	assert!(stderr.contains("PANEL_DEV_SIGN_IN must never be set outside development"), "{stderr}");
}

#[tokio::test]
async fn the_binary_refuses_dev_sign_in_off_loopback() {
	let db = TestDb::create().await;
	let path = db.path().display().to_string();
	let key = "0".repeat(64);
	let with = |origin: &str| {
		panel_migrate(&[
			("PANEL_DB_PATH", path.as_str()),
			("PANEL_DATA_KEY", key.as_str()),
			("PANEL_DEV_SIGN_IN", "sa:admin"),
			("PANEL_PUBLIC_ORIGIN", origin),
		])
	};
	for origin in ["https://sa.evinvest.ltd", "http://192.168.1.10:59120", "http://localhost.evil.example:59120"] {
		let (code, stderr) = with(origin);
		assert_eq!(code, Some(78), "{origin}: {stderr}");
		assert!(stderr.contains("PANEL_PUBLIC_ORIGIN must be http://localhost"), "{origin}: {stderr}");
	}
	let (code, stderr) = with(ORIGIN);
	assert_eq!(code, Some(0), "development on loopback boots: {stderr}");
}

/// An alias, a list of permissions or `none`; anything else is named at boot.
#[tokio::test]
async fn the_binary_takes_an_alias_or_a_list_of_permissions() {
	let db = TestDb::create().await;
	let path = db.path().display().to_string();
	let key = "0".repeat(64);
	let with = |who: &str| {
		panel_migrate(&[
			("PANEL_DB_PATH", path.as_str()),
			("PANEL_DATA_KEY", key.as_str()),
			("PANEL_DEV_SIGN_IN", who),
			("PANEL_PUBLIC_ORIGIN", ORIGIN),
		])
	};
	for who in ["sa:operator", "sa:work:read, sa:work:leads:edit", "none"] {
		let (code, stderr) = with(who);
		assert_eq!(code, Some(0), "{who}: {stderr}");
	}
	for (who, named) in [("operator", "`operator`"), ("sa:work:read,sa:work:everything", "`sa:work:everything`"), ("sa:*", "`sa:*`")] {
		let (code, stderr) = with(who);
		assert_eq!(code, Some(78), "{who}: {stderr}");
		assert!(stderr.contains(named), "{who}: {stderr}");
	}
}

// ── the sign-in, in process ──────────────────────────────────────────────────────────────

#[derive(Default)]
struct Browser {
	jar: HashMap<String, String>,
}

impl Browser {
	async fn send(&mut self, app: &Router, method: Method, uri: &str) -> (StatusCode, Option<String>, Value) {
		let mut req = Request::builder().method(method).uri(uri);
		let cookie: Vec<String> = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
		if !cookie.is_empty() {
			req = req.header(header::COOKIE, cookie.join("; "));
		}
		if let Some(t) = self.jar.get("sa_csrf") {
			req = req.header("x-sa-csrf", t);
		}
		let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
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
		let location = res.headers().get(header::LOCATION).map(|l| l.to_str().unwrap().to_owned());
		let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
		(status, location, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
	}

	/// `/auth/login`, then wherever it sends the browser, on this origin.
	async fn sign_in(&mut self, app: &Router) -> (StatusCode, String) {
		let (status, location, _) = self.send(app, Method::GET, "/auth/login").await;
		assert_eq!(status, StatusCode::FOUND);
		let location = location.unwrap();
		let path = location.strip_prefix(ORIGIN).unwrap_or_else(|| panic!("not back to the panel: {location}"));
		assert!(path.starts_with(&format!("/auth/callback?code={DEV_CODE}&state=")), "{path}");
		let (status, location, _) = self.send(app, Method::GET, path).await;
		(status, location.unwrap_or_default())
	}
}

async fn dev_app(db: &TestDb, permissions: PermissionSet, email: &str) -> Router {
	let who = DevIdentity {
		permissions,
		email: email.to_owned(),
	};
	let config = SignInConfig {
		panel_origin: ORIGIN.to_owned(),
		concierge_origin: ORIGIN.to_owned(),
	};
	http::app(SignIn::new(panel(db).await, Concierge::dev(who), config))
}

#[tokio::test]
async fn dev_sign_in_opens_a_real_session_with_its_permissions() {
	let db = TestDb::create().await;
	let app = dev_app(&db, admin(), "dev-admin@localhost").await;
	let mut b = Browser::default();

	assert_eq!(b.send(&app, Method::GET, "/api/v1/me").await.0, StatusCode::UNAUTHORIZED, "no session yet");
	let (status, location) = b.sign_in(&app).await;
	assert_eq!((status, location.as_str()), (StatusCode::SEE_OTHER, "/"));
	assert!(b.jar.contains_key("sa_session") && b.jar.contains_key("sa_csrf"), "{:?}", b.jar.keys());

	let (status, _, me) = b.send(&app, Method::GET, "/api/v1/me").await;
	assert_eq!(status, StatusCode::OK, "{me}");
	assert_eq!(serde_json::from_value::<PermissionSet>(me["permissions"].clone()).unwrap(), admin());
	assert_eq!(me["email"], "dev-admin@localhost");
	assert_eq!(me["preferred_name"], "Dev sign-in (dev-admin@localhost)", "the UI shows that this is dev sign-in");
	assert_eq!(me["dev_sign_in"], true);
	assert_eq!(b.send(&app, Method::GET, "/api/v1/sources").await.0, StatusCode::OK, "an admin's screen");

	assert_eq!(b.send(&app, Method::POST, "/auth/logout").await.0, StatusCode::NO_CONTENT);
	assert_eq!(b.send(&app, Method::GET, "/api/v1/me").await.0, StatusCode::UNAUTHORIZED, "signed out");
}

#[tokio::test]
async fn dev_sign_in_holds_what_it_was_given() {
	let db = TestDb::create().await;
	let app = dev_app(&db, operator(), "dev-operator@localhost").await;
	let mut b = Browser::default();
	b.sign_in(&app).await;
	let (status, _, me) = b.send(&app, Method::GET, "/api/v1/me").await;
	assert_eq!(status, StatusCode::OK, "{me}");
	assert_eq!(serde_json::from_value::<PermissionSet>(me["permissions"].clone()).unwrap(), operator());
	assert_eq!(b.send(&app, Method::GET, "/api/v1/sources").await.0, StatusCode::FORBIDDEN, "not an operator's screen");

	let db = TestDb::create().await;
	let app = dev_app(&db, ["sa:work:read"].into_iter().collect(), "dev-reader@localhost").await;
	let mut b = Browser::default();
	b.sign_in(&app).await;
	assert_eq!(b.send(&app, Method::GET, "/api/v1/leads").await.0, StatusCode::OK, "work");
	assert_eq!(b.send(&app, Method::GET, "/api/v1/experiments").await.0, StatusCode::FORBIDDEN, "no analysis");
}

#[tokio::test]
async fn dev_sign_in_still_checks_the_callback() {
	let db = TestDb::create().await;
	let app = dev_app(&db, admin(), "dev-admin@localhost").await;

	// A callback nobody started: no pre-login cookie, so no code is exchanged.
	let mut stranger = Browser::default();
	let (status, ..) = stranger.send(&app, Method::GET, &format!("/auth/callback?code={DEV_CODE}&state=x")).await;
	assert_eq!(status, StatusCode::BAD_REQUEST);
	assert!(!stranger.jar.contains_key("sa_session"));

	// The right state with another code: refused as concierge would refuse it.
	let mut b = Browser::default();
	let (_, location, _) = b.send(&app, Method::GET, "/auth/login").await;
	let path = location.unwrap().strip_prefix(ORIGIN).unwrap().replace(DEV_CODE, "forged");
	assert_eq!(b.send(&app, Method::GET, &path).await.0, StatusCode::BAD_REQUEST);
	assert!(!b.jar.contains_key("sa_session"));
}

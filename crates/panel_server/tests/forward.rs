//! The forward (docs/ARCHITECTURE.md, Forward): what reaches a service behind the conductor, and
//! what comes back from it.

use axum::{
	Router,
	http::{Method, StatusCode, header},
};
use panel::testing::{TestDb, admin, panel};
use panel_server::testing::{Browser, Log, SEED, upstream};
use sa_auth::{Keys, Principal, Service, Signer};

async fn setup(db: &TestDb) -> (Router, Log) {
	let (base, seen) = upstream(Router::new()).await;
	(panel_server::testing::app_with(panel(db).await, admin(), "dev-admin@localhost", &base, Default::default()), seen)
}

#[tokio::test]
async fn a_signed_in_call_reaches_the_service_with_an_assertion_and_without_the_browsers_cookies() {
	let db = TestDb::create().await;
	let (app, seen) = setup(&db).await;
	let mut b = Browser::default();

	assert_eq!(b.send(&app, Method::GET, "/api/review_archive/me", &[], false).await.0, StatusCode::UNAUTHORIZED, "no session");
	assert!(seen.lock().unwrap().is_empty(), "nothing reached the service");

	b.sign_in(&app).await;
	let (status, headers) = b
		.send(
			&app,
			Method::GET,
			"/api/review_archive/me/overview?x=1",
			&[("x-sa-assertion", "forged"), ("x-member", "3")],
			false,
		)
		.await;
	assert_eq!(status, StatusCode::OK);
	assert_eq!(headers["x-upstream"], "yes");
	assert!(headers.get(header::SET_COOKIE).is_none(), "a service sets no cookie on the panel's origin");
	assert!(!b.jar.contains_key("upstream"));

	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!(got.path_and_query, "/me/overview?x=1", "the prefix is the panel's");
	assert!(got.headers.get(header::COOKIE).is_none(), "the session never leaves the panel");
	assert_eq!(got.headers["x-member"], "3", "the service's own headers pass");
	let keys: Keys = SEED.parse::<Signer>().unwrap().public().parse().unwrap();
	let token = got.headers["x-sa-assertion"].to_str().unwrap();
	let now = jiff::Timestamp::now().as_second();
	let a = sa_auth::verify(&keys, token, Service::ReviewArchive, "GET", "/me/overview", None, now).expect("the panel's assertion, not the forged one");
	let Principal::User(a) = a.principal else { panic!("a session's call is a user's") };
	assert_eq!(a.email, "dev-admin@localhost");
	assert!(
		a.permissions.iter().all(|p| p.starts_with("sa:review_archive:")) && a.permissions.iter().count() == 3,
		"{:?}",
		a.permissions
	);
}

#[tokio::test]
async fn a_forwarded_write_needs_the_panels_csrf_header_but_playbooks_consent_form_does_not() {
	let db = TestDb::create().await;
	let (app, seen) = setup(&db).await;
	let mut b = Browser::default();
	b.sign_in(&app).await;

	assert_eq!(b.send(&app, Method::POST, "/api/review_archive/me/gmails", &[], false).await.0, StatusCode::FORBIDDEN);
	assert!(seen.lock().unwrap().is_empty());
	assert_eq!(b.send(&app, Method::POST, "/api/review_archive/me/gmails", &[], true).await.0, StatusCode::OK);

	assert_eq!(
		b.send(&app, Method::POST, "/playbook_mcp/authorize", &[], false).await.0,
		StatusCode::OK,
		"its own nonce guards it"
	);
	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!((got.method, got.path_and_query.as_str()), (Method::POST, "/playbook_mcp/authorize"));
	assert!(got.headers.contains_key("x-sa-assertion"));
}

#[tokio::test]
async fn playbooks_open_paths_pass_without_a_session_and_its_consent_page_sends_one_to_sign_in() {
	let db = TestDb::create().await;
	let (app, seen) = setup(&db).await;
	let mut b = Browser::default();

	for path in ["/playbook_mcp/token", "/.well-known/oauth-authorization-server/playbook_mcp", "/review_archive/mfe/x.js"] {
		assert_eq!(b.send(&app, Method::GET, path, &[("authorization", "Bearer t")], false).await.0, StatusCode::OK, "{path}");
		let got = seen.lock().unwrap().pop().unwrap();
		assert!(!got.headers.contains_key("x-sa-assertion"), "{path}: nobody vouched for");
		assert_eq!(got.headers["authorization"], "Bearer t", "{path}: a bearer is the client's");
	}
	assert_eq!(seen.lock().unwrap().len(), 0);

	let (status, headers) = b.send(&app, Method::GET, "/playbook_mcp/authorize?client_id=c&state=s", &[], false).await;
	assert_eq!(status, StatusCode::SEE_OTHER);
	assert_eq!(headers[header::LOCATION], "/auth/login?return_to=%2Fplaybook_mcp%2Fauthorize%3Fclient_id%3Dc%26state%3Ds");
}

#[tokio::test]
async fn grafana_is_reached_under_its_own_path_and_its_query_posts_need_no_csrf() {
	let db = TestDb::create().await;
	let (app, seen) = setup(&db).await;
	let mut b = Browser::default();
	let keys: Keys = SEED.parse::<Signer>().unwrap().public().parse().unwrap();

	assert_eq!(
		b.send(&app, Method::GET, "/api/review_archive/grafana/", &[], false).await.0,
		StatusCode::UNAUTHORIZED,
		"no session"
	);
	assert!(seen.lock().unwrap().is_empty());
	b.sign_in(&app).await;

	for (method, ours, theirs) in [
		(Method::GET, "/api/review_archive/grafana", "/grafana"),
		(Method::GET, "/api/review_archive/grafana/public/build/app.6f3e.js", "/grafana/public/build/app.6f3e.js"),
		(Method::POST, "/api/review_archive/grafana/api/ds/query?ds_type=x", "/grafana/api/ds/query?ds_type=x"),
		(
			Method::POST,
			"/api/review_archive/grafana/apis/query.grafana.app/v0alpha1/namespaces/default/query",
			"/grafana/apis/query.grafana.app/v0alpha1/namespaces/default/query",
		),
	] {
		let (status, headers) = b.send(&app, method.clone(), ours, &[("accept-encoding", "gzip")], false).await;
		assert_eq!(status, StatusCode::OK, "{method} {ours}: no CSRF asked");
		assert_eq!(headers[header::CONTENT_ENCODING], "gzip", "{ours}: passed as is, never decoded");
		assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=31536000", "{ours}");
		let got = seen.lock().unwrap().pop().unwrap();
		assert_eq!((&got.method, got.path_and_query.as_str()), (&method, theirs));
		assert_eq!(got.headers["accept-encoding"], "gzip");
		let path = theirs.split('?').next().unwrap();
		let token = got.headers["x-sa-assertion"].to_str().unwrap();
		sa_auth::verify(&keys, token, Service::ReviewArchive, method.as_str(), path, None, jiff::Timestamp::now().as_second()).expect("signed for the path the service sees");
	}

	assert_eq!(
		b.send(&app, Method::POST, "/api/review_archive/grafanas", &[], false).await.0,
		StatusCode::FORBIDDEN,
		"a neighbour of grafana is the service's API, CSRF and all"
	);
}

#[tokio::test]
async fn a_forwarded_path_never_climbs_out_of_its_prefix() {
	let db = TestDb::create().await;
	let (app, seen) = setup(&db).await;
	let mut b = Browser::default();
	b.sign_in(&app).await;

	for path in [
		"/api/review_archive/grafana/../me",
		"/api/review_archive/grafana/./../me",
		"/api/review_archive/grafana/%2e%2e/me",
		"/api/review_archive/grafana/%2E./me",
		"/api/review_archive/grafana/public/..%2f..%2fme",
		"/api/review_archive/grafana/..%5cme",
		"/api/review_archive/grafana/..",
		"/review_archive/mfe/%2e%2e/me",
	] {
		assert_eq!(b.send(&app, Method::POST, path, &[], false).await.0, StatusCode::BAD_REQUEST, "{path}");
	}
	assert!(seen.lock().unwrap().is_empty(), "nothing reached the service");
}

#[tokio::test]
async fn sites_is_told_the_caller_with_its_own_permissions_only() {
	let db = TestDb::create().await;
	let (app, seen) = setup(&db).await;
	let mut b = Browser::default();

	assert_eq!(b.send(&app, Method::GET, "/api/sites/leads", &[], false).await.0, StatusCode::UNAUTHORIZED, "no session");
	assert!(seen.lock().unwrap().is_empty());
	b.sign_in(&app).await;

	assert_eq!(b.send(&app, Method::GET, "/api/sites/leads?stage=created", &[], false).await.0, StatusCode::OK);
	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!(got.path_and_query, "/api/v1/leads?stage=created");
	let Principal::User(user) = got.assertion(Service::Sites, None).principal else {
		panic!("a session's call is a user's")
	};
	assert_eq!(user.email, "dev-admin@localhost");
	let held: Vec<&str> = user.permissions.iter().collect();
	assert!(!held.is_empty() && held.iter().all(|p| p.starts_with("sa:sites:")), "{held:?}");
	assert_eq!(held.len(), sa_auth::SA_ADMIN.members.iter().filter(|p| p.starts_with("sa:sites:")).count());

	assert_eq!(
		b.send(&app, Method::POST, "/api/sites/leads/aquafix/L-1/stage", &[], false).await.0,
		StatusCode::FORBIDDEN,
		"a write needs CSRF"
	);
	assert!(seen.lock().unwrap().is_empty());
	assert_eq!(
		b.send(&app, Method::POST, "/api/sites/leads/aquafix/L-1/stage", &[("idempotency-key", "k1")], true).await.0,
		StatusCode::OK
	);
	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!((got.method.as_str(), got.path_and_query.as_str()), ("POST", "/api/v1/leads/aquafix/L-1/stage"));
	assert_eq!(got.headers["idempotency-key"], "k1");
	got.assertion(Service::Sites, None);
}

#[tokio::test]
async fn the_sites_bundle_and_the_booking_hooks_pass_without_a_session() {
	let db = TestDb::create().await;
	let (app, seen) = setup(&db).await;
	let mut b = Browser::default();
	for (ours, theirs) in [
		("/sites/mfe/mfe-sites.js", "/mfe/mfe-sites.js"),
		("/api/hooks/booking/cal_com/aquafix", "/api/hooks/booking/cal_com/aquafix"),
	] {
		let method = if ours.starts_with("/api") { Method::POST } else { Method::GET };
		assert_eq!(b.send(&app, method, ours, &[], false).await.0, StatusCode::OK, "{ours}");
		let got = seen.lock().unwrap().pop().unwrap();
		assert_eq!(got.path_and_query, theirs);
		assert!(!got.headers.contains_key(sa_auth::HEADER), "{ours}: nobody vouched for");
	}
	assert_eq!(b.send(&app, Method::GET, "/sites/mfe/%2e%2e/api/v1/leads", &[], false).await.0, StatusCode::BAD_REQUEST);
	assert!(seen.lock().unwrap().is_empty());
}

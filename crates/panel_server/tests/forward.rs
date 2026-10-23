//! The forward (docs/ARCHITECTURE.md, Forward): what reaches a service behind the panel, and
//! what comes back from it.

use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
};

use axum::{
	Router,
	body::{Body, to_bytes},
	extract::Request,
	http::{HeaderMap, Method, StatusCode, header},
	response::IntoResponse,
};
use panel::testing::{TestDb, admin, panel};
use panel_server::{
	concierge::{Concierge, DEV_CODE, DevIdentity},
	forward::{Forward, Upstreams},
	http,
	signin::{SignIn, SignInConfig},
};
use sa_auth::{Keys, Service, Signer};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:59120";
const SEED: &str = "k1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// What a service behind the panel was sent.
#[derive(Clone, Debug)]
struct Seen {
	method: Method,
	path_and_query: String,
	headers: HeaderMap,
}

/// One service standing in for both: records each request, answers with a cookie of its own.
async fn upstream() -> (String, Arc<Mutex<Vec<Seen>>>) {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let log = seen.clone();
	let app = Router::new().fallback(move |req: Request| {
		let log = log.clone();
		async move {
			log.lock().unwrap().push(Seen {
				method: req.method().clone(),
				path_and_query: req.uri().path_and_query().unwrap().to_string(),
				headers: req.headers().clone(),
			});
			([(header::SET_COOKIE, "upstream=1; Path=/"), (header::HeaderName::from_static("x-upstream"), "yes")], "ok").into_response()
		}
	});
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(format!("http://{addr}"), seen)
}

async fn app(db: &TestDb, base: &str) -> Router {
	let who = DevIdentity {
		permissions: admin(),
		email: "dev-admin@localhost".to_owned(),
	};
	let config = SignInConfig {
		panel_origin: ORIGIN.to_owned(),
		concierge_origin: ORIGIN.to_owned(),
	};
	let sign_in = SignIn::new(panel(db).await, Concierge::dev(who), config);
	let upstreams = Upstreams {
		review_archive: base.parse().unwrap(),
		playbook: base.parse().unwrap(),
		signer: SEED.parse().unwrap(),
	};
	http::app(sign_in.clone()).merge(Forward::new(sign_in, upstreams).routes())
}

#[derive(Default)]
struct Browser {
	jar: HashMap<String, String>,
}

impl Browser {
	async fn send(&mut self, app: &Router, method: Method, uri: &str, extra: &[(&str, &str)], csrf: bool) -> (StatusCode, HeaderMap) {
		let mut req = Request::builder().method(method).uri(uri);
		let cookie: Vec<String> = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
		if !cookie.is_empty() {
			req = req.header(header::COOKIE, cookie.join("; "));
		}
		if csrf && let Some(t) = self.jar.get("sa_csrf") {
			req = req.header("x-sa-csrf", t);
		}
		for (k, v) in extra {
			req = req.header(*k, *v);
		}
		let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
		for c in res.headers().get_all(header::SET_COOKIE) {
			let (pair, _) = c.to_str().unwrap().split_once(';').unwrap();
			let (k, v) = pair.split_once('=').unwrap();
			self.jar.insert(k.to_owned(), v.to_owned());
		}
		let (status, headers) = (res.status(), res.headers().clone());
		to_bytes(res.into_body(), usize::MAX).await.unwrap();
		(status, headers)
	}

	async fn sign_in(&mut self, app: &Router) {
		let (_, headers) = self.send(app, Method::GET, "/auth/login", &[], false).await;
		let location = headers[header::LOCATION].to_str().unwrap().strip_prefix(ORIGIN).unwrap().to_owned();
		assert!(location.starts_with(&format!("/auth/callback?code={DEV_CODE}")));
		self.send(app, Method::GET, &location, &[], false).await;
		assert!(self.jar.contains_key("sa_session"));
	}
}

#[tokio::test]
async fn a_signed_in_call_reaches_the_service_with_an_assertion_and_without_the_browsers_cookies() {
	let db = TestDb::create().await;
	let (base, seen) = upstream().await;
	let app = app(&db, &base).await;
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
	let a = sa_auth::verify(&keys, token, Service::ReviewArchive, "GET", "/me/overview", now).expect("the panel's assertion, not the forged one");
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
	let (base, seen) = upstream().await;
	let app = app(&db, &base).await;
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
	let (base, seen) = upstream().await;
	let app = app(&db, &base).await;
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

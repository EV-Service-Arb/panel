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
	forward::{Cabinet, CabinetPins, ConciergeMfe, Forward, Upstreams, concierge_pins},
	http,
	signin::{SignIn, SignInConfig},
};
use sa_auth::{Keys, Service, Signer};
use sha2::{Digest, Sha256};
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

const CHIP_JS: &str = "customElements.define()";
const CHIP_CSS: &str = ".chip{}";

/// A cabinet: the chip's two files, and the ways a cabinet can go wrong.
async fn cabinet() -> (String, Arc<Mutex<Vec<Seen>>>) {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let log = seen.clone();
	let app = Router::new().fallback(move |req: Request| {
		let log = log.clone();
		async move {
			let path = req.uri().path().to_owned();
			let fresh = req.headers().get(header::IF_NONE_MATCH).is_some_and(|v| v == "\"c1\"");
			log.lock().unwrap().push(Seen {
				method: req.method().clone(),
				path_and_query: req.uri().path_and_query().unwrap().to_string(),
				headers: req.headers().clone(),
			});
			let cookie = (header::SET_COOKIE, "cabinet=1; Path=/");
			let inner = (header::HeaderName::from_static("x-cabinet-pod"), "10.42.0.7");
			let js = (header::CONTENT_TYPE, "text/javascript");
			match path.as_str() {
				"/cabinet/mfe/account-chip.js" if fresh => (StatusCode::NOT_MODIFIED, [(header::ETAG, "\"c1\"")]).into_response(),
				"/cabinet/mfe/account-chip.js" => (
					[
						(header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
						(header::CACHE_CONTROL, "public, max-age=300, stale-while-revalidate=86400"),
						(header::ETAG, "\"c1\""),
						(header::LAST_MODIFIED, "Sun, 11 Oct 2026 07:00:00 GMT"),
						cookie,
						inner,
					],
					CHIP_JS,
				)
					.into_response(),
				"/cabinet/mfe/account-chip.css" => ([(header::CONTENT_TYPE, "text/css")], CHIP_CSS).into_response(),
				"/cabinet/mfe/tampered.js" => ([js], "fetch('/api/v1/me').then(steal)").into_response(),
				"/cabinet/mfe/zipped.js" => ([js, (header::CONTENT_ENCODING, "gzip")], CHIP_JS).into_response(),
				"/cabinet/mfe/unpinned.js" => ([js], CHIP_JS).into_response(),
				"/cabinet/mfe/moved.js" => (StatusCode::FOUND, [(header::LOCATION, "http://ev-banking-cabinet.apps:50061/elsewhere.js")]).into_response(),
				"/cabinet/mfe/page.js" => ([(header::CONTENT_TYPE, "text/html")], "<html>").into_response(),
				"/cabinet/mfe/huge.js" => ([js], vec![b'x'; 5 << 20]).into_response(),
				"/cabinet/mfe/broken.js" => (StatusCode::INTERNAL_SERVER_ERROR, "stack trace at 10.42.0.7").into_response(),
				_ => (StatusCode::NOT_FOUND, "<html>404</html>").into_response(),
			}
		}
	});
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(format!("http://{addr}"), seen)
}

fn sha256(bytes: &[u8]) -> String {
	Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The chip's files pinned as they are; the misbehaving ones pinned to what they are not, so
/// that it is the cabinet's answer, not a missing pin, that turns them away. `unpinned.js`
/// has none.
fn pins() -> CabinetPins {
	let any = "0".repeat(64);
	let mut raw = format!(
		"account-chip.js={},account-chip.css={},zipped.js={}",
		sha256(CHIP_JS.as_bytes()),
		sha256(CHIP_CSS.as_bytes()),
		sha256(CHIP_JS.as_bytes())
	);
	for f in ["tampered.js", "moved.js", "page.js", "huge.js", "broken.js", "gone.js"] {
		raw.push_str(&format!(",{f}={any}"));
	}
	CabinetPins::parse(&raw).unwrap()
}

fn chip(base: &str) -> Router {
	Cabinet::new(&base.parse().unwrap(), pins()).routes()
}

async fn fetch(app: &Router, method: Method, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
	let mut req = Request::builder().method(method).uri(uri);
	for (k, v) in headers {
		req = req.header(*k, *v);
	}
	let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
	let (status, headers) = (res.status(), res.headers().clone());
	let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
	(status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn the_chips_pinned_bundle_comes_from_the_cabinet_with_nothing_of_the_browsers_and_nothing_of_the_cabinets() {
	let (base, seen) = cabinet().await;
	let app = chip(&base);

	let (status, h, body) = fetch(
		&app,
		Method::GET,
		"/cabinet/mfe/account-chip.js?v=3",
		&[
			("cookie", "__Host-sa_session=s; sa_csrf=t"),
			("x-sa-csrf", "t"),
			("x-sa-assertion", "forged"),
			("authorization", "Bearer t"),
			("accept-encoding", "gzip"),
			("if-none-match", "\"c0\""),
		],
	)
	.await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, CHIP_JS), "the pin matches");
	assert_eq!(h[header::CONTENT_TYPE], "text/javascript; charset=utf-8");
	assert_eq!(h[header::CACHE_CONTROL], "public, max-age=300", "the panel's, not the cabinet's stale-while-revalidate");
	assert_eq!(h[header::ETAG], "\"c1\"");
	assert_eq!(h[header::LAST_MODIFIED], "Sun, 11 Oct 2026 07:00:00 GMT");
	assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
	assert!(!h.contains_key(header::SET_COOKIE), "the cabinet sets nothing on the panel's origin");
	assert!(!h.contains_key("x-cabinet-pod"), "nothing but what describes the file");

	let got = seen.lock().unwrap().pop().unwrap();
	assert_eq!((got.method, got.path_and_query.as_str()), (Method::GET, "/cabinet/mfe/account-chip.js"));
	for name in ["cookie", "x-sa-csrf", "x-sa-assertion", "authorization", "accept-encoding"] {
		assert!(!got.headers.contains_key(name), "{name} went up");
	}
	assert_eq!(got.headers[header::IF_NONE_MATCH], "\"c0\"");

	let (status, h, body) = fetch(&app, Method::GET, "/cabinet/mfe/account-chip.css", &[]).await;
	assert_eq!((status, h[header::CONTENT_TYPE].to_str().unwrap(), body.as_str()), (StatusCode::OK, "text/css", CHIP_CSS));
	assert_eq!(h[header::CACHE_CONTROL], "public, max-age=300", "said by the panel when the cabinet says nothing too");

	let (status, h, body) = fetch(&app, Method::GET, "/cabinet/mfe/account-chip.js", &[("if-none-match", "\"c1\"")]).await;
	assert_eq!((status, body.as_str()), (StatusCode::NOT_MODIFIED, ""), "the browser's copy was checked when it came");
	assert_eq!(
		(&h[header::ETAG], &h[header::CACHE_CONTROL]),
		(&"\"c1\"".parse().unwrap(), &"public, max-age=300".parse().unwrap())
	);

	let (status, h, body) = fetch(&app, Method::HEAD, "/cabinet/mfe/account-chip.js", &[]).await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, ""));
	assert_eq!(h[header::CONTENT_TYPE], "text/javascript; charset=utf-8");
}

#[tokio::test]
async fn bytes_the_pin_does_not_vouch_for_are_never_served() {
	let (base, seen) = cabinet().await;
	let app = chip(&base);
	for (file, why) in [("tampered.js", "not the pinned bytes"), ("zipped.js", "encoded, though the pinned bytes")] {
		let (status, h, body) = fetch(&app, Method::GET, &format!("/cabinet/mfe/{file}"), &[]).await;
		assert_eq!((status, body.as_str()), (StatusCode::BAD_GATEWAY, r#"{"error":"the cabinet did not answer"}"#), "{file}: {why}");
		assert!(!h.contains_key(header::ETAG) && !h.contains_key(header::CACHE_CONTROL), "{file}: nothing of it");
	}
	assert_eq!(seen.lock().unwrap().len(), 2);

	let (status, _, body) = fetch(&app, Method::GET, "/cabinet/mfe/unpinned.js", &[]).await;
	assert_eq!((status, body.as_str()), (StatusCode::NOT_FOUND, r#"{"error":"not found"}"#), "no pin, no file");
	assert_eq!(seen.lock().unwrap().len(), 2, "and the cabinet was not asked");
}

#[tokio::test]
async fn the_cabinet_is_read_only_and_only_for_the_bundles_plain_file_names() {
	let (base, seen) = cabinet().await;
	let app = chip(&base);

	for m in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH, Method::OPTIONS] {
		assert_eq!(fetch(&app, m.clone(), "/cabinet/mfe/account-chip.js", &[]).await.0, StatusCode::METHOD_NOT_ALLOWED, "{m}");
	}
	for uri in [
		"/cabinet/mfe/../api/me.js",
		"/cabinet/mfe/%2e%2e/api/me.js",
		"/cabinet/mfe/%2E%2E%2Fapi%2Fme.js",
		"/cabinet/mfe/..%2fapi.js",
		"/cabinet/mfe/%252e%252e/api.js",
		"/cabinet/mfe//account-chip.js",
		"/cabinet/mfe/x/./account-chip.js",
		"/cabinet/mfe/account-chip.js/",
		"/cabinet/mfe/account-chip.js.map",
		"/cabinet/mfe/index.html",
		"/cabinet/mfe/.env.js",
	] {
		let (status, h, body) = fetch(&app, Method::GET, uri, &[]).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
		assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/json"), "{uri}: {h:?}");
		assert_eq!(body, r#"{"error":"not found"}"#, "{uri}");
	}
	assert!(seen.lock().unwrap().is_empty(), "nothing reached the cabinet");
}

#[tokio::test]
async fn a_cabinet_that_redirects_misbehaves_or_is_gone_is_a_502_that_names_nothing_inside() {
	let (base, seen) = cabinet().await;
	let app = chip(&base);

	let (status, _, body) = fetch(&app, Method::GET, "/cabinet/mfe/gone.js", &[]).await;
	assert_eq!(
		(status, body.as_str()),
		(StatusCode::NOT_FOUND, r#"{"error":"not found"}"#),
		"the cabinet's 404 page stays its own"
	);
	for (file, why) in [
		("moved.js", "a redirect is not followed"),
		("page.js", "HTML is no script"),
		("huge.js", "over the bound"),
		("broken.js", "its 500"),
	] {
		let (status, h, body) = fetch(&app, Method::GET, &format!("/cabinet/mfe/{file}"), &[]).await;
		assert_eq!(status, StatusCode::BAD_GATEWAY, "{file}: {why}");
		assert!(!h.contains_key(header::LOCATION), "{file}");
		assert_eq!(body, r#"{"error":"the cabinet did not answer"}"#, "{file}");
	}
	assert_eq!(seen.lock().unwrap().len(), 5, "each asked once, the redirect not followed");

	// A port nobody listens on: bound, then let go.
	let gone = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
	let app = chip(&format!("http://{gone}"));
	let (status, _, body) = fetch(&app, Method::GET, "/cabinet/mfe/account-chip.js", &[]).await;
	assert_eq!((status, body.as_str()), (StatusCode::BAD_GATEWAY, r#"{"error":"the cabinet did not answer"}"#));
}

const ELEMENT_JS: &str = "customElements.define('account-chip')";

/// A concierge's web port: its manifest (`registry.json`) and the hashed files. `manifest`
/// is what it says today; every request is recorded.
async fn concierge(manifest: &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
	let seen = Arc::new(Mutex::new(Vec::new()));
	let log = seen.clone();
	let app = Router::new().fallback(move |req: Request| {
		let log = log.clone();
		async move {
			let path = req.uri().path().to_owned();
			// Whatever the browser sent, a concierge that honours it would answer 304.
			let conditional = req.headers().contains_key(header::IF_NONE_MATCH);
			log.lock().unwrap().push(Seen {
				method: req.method().clone(),
				path_and_query: req.uri().path_and_query().unwrap().to_string(),
				headers: req.headers().clone(),
			});
			let js = (header::CONTENT_TYPE, "text/javascript");
			match path.as_str() {
				"/auth/mfe/registry.json" => ([(header::CONTENT_TYPE, "application/json")], manifest).into_response(),
				"/auth/mfe/account-chip-ab12.js" if conditional => StatusCode::NOT_MODIFIED.into_response(),
				"/auth/mfe/account-chip-ab12.js" => ([js, (header::ETAG, "\"c1\"")], ELEMENT_JS).into_response(),
				"/auth/mfe/sign-in-cd34.js" => ([js], "tampered").into_response(),
				_ => (StatusCode::NOT_FOUND, "<html>404 at 10.42.0.7</html>").into_response(),
			}
		}
	});
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(format!("http://{addr}"), seen)
}

const MANIFEST: &str = r#"[
	{"name":"concierge.other","scriptUrl":"/auth/mfe/other-ff.js","extra":[1,2]},
	{"name":"concierge.account-chip","scriptUrl":"/api/auth/mfe/account-chip-ab12.js","integrity":"x"},
	{"name":"concierge.sign-in","scriptUrl":"/auth/mfe/sign-in-cd34.js"}
]"#;

fn element(base: &str) -> Router {
	let pins = format!("account-chip.js={},sign-in.js={}", sha256(ELEMENT_JS.as_bytes()), sha256(ELEMENT_JS.as_bytes()));
	ConciergeMfe::new(&base.parse().unwrap(), concierge_pins(&pins).unwrap()).routes()
}

fn bodies(seen: &Arc<Mutex<Vec<Seen>>>) -> Vec<String> {
	seen.lock().unwrap().iter().map(|s| s.path_and_query.clone()).collect()
}

#[tokio::test]
async fn concierges_element_is_served_once_verified_and_then_from_memory_with_the_pin_as_its_tag() {
	let (base, seen) = concierge(MANIFEST).await;
	let app = element(&base);

	// The browser's validators never go up: concierge's 304 cannot stand in for the bytes.
	let (status, h, body) = fetch(&app, Method::GET, "/auth/mfe/account-chip.js?x=1", &[("if-none-match", "\"c1\""), ("cookie", "sa_session=s")]).await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, ELEMENT_JS));
	let etag = format!("\"{}\"", sha256(ELEMENT_JS.as_bytes()));
	assert_eq!(h[header::ETAG], etag.as_str(), "the pin is the tag, not concierge's");
	assert_eq!(h[header::CACHE_CONTROL], "public, max-age=300");
	assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
	assert_eq!(bodies(&seen), ["/auth/mfe/registry.json", "/auth/mfe/account-chip-ab12.js"]);
	for s in seen.lock().unwrap().iter() {
		assert!(!s.headers.contains_key(header::IF_NONE_MATCH) && !s.headers.contains_key(header::COOKIE), "{s:?}");
	}

	// Verified once: a second request, and a HEAD, do not ask concierge.
	for method in [Method::GET, Method::HEAD] {
		let (status, _, _) = fetch(&app, method, "/auth/mfe/account-chip.js", &[]).await;
		assert_eq!(status, StatusCode::OK);
	}
	// A browser holding the tag is told so, whatever concierge would say.
	for held in [etag.clone(), format!("W/{etag}"), format!("\"zz\", {etag}"), "*".to_owned()] {
		let (status, h, body) = fetch(&app, Method::GET, "/auth/mfe/account-chip.js", &[("if-none-match", &held)]).await;
		assert_eq!((status, body.as_str()), (StatusCode::NOT_MODIFIED, ""), "{held}");
		assert_eq!(h[header::ETAG], etag.as_str());
	}
	let (status, _, _) = fetch(&app, Method::GET, "/auth/mfe/account-chip.js", &[("if-none-match", "\"stale\"")]).await;
	assert_eq!(status, StatusCode::OK, "another tag gets the file");
	assert_eq!(seen.lock().unwrap().len(), 2, "concierge was asked for it once");
}

#[tokio::test]
async fn bytes_the_pin_does_not_vouch_for_and_every_other_failure_of_concierge_are_a_502_that_names_nothing() {
	let (base, seen) = concierge(MANIFEST).await;
	let app = element(&base);
	let want = (StatusCode::BAD_GATEWAY, r#"{"error":"concierge did not answer"}"#);

	let (status, h, body) = fetch(&app, Method::GET, "/auth/mfe/sign-in.js", &[]).await;
	assert_eq!((status, body.as_str()), want, "not the pinned bytes");
	assert!(!h.contains_key(header::ETAG) && !h.contains_key(header::CACHE_CONTROL));
	let (status, ..) = fetch(&app, Method::GET, "/auth/mfe/sign-in.js", &[]).await;
	assert_eq!(status, StatusCode::BAD_GATEWAY, "a failure is not kept");
	assert_eq!(seen.lock().unwrap().len(), 4, "asked again each time");

	// A name that is not served, or not an element at all, never reaches concierge.
	let before = seen.lock().unwrap().len();
	for uri in ["/auth/mfe/other-ff.js", "/auth/mfe/registry.json", "/auth/mfe/account-chip.css", "/auth/mfe/..%2fx.js"] {
		let (status, _, body) = fetch(&app, Method::GET, uri, &[]).await;
		assert_eq!((status, body.as_str()), (StatusCode::NOT_FOUND, r#"{"error":"not found"}"#), "{uri}");
	}
	assert_eq!(seen.lock().unwrap().len(), before);

	// A 404 from concierge, for the manifest or for the file, is its failure, not a missing file.
	let (gone, _) = concierge("[]").await;
	let (status, _, body) = fetch(&element(&gone), Method::GET, "/auth/mfe/account-chip.js", &[]).await;
	assert_eq!((status, body.as_str()), want, "no entry for the element");
	let (base, _) = concierge(r#"[{"name":"concierge.account-chip","scriptUrl":"/auth/mfe/missing-99.js"}]"#).await;
	let (status, _, body) = fetch(&element(&base), Method::GET, "/auth/mfe/account-chip.js", &[]).await;
	assert_eq!((status, body.as_str()), want, "404 of the file");
}

#[tokio::test]
async fn a_manifest_that_is_broken_or_names_no_plain_script_is_a_502() {
	for manifest in [
		"not json",
		"{}",
		"[]",
		r#"[{"name":"concierge.account-chip"}]"#,
		r#"[{"name":"concierge.account-chip","scriptUrl":7}]"#,
		r#"[{"name":"concierge.account-chip","scriptUrl":"/auth/mfe/account-chip-ab12.js.map"}]"#,
		r#"[{"name":"concierge.account-chip","scriptUrl":"/auth/mfe/%2e%2e%2fx.js"}]"#,
		r#"[{"name":"concierge.account-chip","scriptUrl":"/auth/mfe/"}]"#,
	] {
		let (base, seen) = concierge(manifest).await;
		let (status, _, body) = fetch(&element(&base), Method::GET, "/auth/mfe/account-chip.js", &[]).await;
		assert_eq!((status, body.as_str()), (StatusCode::BAD_GATEWAY, r#"{"error":"concierge did not answer"}"#), "{manifest}");
		assert_eq!(bodies(&seen), ["/auth/mfe/registry.json"], "{manifest}: no file was asked for");
	}
	// Entries of other shapes beside the one wanted are not an error (MANIFEST has such).
	let (base, _) = concierge(r#"[1,"x",null,{"name":7},{"name":"concierge.account-chip","scriptUrl":"/auth/mfe/account-chip-ab12.js"}]"#).await;
	let (status, ..) = fetch(&element(&base), Method::GET, "/auth/mfe/account-chip.js", &[]).await;
	assert_eq!(status, StatusCode::OK);
}

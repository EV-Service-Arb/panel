//! The front end's files behind the API's routes: directories, the 404 page, the headers, and
//! nothing outside the directory.

use std::path::{Path, PathBuf};

use axum::{
	Router,
	body::{Body, to_bytes},
	http::{HeaderMap, Method, Request, StatusCode, header},
	routing::get,
};
use panel_server::{
	forward::{Cabinet, CabinetPins, ConciergeMfe, concierge_pins},
	web::{self, CSP, Files, csp},
};
use tower::ServiceExt;
use uuid::Uuid;

/// A scratch directory holding `web/` (a stand-in export) and, beside it, a file the server
/// must never hand out.
struct Export {
	root: PathBuf,
}

impl Export {
	fn new() -> Self {
		let root = std::env::temp_dir().join(format!("panel-web-{}", Uuid::now_v7().simple()));
		for (path, body) in [
			("web/index.html", "<html>overview</html>"),
			("web/404.html", "<html>not here</html>"),
			("web/leads/index.html", "<html>leads</html>"),
			("web/_next/static/chunks/app-3f2a.js", "console.log(1)"),
			("web/favicon.ico", "ico"),
			("secret.txt", "do not serve"),
		] {
			let path = root.join(path);
			std::fs::create_dir_all(path.parent().unwrap()).unwrap();
			std::fs::write(path, body).unwrap();
		}
		Self { root }
	}

	fn web(&self) -> PathBuf {
		self.root.join("web")
	}
}

impl Drop for Export {
	fn drop(&mut self) {
		// Scratch space: a leftover directory in the temp dir harms nothing.
		let _gone = std::fs::remove_dir_all(&self.root);
	}
}

/// Routes shaped like the real app's: `/health`, and a nested `/api/v1`.
fn app_routes() -> Router {
	let api = Router::new().route("/me", get(|| async { "me" }));
	Router::new().route("/health", get(|| async { "ok" })).nest("/api/v1", api)
}

fn app(dir: &Path) -> Router {
	web::serve(app_routes(), Files::new(dir).unwrap())
}

async fn send(app: &Router, method: Method, uri: &str, headers: &[(header::HeaderName, &str)]) -> (StatusCode, HeaderMap, String) {
	let mut req = Request::builder().method(method).uri(uri);
	for (k, v) in headers {
		req = req.header(k, *v);
	}
	let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
	let status = res.status();
	let headers = res.headers().clone();
	let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
	(status, headers, String::from_utf8_lossy(&body).into_owned())
}

async fn get_(app: &Router, uri: &str) -> (StatusCode, HeaderMap, String) {
	send(app, Method::GET, uri, &[]).await
}

fn is_page(h: &HeaderMap) -> bool {
	h[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html")
		&& h[header::CACHE_CONTROL] == "no-cache"
		&& h[header::CONTENT_SECURITY_POLICY] == CSP
		&& h[header::X_CONTENT_TYPE_OPTIONS] == "nosniff"
		&& h[header::REFERRER_POLICY] == "same-origin"
}

#[tokio::test]
async fn pages_directories_and_the_404_page() {
	let export = Export::new();
	let app = app(&export.web());

	let (status, h, body) = get_(&app, "/").await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, "<html>overview</html>"));
	assert!(is_page(&h), "{h:?}");
	assert!(!h.contains_key(header::LAST_MODIFIED), "the store's 1970 mtime is never told");

	let (status, h, body) = get_(&app, "/leads/").await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, "<html>leads</html>"));
	assert!(is_page(&h));
	let (status, h, _) = get_(&app, "/leads").await;
	assert!(status.is_redirection(), "{status}");
	assert_eq!(h[header::LOCATION], "/leads/");

	let (status, h, body) = get_(&app, "/no/such/screen/").await;
	assert_eq!((status, body.as_str()), (StatusCode::NOT_FOUND, "<html>not here</html>"));
	assert!(is_page(&h), "the 404 page is a page: {h:?}");

	let (status, h, body) = get_(&app, "/favicon.ico").await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, "ico"));
	assert_eq!(h[header::CACHE_CONTROL], "no-cache", "not content-hashed");
	assert!(!h.contains_key(header::CONTENT_SECURITY_POLICY));

	let (status, _, _) = send(&app, Method::POST, "/", &[]).await;
	assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn the_platform_policy_is_sent_only_when_switched_on() {
	let export = Export::new();
	for (on, want) in [(false, CSP.to_owned()), (true, csp(true))] {
		let app = web::serve(app_routes(), Files::new(&export.web()).unwrap().platform_mfe(on));
		for uri in ["/", "/no/such/screen/"] {
			let (_, h, _) = get_(&app, uri).await;
			assert_eq!(h[header::CONTENT_SECURITY_POLICY], want.as_str(), "{uri}, platform_mfe={on}");
		}
		let (_, h, _) = get_(&app, "/favicon.ico").await;
		assert!(!h.contains_key(header::CONTENT_SECURITY_POLICY));
	}
	assert_ne!(csp(true), CSP);
}

#[tokio::test]
async fn hashed_assets_are_immutable_and_nothing_revalidates_by_date() {
	let export = Export::new();
	let app = app(&export.web());

	let (status, h, body) = get_(&app, "/_next/static/chunks/app-3f2a.js").await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, "console.log(1)"));
	assert_eq!(h[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
	assert!(h[header::CONTENT_TYPE].to_str().unwrap().contains("javascript"), "{h:?}");
	assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

	let (status, h, _) = get_(&app, "/_next/static/chunks/gone-0000.js").await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	assert_eq!(h[header::CACHE_CONTROL], "no-cache", "a miss is not cached for a year");

	// Every release's files carry the same mtime, so a date says nothing about which one the
	// browser holds: the page is sent whole.
	let later = "Tue, 01 Jan 2030 00:00:00 GMT";
	let (status, _, body) = send(&app, Method::GET, "/", &[(header::IF_MODIFIED_SINCE, later)]).await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, "<html>overview</html>"));
}

#[tokio::test]
async fn the_api_side_never_falls_through_to_the_front_end() {
	let export = Export::new();
	let app = app(&export.web());

	assert_eq!(get_(&app, "/health").await.2, "ok");
	assert_eq!(get_(&app, "/api/v1/me").await.2, "me");
	for uri in [
		"/api/v1/nope",
		"/api/ingest/v1/events",
		"/api",
		"/auth/login",
		"/health/",
		"/grafana",
		"/grafana/d/abc",
		"/cabinet",
		"/cabinet/mfe/account-chip.js",
	] {
		let (status, h, body) = get_(&app, uri).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
		assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/json"), "{uri}: {h:?}");
		assert_eq!(body, r#"{"error":"not found"}"#, "{uri}");
	}
}

#[tokio::test]
async fn the_cabinets_bundle_is_mounted_only_when_configured_and_pinned_and_is_never_a_page() {
	let export = Export::new();
	// A port nobody listens on: a 502 tells the route is mounted, and reached the cabinet.
	let gone = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
	let url = || Some(format!("http://{gone}").parse().unwrap());
	let pins = || Some(CabinetPins::parse(&format!("account-chip.js={}", "0".repeat(64))).unwrap());
	let cabinet = Cabinet::configured(url(), pins()).unwrap();
	let mounted = web::serve(app_routes().merge(cabinet.routes()), Files::new(&export.web()).unwrap());
	let (status, _, body) = get_(&mounted, "/cabinet/mfe/account-chip.js").await;
	assert_eq!((status, body.as_str()), (StatusCode::BAD_GATEWAY, r#"{"error":"the cabinet did not answer"}"#));
	for uri in ["/cabinet/mfe/", "/cabinet/mfe", "/cabinet/mfe/%2e%2e/x.js", "/cabinet/api/me"] {
		let (status, h, body) = get_(&mounted, uri).await;
		assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
		assert!(h[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/json"), "{uri}: {h:?}");
		assert_eq!(body, r#"{"error":"not found"}"#, "{uri}");
	}
	assert_eq!(send(&mounted, Method::POST, "/cabinet/mfe/account-chip.js", &[]).await.0, StatusCode::METHOD_NOT_ALLOWED);

	// Fail closed: the address without the pins mounts nothing, and so does neither.
	assert_eq!(Cabinet::configured(url(), None).err(), Some(vec!["PANEL_CABINET_MFE_PINS"]));
	assert_eq!(Cabinet::configured(None, pins()).err(), Some(vec!["PANEL_CABINET_URL"]));
	assert_eq!(Cabinet::configured(None, None).err(), Some(vec!["PANEL_CABINET_URL", "PANEL_CABINET_MFE_PINS"]));
	let unmounted = app(&export.web());
	for uri in ["/cabinet/mfe/account-chip.js", "/cabinet", "/cabinet/elsewhere"] {
		let (status, h, body) = get_(&unmounted, uri).await;
		assert_eq!((status, body.as_str()), (StatusCode::NOT_FOUND, r#"{"error":"not found"}"#), "{uri}");
		assert!(!is_page(&h), "{uri}: not the SPA's 404 page, /cabinet is reserved");
	}
}

/// A concierge's web port: the manifest and one hashed script, the way its `web/` build
/// writes them, with a `Set-Cookie` the panel must not pass on.
async fn fake_concierge(script: &'static str) -> std::net::SocketAddr {
	let manifest = r#"[{"name":"concierge.sign-in","tag":"mfe-concierge-sign-in","scriptUrl":"/api/auth/mfe/sign-in-AAAA.js","kind":"component"},
		{"name":"concierge.account-chip","tag":"mfe-concierge-account-chip","scriptUrl":"/api/auth/mfe/account-chip-BBBB.js","kind":"component"}]"#;
	let app = Router::new()
		.route("/auth/mfe/registry.json", get(move || async move { ([(header::CONTENT_TYPE, "application/json")], manifest) }))
		.route(
			"/auth/mfe/account-chip-BBBB.js",
			get(move || async move { ([(header::CONTENT_TYPE, "text/javascript"), (header::SET_COOKIE, "a=b")], script) }),
		)
		.route("/auth/mfe/sign-in-AAAA.js", get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<html>") }));
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await });
	addr
}

#[tokio::test]
async fn concierges_elements_are_served_pinned_under_stable_names() {
	use sha2::{Digest, Sha256};
	let export = Export::new();
	let script = "customElements.define('x',class{})";
	let hex = |b: &[u8]| Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect::<String>();
	let addr = fake_concierge(script).await;
	let url = || Some(format!("http://{addr}").parse().unwrap());
	let pins = |chip: &str| Some(concierge_pins(&format!("account-chip.js={chip},sign-in.js={}", hex(b"<html>"))).unwrap());

	let ok = web::serve(
		app_routes().merge(ConciergeMfe::configured(url(), pins(&hex(script.as_bytes()))).unwrap().routes()),
		Files::new(&export.web()).unwrap(),
	);
	let (status, h, body) = get_(&ok, "/auth/mfe/account-chip.js").await;
	assert_eq!((status, body.as_str()), (StatusCode::OK, script));
	assert_eq!(h[header::CACHE_CONTROL], "public, max-age=300");
	assert!(!h.contains_key(header::SET_COOKIE), "{h:?}");
	// served by the stable name only; the hashed one and the unpinned are no files of ours
	for uri in [
		"/auth/mfe/account-chip-BBBB.js",
		"/auth/mfe/registry.json",
		"/auth/mfe/account-chip.css",
		"/auth/mfe/other.js",
		"/auth/mfe/%2e%2e",
	] {
		assert_eq!(get_(&ok, uri).await.0, StatusCode::NOT_FOUND, "{uri}");
	}
	// a script answered as a page does not run
	assert_eq!(get_(&ok, "/auth/mfe/sign-in.js").await.0, StatusCode::BAD_GATEWAY);
	assert_eq!(send(&ok, Method::POST, "/auth/mfe/account-chip.js", &[]).await.0, StatusCode::METHOD_NOT_ALLOWED);

	// other bytes than the pinned ones come down as nothing
	let wrong = web::serve(
		app_routes().merge(ConciergeMfe::configured(url(), pins(&"0".repeat(64))).unwrap().routes()),
		Files::new(&export.web()).unwrap(),
	);
	let (status, _, body) = get_(&wrong, "/auth/mfe/account-chip.js").await;
	assert_eq!((status, body.as_str()), (StatusCode::BAD_GATEWAY, r#"{"error":"concierge did not answer"}"#));

	assert_eq!(ConciergeMfe::configured(url(), None).err(), Some(vec!["PANEL_CONCIERGE_MFE_PINS"]));
	assert_eq!(ConciergeMfe::configured(None, None).err(), Some(vec!["PANEL_CONCIERGE_URL", "PANEL_CONCIERGE_MFE_PINS"]));
	// not mounted: /auth stays the API's, a JSON 404
	let (status, h, _) = get_(&app(&export.web()), "/auth/mfe/account-chip.js").await;
	assert_eq!(status, StatusCode::NOT_FOUND);
	assert!(!is_page(&h));
}

#[tokio::test]
async fn nothing_outside_the_directory() {
	let export = Export::new();
	let app = app(&export.web());
	for uri in [
		"/../secret.txt",
		"/%2e%2e/secret.txt",
		"/..%2fsecret.txt",
		"/leads/..%2f..%2fsecret.txt",
		"/%2fetc%2fpasswd",
		"//etc/passwd",
	] {
		let (status, _, body) = get_(&app, uri).await;
		assert!(!body.contains("do not serve") && !body.contains("root:"), "{uri} leaked: {body}");
		assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
	}
}

#[test]
fn a_directory_without_the_export_is_refused_at_boot() {
	let export = Export::new();
	let e = Files::new(&export.root).unwrap_err();
	assert!(format!("{e}").contains("has no index.html"), "{e}");
	assert!(Files::new(&export.root.join("missing")).is_err());
}

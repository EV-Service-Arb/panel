//! The front end's files behind the API's routes: directories, the 404 page, the headers, and
//! nothing outside the directory.

use std::path::{Path, PathBuf};

use axum::{
	Router,
	body::{Body, to_bytes},
	http::{HeaderMap, Method, Request, StatusCode, header},
	routing::get,
};
use panel_server::web::{self, CSP, Files, csp};
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
async fn the_retired_mfe_paths_are_json_404s_never_a_page() {
	// The chip and the sign-in load from the platform now; a tab on an earlier build may still
	// ask here, and a script request must not get the panel's HTML.
	let export = Export::new();
	let app = app(&export.web());
	for uri in ["/cabinet/mfe/account-chip.js", "/cabinet", "/cabinet/elsewhere", "/auth/mfe/account-chip.js"] {
		let (status, h, body) = get_(&app, uri).await;
		assert_eq!((status, body.as_str()), (StatusCode::NOT_FOUND, r#"{"error":"not found"}"#), "{uri}");
		assert!(!is_page(&h), "{uri}: not the SPA's 404 page");
	}
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

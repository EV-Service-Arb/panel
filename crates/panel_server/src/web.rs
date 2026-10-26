//! The front end: the Next static export (`frontend/`, `npm run build` → `out/`), served from
//! `PANEL_WEB_DIR` on the same origin as `/api` and `/auth`, behind every route of its own.
//!
//! ```text
//! /api, /auth, /health, /grafana,  never the front end: a route of theirs, else a JSON 404
//! /cabinet, the forward's prefixes
//! /review_archive/<view>           the review_archive page: the dashboard routes inside it
//! a directory                      its index.html (the export writes leads/index.html);
//!                                  /leads is redirected to /leads/ first
//! a file                           itself
//! anything else                    404.html, status 404
//! ```
//!
//! Files are read by tower-http's `ServeDir`, which refuses any path that would leave the
//! directory (`..`, a drive or a root component, encoded or not) before touching the disk.
//!
//! Headers: `/_next/static/*` is content-hashed, so `immutable` for a year; everything else
//! `no-cache`, and every HTML page a CSP.

use std::{
	collections::BTreeSet,
	io,
	path::{Path, PathBuf},
};

use axum::{
	Json, Router,
	body::Body,
	extract::Request,
	http::{HeaderValue, StatusCode, header},
	response::{IntoResponse, Response},
};
use base64::Engine;
use eyre::WrapErr;
use serde_json::json;
use sha2::{Digest, Sha256};
use tower_http::{
	services::{ServeDir, ServeFile},
	set_status::SetStatus,
};

/// The platform's origin, where the hosts load its micro-frontends from.
const PLATFORM_ORIGIN: &str = "https://evinvest.ltd";

/// Cloudflare's Turnstile: the sign-in element loads its `api.js` and draws the challenge in
/// an iframe, both from this origin.
const TURNSTILE_ORIGIN: &str = "https://challenges.cloudflare.com";

/// The pages' policy. Scripts run from this origin, and inline only as `inline` names them:
/// Next inlines its bootstrap and the RSC payload, so every inline script of the export is
/// admitted by its hash, on every page (a client-side navigation keeps the first page's
/// policy). The kit sets inline styles, hence `'unsafe-inline'` in `style-src`;
/// review_archive's dashboard is wasm, hence `'wasm-unsafe-eval'`.
///
/// With `platform_mfe`, also the platform's MFE directories (concierge's `/api/auth/mfe/`, the
/// wallet's `/wallet/mfe/`) in `script-src`, the wallet's in `style-src` (its stylesheet's
/// `<link>`), its origin in `connect-src` (the elements call its API, cross-origin), and
/// Turnstile's origin in `script-src` and a `frame-src` of it alone (without one the frame
/// would fall back to `default-src 'self'` and the sign-in dialog would show "Captcha failed
/// to load"). Directories, never the origin: a `script-src` of the whole origin would let any
/// script the platform serves run in the panel's session, not only the elements it publishes
/// for hosts.
fn csp(platform_mfe: bool, inline: &BTreeSet<String>) -> String {
	let hashes: String = inline.iter().map(|h| format!(" '{h}'")).collect();
	let (scripts, styles, connect, frames) = if platform_mfe {
		(
			format!(" {PLATFORM_ORIGIN}/api/auth/mfe/ {PLATFORM_ORIGIN}/wallet/mfe/ {TURNSTILE_ORIGIN}"),
			format!(" {PLATFORM_ORIGIN}/wallet/mfe/"),
			format!(" {PLATFORM_ORIGIN}"),
			format!("; frame-src {TURNSTILE_ORIGIN}"),
		)
	} else {
		(String::new(), String::new(), String::new(), String::new())
	};
	format!(
		"default-src 'self'; script-src 'self'{hashes} 'wasm-unsafe-eval'{scripts}; style-src 'self' 'unsafe-inline'{styles}; img-src 'self' data:; \
		 connect-src 'self'{connect}{frames}; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
	)
}

/// Trusted Types, reported in the console only: the step before enforcing them (banking
/// WALLET-INPAGE-SPEC control (g)).
const TRUSTED_TYPES_REPORT_ONLY: &str = "require-trusted-types-for 'script'";

/// `sha256-<base64>` of every inline `<script>` (one without `src`) of the export's pages.
fn inline_scripts(dir: &Path) -> eyre::Result<BTreeSet<String>> {
	let mut pages = Vec::new();
	let mut dirs = vec![dir.to_path_buf()];
	while let Some(d) = dirs.pop() {
		for entry in std::fs::read_dir(&d).wrap_err_with(|| format!("reading {}", d.display()))? {
			let path: PathBuf = entry.wrap_err_with(|| format!("reading {}", d.display()))?.path();
			if path.is_dir() {
				dirs.push(path);
			} else if path.extension().is_some_and(|e| e == "html") {
				pages.push(path);
			}
		}
	}
	let mut hashes = BTreeSet::new();
	for page in pages {
		let html = std::fs::read_to_string(&page).wrap_err_with(|| format!("reading {}", page.display()))?;
		for script in inline_bodies(&html).wrap_err_with(|| format!("the scripts of {}", page.display()))? {
			hashes.insert(format!("sha256-{}", base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script.as_bytes()))));
		}
	}
	Ok(hashes)
}

/// The text of each `<script>` without a `src`, as the browser hashes it (a script's text is
/// raw: no entity is decoded in it).
fn inline_bodies(html: &str) -> eyre::Result<Vec<&str>> {
	let lower = html.to_ascii_lowercase();
	let mut bodies = Vec::new();
	let mut at = 0;
	while let Some(open) = lower[at..].find("<script").map(|i| at + i) {
		let name_end = open + "<script".len();
		if !lower[name_end..].starts_with(|c: char| c == '>' || c.is_ascii_whitespace()) {
			at = name_end;
			continue;
		}
		let tag_end = lower[name_end..].find('>').map(|i| name_end + i).ok_or_else(|| eyre::eyre!("a <script> tag never closes"))?;
		let close = lower[tag_end..].find("</script").map(|i| tag_end + i).ok_or_else(|| eyre::eyre!("a <script> never ends"))?;
		let has_src = lower[name_end..tag_end].split(|c: char| c.is_ascii_whitespace()).any(|a| a == "src" || a.starts_with("src="));
		if !has_src {
			bodies.push(&html[tag_end + 1..close]);
		}
		at = close;
	}
	Ok(bodies)
}

/// Where the build puts what it names by content hash.
const HASHED: &str = "/_next/static/";

/// Paths that belong to the API side, whether or not a route of theirs is mounted: an unknown
/// one is a JSON 404, never the front end's page. `/grafana` is held for the dashboards;
/// `/cabinet` once served the cabinet's account chip here, and a tab still on an earlier
/// build may ask for `/cabinet/mfe/*`: a script request must get a 404, not the panel's HTML.
const RESERVED: [&str; 5] = ["/api", "/auth", "/health", "/grafana", "/cabinet"];

/// The export's files.
#[derive(Clone, Debug)]
pub struct Files {
	dir: ServeDir<SetStatus<ServeFile>>,
	/// The hashes of the export's inline scripts, read once at boot.
	inline: BTreeSet<String>,
	/// Built once at boot, so a page costs no allocation for it.
	csp: HeaderValue,
}

impl Files {
	/// The export in `dir`; it must hold `index.html` and `404.html`, so a wrong path fails the
	/// boot rather than every page.
	pub fn new(dir: &Path) -> eyre::Result<Self> {
		for page in ["index.html", "404.html"] {
			let path = dir.join(page);
			eyre::ensure!(path.is_file(), "PANEL_WEB_DIR {} has no {page}: not a build of frontend/", dir.display());
		}
		let inline = inline_scripts(dir)?;
		let csp = HeaderValue::from_str(&csp(false, &inline)).expect("the policy is ASCII without control characters");
		let dir = ServeDir::new(dir).append_index_html_on_directories(true).not_found_service(ServeFile::new(dir.join("404.html")));
		Ok(Self { dir, inline, csp })
	}

	/// The pages' policy with or without the platform's MFE directories.
	#[must_use]
	pub fn platform_mfe(mut self, on: bool) -> Self {
		self.csp = HeaderValue::from_str(&csp(on, &self.inline)).expect("the policy is ASCII without control characters");
		self
	}
}

/// `app`, with the front end answering whatever none of its routes does.
pub fn serve(app: Router, files: Files) -> Router {
	app.fallback(move |req: Request| page(files.clone(), req))
}

fn reserved(path: &str) -> bool {
	RESERVED.iter().copied().chain(crate::forward::prefixes()).any(|r| crate::forward::under(path, r))
}

/// The page the dashboard's own views load from, when a link to one of them is opened.
const DASHBOARD: &str = "/review_archive";

/// A view of the dashboard: below its page, and not a file (the export's payloads have a dot).
fn dashboard_view(path: &str) -> bool {
	path.strip_prefix(DASHBOARD)
		.is_some_and(|rest| rest.starts_with('/') && rest.len() > 1 && !rest.rsplit('/').next().is_some_and(|last| last.contains('.')))
}

async fn page(mut files: Files, mut req: Request) -> Response {
	if reserved(req.uri().path()) {
		return dressed((StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(), false, &files.csp);
	}
	if dashboard_view(req.uri().path()) {
		*req.uri_mut() = axum::http::Uri::from_static("/review_archive/");
	}
	let path = req.uri().path();
	let hashed = path.starts_with(HASHED);
	// Every file in the image has the Nix store's mtime, 1970: answering If-Modified-Since
	// from it would keep a browser on the previous release's HTML, whose chunks are gone.
	// So nothing is revalidated by date — the HTML is small, and the rest is hashed.
	for h in [header::IF_MODIFIED_SINCE, header::IF_UNMODIFIED_SINCE] {
		req.headers_mut().remove(h);
	}
	let res = match files.dir.try_call(req).await {
		Ok(res) => res.map(Body::new),
		Err(e) => return failed(&e, &files.csp),
	};
	dressed(res, hashed, &files.csp)
}

fn failed(e: &io::Error, csp: &HeaderValue) -> Response {
	crate::report(&eyre::eyre!("{e}"), "serving the front end");
	dressed(StatusCode::INTERNAL_SERVER_ERROR.into_response(), false, csp)
}

fn dressed(mut res: Response, hashed: bool, csp: &HeaderValue) -> Response {
	let ok = res.status().is_success();
	let h = res.headers_mut();
	h.remove(header::LAST_MODIFIED);
	h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
	h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
	let html = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("text/html"));
	let cache = if hashed && ok && !html { "public, max-age=31536000, immutable" } else { "no-cache" };
	h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
	if html {
		h.insert(header::CONTENT_SECURITY_POLICY, csp.clone());
		h.insert(header::CONTENT_SECURITY_POLICY_REPORT_ONLY, HeaderValue::from_static(TRUSTED_TYPES_REPORT_ONLY));
	}
	res
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reserved_prefixes_are_whole_segments() {
		for p in ["/api", "/api/", "/api/v1/nope", "/auth/x", "/health", "/grafana", "/grafana/d/1", "/cabinet", "/cabinet/mfe/x.js"] {
			assert!(reserved(p), "{p}");
		}
		for p in [
			"/",
			"/apiary/",
			"/authors",
			"/healthy",
			"/cabinets",
			"/leads/",
			"/_next/static/a.js",
			"/review_archive",
			"/review_archive/gmails/3",
		] {
			assert!(!reserved(p), "{p}");
		}
		for p in ["/review_archive/mfe/x.js", "/playbook_mcp/token", "/.well-known/oauth-protected-resource/playbook_mcp"] {
			assert!(reserved(p), "{p}");
		}
	}

	#[test]
	fn the_dashboards_views_load_its_page() {
		for p in ["/review_archive/gmails/3", "/review_archive/members/1/tokens", "/review_archive/telegram"] {
			assert!(dashboard_view(p), "{p}");
		}
		for p in ["/review_archive", "/review_archive/", "/review_archive/index.txt", "/review_archived/x"] {
			assert!(!dashboard_view(p), "{p}");
		}
	}
}

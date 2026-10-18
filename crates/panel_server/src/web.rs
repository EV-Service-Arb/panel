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

use std::{io, path::Path};

use axum::{
	Json, Router,
	body::Body,
	extract::Request,
	http::{HeaderValue, StatusCode, header},
	response::{IntoResponse, Response},
};
use serde_json::json;
use tower_http::{
	services::{ServeDir, ServeFile},
	set_status::SetStatus,
};

/// Next inlines its bootstrap and the RSC payload as `<script>` elements, and the kit sets
/// inline styles, hence `'unsafe-inline'`; review_archive's dashboard is wasm, hence
/// `'wasm-unsafe-eval'`; nothing is loaded from another origin (the fonts
/// are the system's, the kit's CSS is bundled), and the pages talk to this origin alone. The
/// cabinet's account chip is on this origin too (`/cabinet/mfe`, Forward), its script and its
/// stylesheet alike, and loads no font.
///
/// This is the policy with `PANEL_CSP_PLATFORM_MFE` off, the default; [`csp`] builds the other.
pub const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// The platform's origin, where the hosts load its micro-frontends from.
const PLATFORM_ORIGIN: &str = "https://evinvest.ltd";

/// Cloudflare's Turnstile: the sign-in element loads its `api.js` and draws the challenge in
/// an iframe, both from this origin.
const TURNSTILE_ORIGIN: &str = "https://challenges.cloudflare.com";

/// The policy for the pages: [`CSP`], and with `platform_mfe` also the platform's MFE
/// directories (concierge's `/api/auth/mfe/`, the wallet's `/wallet/mfe/`) in `script-src`
/// and its origin in `connect-src` (the elements call its API, cross-origin), plus Turnstile's
/// origin in `script-src` and a `frame-src` of it alone (without one the frame would fall back
/// to `default-src 'self'` and the sign-in dialog would show "Captcha failed to load").
/// Turnstile needs no `connect-src` entry, as in the invest and admin policies.
///
/// Directories, never the origin: a `script-src` of the whole origin would let any script
/// the platform serves run in the panel's session, which is the very thing the pins on
/// `/auth/mfe` and `/cabinet/mfe` rule out. Those pins and routes stay the fallback until the
/// platform's bundles are proven in production; a separate change removes them. Nothing else
/// widens: styles, images and fonts stay on this origin.
pub fn csp(platform_mfe: bool) -> String {
	if !platform_mfe {
		return CSP.to_owned();
	}
	let widened = CSP
		.replace(
			"'wasm-unsafe-eval';",
			&format!("'wasm-unsafe-eval' {PLATFORM_ORIGIN}/api/auth/mfe/ {PLATFORM_ORIGIN}/wallet/mfe/ {TURNSTILE_ORIGIN};"),
		)
		.replace("connect-src 'self';", &format!("connect-src 'self' {PLATFORM_ORIGIN}; frame-src {TURNSTILE_ORIGIN};"));
	debug_assert_ne!(widened, CSP, "the base policy changed shape; update the two anchors");
	widened
}

/// Where the build puts what it names by content hash.
const HASHED: &str = "/_next/static/";

/// Paths that belong to the API side, whether or not a route of theirs is mounted: an unknown
/// one is a JSON 404, never the front end's page. `/grafana` is held for the dashboards;
/// `/cabinet` is the cabinet's (Forward), whole, mounted or not: a missing bundle file must
/// not come back as the panel's HTML page.
const RESERVED: [&str; 5] = ["/api", "/auth", "/health", "/grafana", "/cabinet"];

/// The export's files.
#[derive(Clone, Debug)]
pub struct Files {
	dir: ServeDir<SetStatus<ServeFile>>,
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
		let dir = ServeDir::new(dir).append_index_html_on_directories(true).not_found_service(ServeFile::new(dir.join("404.html")));
		Ok(Self {
			dir,
			csp: HeaderValue::from_static(CSP),
		})
	}

	/// The pages' policy with or without the platform's MFE directories ([`csp`]).
	#[must_use]
	pub fn platform_mfe(mut self, on: bool) -> Self {
		self.csp = HeaderValue::from_str(&csp(on)).expect("the policy is ASCII without control characters");
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
	}
	res
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The directive's sources, split on spaces.
	fn sources<'a>(policy: &'a str, directive: &str) -> Vec<&'a str> {
		let d = policy.split("; ").find(|d| d.split(' ').next() == Some(directive)).unwrap();
		d.split(' ').skip(1).collect()
	}

	#[test]
	fn the_policy_is_the_old_one_byte_for_byte_when_off() {
		assert_eq!(csp(false), CSP);
	}

	#[test]
	fn the_platform_widens_script_connect_and_frame_sources_only() {
		let on = csp(true);
		let off = CSP;
		assert_eq!(
			sources(&on, "script-src"),
			[
				"'self'",
				"'unsafe-inline'",
				"'wasm-unsafe-eval'",
				"https://evinvest.ltd/api/auth/mfe/",
				"https://evinvest.ltd/wallet/mfe/",
				"https://challenges.cloudflare.com"
			]
		);
		assert_eq!(sources(&on, "frame-src"), ["https://challenges.cloudflare.com"]);
		assert_eq!(sources(&on, "connect-src"), ["'self'", "https://evinvest.ltd"]);
		assert!(!sources(&on, "script-src").contains(&"https://evinvest.ltd"), "the bare origin is never a script source");
		// every other directive, in the same order, is untouched
		let rest = |p: &str| {
			p.split("; ")
				.filter(|d| !d.starts_with("script-src") && !d.starts_with("connect-src") && !d.starts_with("frame-src"))
				.map(str::to_owned)
				.collect::<Vec<_>>()
		};
		assert_eq!(rest(&on), rest(off));
		assert_eq!(on.split("; ").count(), off.split("; ").count() + 1);
		HeaderValue::from_str(&on).unwrap();
	}

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

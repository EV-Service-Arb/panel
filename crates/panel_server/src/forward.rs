//! The services behind the panel's origin (docs/ARCHITECTURE.md, Forward): a prefix of ours
//! is streamed to a service both ways, and who is calling is told to it by an assertion the
//! panel signs for that one request ([`sa_auth`]), never by the browser's cookies.
//!
//! ```text
//! /api/review_archive/*            → review_archive /*          session + assertion; CSRF on writes
//! /review_archive/mfe/*            → review_archive /mfe/*      open: the dashboard's bundle
//! /playbook_mcp/authorize          → playbook (same path)       session + assertion; its consent form
//!                                                               POST carries playbook's own nonce
//! /playbook_mcp/*, two .well-known → playbook (same path)       open: OAuth and bearer, no cookie
//! /cabinet/mfe/<file>.{js,css}     → cabinet (same path)        open, GET/HEAD: the account chip,
//!                                                               each file pinned by its SHA-256
//! ```
//!
//! The cabinet is no `sa_auth` service and is told nobody, so it is not one of [`ROUTES`]
//! but a [`Cabinet`] of its own, mounted without the sign-in.

use std::time::Duration;

use axum::{
	Json, Router,
	body::Body,
	error_handling::HandleErrorLayer,
	extract::{Request, State},
	http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
	response::{IntoResponse, Redirect, Response},
	routing::{any, get},
};
use http_body_util::{BodyExt, Limited};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use sa_auth::{Assertion, Service, Signer};
use serde_json::json;
use sha2::{Digest, Sha256};
use tower::{ServiceBuilder, limit::GlobalConcurrencyLimitLayer};
use tower_http::timeout::TimeoutLayer;

use crate::{
	cookies,
	signin::{self, Denied, Freshness, SignIn},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Gate {
	/// Signed in, and the assertion added.
	Session {
		csrf: bool,
	},
	Open,
}

struct Route {
	/// Ours, a whole path segment or more.
	prefix: &'static str,
	/// What replaces `prefix` upstream.
	upstream: &'static str,
	service: Service,
	gate: Gate,
}

/// Longest prefixes first: the first match wins.
const ROUTES: [Route; 6] = [
	Route {
		prefix: "/api/review_archive",
		upstream: "",
		service: Service::ReviewArchive,
		gate: Gate::Session { csrf: true },
	},
	Route {
		prefix: "/review_archive/mfe",
		upstream: "/mfe",
		service: Service::ReviewArchive,
		gate: Gate::Open,
	},
	Route {
		prefix: "/playbook_mcp/authorize",
		upstream: "/playbook_mcp/authorize",
		service: Service::Playbook,
		gate: Gate::Session { csrf: false },
	},
	Route {
		prefix: "/playbook_mcp",
		upstream: "/playbook_mcp",
		service: Service::Playbook,
		gate: Gate::Open,
	},
	Route {
		prefix: "/.well-known/oauth-authorization-server/playbook_mcp",
		upstream: "/.well-known/oauth-authorization-server/playbook_mcp",
		service: Service::Playbook,
		gate: Gate::Open,
	},
	Route {
		prefix: "/.well-known/oauth-protected-resource/playbook_mcp",
		upstream: "/.well-known/oauth-protected-resource/playbook_mcp",
		service: Service::Playbook,
		gate: Gate::Open,
	},
];

/// Every path the forward owns: never the front end's.
pub fn prefixes() -> impl Iterator<Item = &'static str> {
	ROUTES.iter().map(|r| r.prefix)
}

/// Whether `path` is `prefix` or below it, by whole segments.
pub fn under(path: &str, prefix: &str) -> bool {
	path.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Hop-by-hop headers (RFC 9110 §7.6.1) and what only the panel may say: never passed on.
const DROPPED_UP: [&str; 10] = [
	"connection",
	"keep-alive",
	"proxy-connection",
	"te",
	"trailer",
	"transfer-encoding",
	"upgrade",
	"cookie",
	sa_auth::HEADER,
	cookies::CSRF_HEADER,
];
const DROPPED_DOWN: [&str; 8] = ["connection", "keep-alive", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "set-cookie"];

/// Where each service is, and the key the panel vouches with.
pub struct Upstreams {
	pub review_archive: Uri,
	pub playbook: Uri,
	pub signer: Signer,
}

#[derive(Clone)]
pub struct Forward {
	sign_in: SignIn,
	upstreams: std::sync::Arc<Upstreams>,
	client: Client<HttpConnector, Body>,
}

impl Forward {
	pub fn new(sign_in: SignIn, upstreams: Upstreams) -> Self {
		let mut connector = HttpConnector::new();
		connector.set_connect_timeout(Some(Duration::from_secs(5)));
		Self {
			sign_in,
			upstreams: std::sync::Arc::new(upstreams),
			client: Client::builder(hyper_util::rt::TokioExecutor::new()).build(connector),
		}
	}

	pub fn routes(self) -> Router {
		let mut router = Router::new();
		for r in &ROUTES {
			router = router.route(r.prefix, any(forward)).route(&format!("{}/{{*rest}}", r.prefix), any(forward));
		}
		router.with_state(self)
	}
}

fn json_error(status: StatusCode, msg: &str) -> Response {
	(status, Json(json!({ "error": msg }))).into_response()
}

async fn forward(State(f): State<Forward>, req: Request) -> Response {
	let path = req.uri().path().to_owned();
	let route = ROUTES.iter().find(|r| under(&path, r.prefix)).expect("mounted under one of ROUTES");
	let upstream_path = format!("{}{}", route.upstream, &path[route.prefix.len()..]);
	let upstream_path = if upstream_path.is_empty() { "/".to_owned() } else { upstream_path };
	let assertion = match route.gate {
		Gate::Open => None,
		Gate::Session { csrf } => {
			let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
			if csrf && !safe && !f.sign_in.cookies().csrf_ok(req.headers()) {
				return json_error(StatusCode::FORBIDDEN, "csrf");
			}
			// a write must not outlive a revoked grant by even the cached GetMe's minute
			let freshness = if safe { Freshness::Cached } else { Freshness::Fresh };
			match signin::check(&f.sign_in, req.headers(), freshness).await {
				Ok(caller) => Some(Assertion {
					aud: route.service,
					sub: caller.user_id.to_string(),
					email: caller.email.clone(),
					email_verified: caller.email_verified,
					name: caller.preferred_name.clone(),
					permissions: caller.permissions.iter().filter(|p| p.starts_with(route.service.prefix())).collect(),
					method: req.method().to_string(),
					path: upstream_path.clone(),
					exp: jiff::Timestamp::now().as_second() + sa_auth::TTL,
				}),
				// a page the browser navigated to goes to sign in and comes back; a call is told
				Err(Denied::Unauthenticated) if route.service == Service::Playbook && *req.method() == Method::GET => {
					let back = req.uri().path_and_query().map_or(path.as_str(), |pq| pq.as_str());
					return Redirect::to(&format!("/auth/login?return_to={}", signin::encode(back))).into_response();
				}
				Err(denied) => return denied.into_response(&f.sign_in),
			}
		}
	};
	let base = match route.service {
		Service::ReviewArchive => &f.upstreams.review_archive,
		Service::Playbook => &f.upstreams.playbook,
	};
	let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
	let uri: Uri = match format!("{}{upstream_path}{query}", base.to_string().trim_end_matches('/')).parse() {
		Ok(u) => u,
		Err(_) => return json_error(StatusCode::BAD_REQUEST, "not a forwardable path"),
	};
	let (mut parts, body) = req.into_parts();
	parts.uri = uri;
	strip(&mut parts.headers, &DROPPED_UP);
	parts.headers.remove(header::HOST);
	if let Some(a) = assertion {
		let token = f.upstreams.signer.sign(&a);
		parts
			.headers
			.insert(HeaderName::from_static(sa_auth::HEADER), HeaderValue::from_str(&token).expect("a JWS is base64url and dots"));
	}
	match f.client.request(Request::from_parts(parts, body)).await {
		Ok(res) => {
			let (mut parts, body) = res.into_parts();
			strip(&mut parts.headers, &DROPPED_DOWN);
			Response::from_parts(parts, Body::new(body))
		}
		Err(e) => {
			tracing::warn!(error = %e, service = ?route.service, "a forward failed");
			json_error(StatusCode::BAD_GATEWAY, "the service did not answer")
		}
	}
}

fn strip(headers: &mut HeaderMap, names: &[&str]) {
	// a header `Connection` names is hop-by-hop too
	let named: Vec<String> = headers
		.get_all(header::CONNECTION)
		.iter()
		.filter_map(|v| v.to_str().ok())
		.flat_map(|v| v.split(','))
		.map(|n| n.trim().to_ascii_lowercase())
		.filter(|n| !n.is_empty())
		.collect();
	for n in names.iter().copied().map(str::to_owned).chain(named) {
		headers.remove(n.as_str());
	}
}

/// The cabinet's element bundles, the account chip among them: served from the panel's origin
/// so the page's `script-src 'self'` admits them. The cabinet answers the same path (its
/// `next.config.ts` rewrites it onto its `public/mfe/`).
pub const CABINET_MFE: &str = "/cabinet/mfe";

/// The files handed out, by extension, and the types each must come back as. What the
/// cabinet's esbuild writes (`cabinet/frontend/mfe/build.mjs`): one `.js`, one `.css`. No
/// `.map`: the build emits none, and one would publish the cabinet's sources here for nothing
/// the page needs. A type check, because a `.js` answered with Next's HTML page must not run.
const BUNDLE: [(&str, &[&str]); 2] = [("js", &["text/javascript", "application/javascript"]), ("css", &["text/css"])];

/// A file has a whole answer by then, or the browser gets a 502: the chip is a few dozen
/// KiB, and a hung cabinet must not hold the panel's connections.
const CABINET_TIMEOUT: Duration = Duration::from_secs(10);
/// Files served at once, past it a 503: the neighbours' budget (the API's 32).
const CABINET_CONCURRENT: usize = 32;
/// The route's own bound, behind [`CABINET_TIMEOUT`] so that the fetch's 502 (and its log
/// line) is what a slow cabinet gets; this one catches whatever else could hang.
const CABINET_ROUTE_TIMEOUT: Duration = Duration::from_secs(15);
/// The browser keeps a file this long: a new release reaches the pages within five minutes.
/// Not the cabinet's own (`stale-while-revalidate`): a file kept past the pin's change would
/// keep bytes the pin no longer vouches for: how long a checked file lives is the panel's say.
const CABINET_CACHE: &str = "public, max-age=300";
/// The answer is buffered (its type is checked before anything goes down), so it is bounded.
const CABINET_MAX_BYTES: usize = 4 << 20;

/// An allow-list both ways, not [`DROPPED_UP`]'s deny-list: a static file needs nothing of
/// the caller's but these, so the session cookie, the CSRF header and whatever comes next
/// cannot go up by omission; and nothing comes down but what describes the file. Not
/// `Cache-Control`: [`CABINET_CACHE`] is said instead. Not `Accept-Encoding` either: a pin is
/// of the file as built, so the cabinet is asked for it unencoded, and an encoded answer is
/// refused rather than unpacked.
const CABINET_UP: [HeaderName; 3] = [header::ACCEPT, header::IF_NONE_MATCH, header::IF_MODIFIED_SINCE];
const CABINET_DOWN: [HeaderName; 4] = [header::CONTENT_TYPE, header::ETAG, header::LAST_MODIFIED, header::VARY];

/// The SHA-256 each file of the bundle must have (`PANEL_CABINET_MFE_PINS`), by its name under
/// `/cabinet/mfe/`. The bundle runs on the panel's origin with the panel's session, so the
/// bytes are the panel's own code: the cabinet answering is not enough, its release must have
/// been named here. A file without a pin is not served.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CabinetPins(std::collections::BTreeMap<String, [u8; 32]>);

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum BadPins {
	#[error("no pin in it: want `account-chip.js=<sha256 hex>,account-chip.css=<sha256 hex>`")]
	Empty,
	#[error("`{0}` is not `<file>=<sha256 hex>`")]
	Shape(String),
	#[error("`{0}` is no file of the bundle (a plain name ending in .js or .css)")]
	File(String),
	#[error("the pin of `{0}` is not 64 lowercase hex digits")]
	Digest(String),
	#[error("`{0}` is pinned twice")]
	Twice(String),
}

impl CabinetPins {
	/// `account-chip.js=<hex>,account-chip.css=<hex>`, spaces around either side allowed.
	pub fn parse(raw: &str) -> Result<Self, BadPins> {
		let mut pins = std::collections::BTreeMap::new();
		for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
			let (file, digest) = entry.split_once('=').ok_or_else(|| BadPins::Shape(entry.to_owned()))?;
			let (file, digest) = (file.trim(), digest.trim());
			if bundle_types(file).is_none() {
				return Err(BadPins::File(file.to_owned()));
			}
			let digest = decode_digest(digest).ok_or_else(|| BadPins::Digest(file.to_owned()))?;
			if pins.insert(file.to_owned(), digest).is_some() {
				return Err(BadPins::Twice(file.to_owned()));
			}
		}
		if pins.is_empty() {
			return Err(BadPins::Empty);
		}
		Ok(Self(pins))
	}

	fn get(&self, file: &str) -> Option<&[u8; 32]> {
		self.0.get(file)
	}
}

/// Lowercase only: what `sha256sum` prints, so a pin has one spelling.
fn decode_digest(hex: &str) -> Option<[u8; 32]> {
	let nibble = |b: u8| match b {
		b'0'..=b'9' => Some(b - b'0'),
		b'a'..=b'f' => Some(b - b'a' + 10),
		_ => None,
	};
	let bytes = hex.as_bytes();
	if bytes.len() != 64 {
		return None;
	}
	let mut out = [0u8; 32];
	for (o, [hi, lo]) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
		*o = nibble(*hi)? << 4 | nibble(*lo)?;
	}
	Some(out)
}

fn encode_digest(digest: &[u8]) -> String {
	digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The cabinet behind `/cabinet/mfe` (`PANEL_CABINET_URL`).
#[derive(Clone)]
pub struct Cabinet {
	/// A bare `http://` origin, without the trailing slash.
	base: std::sync::Arc<str>,
	pins: std::sync::Arc<CabinetPins>,
	client: Client<HttpConnector, Body>,
}

impl Cabinet {
	pub fn new(base: &Uri, pins: CabinetPins) -> Self {
		let mut connector = HttpConnector::new();
		connector.set_connect_timeout(Some(Duration::from_secs(5)));
		Self {
			base: base.to_string().trim_end_matches('/').into(),
			pins: std::sync::Arc::new(pins),
			client: Client::builder(hyper_util::rt::TokioExecutor::new()).build(connector),
		}
	}

	/// The cabinet if both its address and its pins are given, else the names of what is
	/// missing. Fail closed: the cabinet's code does not run on this origin unpinned.
	pub fn configured(url: Option<Uri>, pins: Option<CabinetPins>) -> Result<Self, Vec<&'static str>> {
		match (url, pins) {
			(Some(url), Some(pins)) => Ok(Self::new(&url, pins)),
			(url, pins) => Err([("PANEL_CABINET_URL", url.is_none()), ("PANEL_CABINET_MFE_PINS", pins.is_none())]
				.into_iter()
				.filter_map(|(name, unset)| unset.then_some(name))
				.collect()),
		}
	}

	/// `GET` and `HEAD` of `/cabinet/mfe/*`; any other method is a 405. At most
	/// [`CABINET_CONCURRENT`] at once, past it a 503 rather than a queue.
	pub fn routes(self) -> Router {
		let layers = ServiceBuilder::new()
			// The only error the layers below raise is the shed: everything else is a response.
			.layer(HandleErrorLayer::new(|_: tower::BoxError| async { json_error(StatusCode::SERVICE_UNAVAILABLE, "busy, try again") }))
			.load_shed()
			.layer(GlobalConcurrencyLimitLayer::with_semaphore(std::sync::Arc::new(tokio::sync::Semaphore::new(CABINET_CONCURRENT))))
			.layer(TimeoutLayer::with_status_code(StatusCode::SERVICE_UNAVAILABLE, CABINET_ROUTE_TIMEOUT));
		Router::new().route(&format!("{CABINET_MFE}/{{*file}}"), get(cabinet_mfe).layer(layers)).with_state(self)
	}
}

/// The types `file` may come back as, if it is a name the bundle could have: segments of
/// `[A-Za-z0-9._-]`, none empty and none starting with a dot (no `.`, `..` or hidden file),
/// no `%` at all (nothing encoded, once or twice: no file of the bundle needs it), and an
/// extension of [`BUNDLE`].
fn bundle_types(file: &str) -> Option<&'static [&'static str]> {
	let named = |s: &str| !s.is_empty() && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
	if !file.split('/').all(named) {
		return None;
	}
	let last = file.rsplit('/').next()?;
	let (_, ext) = last.rsplit_once('.')?;
	BUNDLE.iter().find(|(e, _)| *e == ext).map(|(_, types)| *types)
}

/// Why the cabinet gave no file: said in the log, never to the browser (it names the address).
#[derive(Debug, thiserror::Error)]
enum Unanswered {
	#[error("no answer within {CABINET_TIMEOUT:?}")]
	Timeout,
	#[error("{0}")]
	Request(#[from] hyper_util::client::legacy::Error),
	#[error("reading the body (over {CABINET_MAX_BYTES} bytes?): {0}")]
	Body(tower::BoxError),
	#[error("status {0}")]
	Status(StatusCode),
	#[error("type {0:?} for a file of the bundle")]
	Type(Option<HeaderValue>),
	#[error("encoded {0:?}, though asked for none")]
	Encoded(HeaderValue),
	#[error("sha256 {got} is not the pinned one")]
	Pin { got: String },
}

fn not_found() -> Response {
	json_error(StatusCode::NOT_FOUND, "not found")
}

async fn cabinet_mfe(State(c): State<Cabinet>, req: Request) -> Response {
	let path = req.uri().path();
	let Some(file) = path.strip_prefix(CABINET_MFE).and_then(|rest| rest.strip_prefix('/')) else {
		return not_found();
	};
	// Before the cabinet is asked: an unpinned file is no file of ours.
	let (Some(types), Some(pin)) = (bundle_types(file), c.pins.get(file)) else {
		return not_found();
	};
	// The query is dropped: a static file has no use for one.
	let Ok(uri) = format!("{}{path}", c.base).parse::<Uri>() else {
		return not_found();
	};
	let mut up = axum::http::Request::get(uri);
	for name in &CABINET_UP {
		for v in req.headers().get_all(name) {
			up = up.header(name, v);
		}
	}
	// Built from a parsed URI and headers copied from a request: it cannot fail.
	let up = up.body(Body::empty()).expect("a GET of a parsed URI");
	match fetch(&c, up, types, pin).await {
		Ok(res) => res,
		Err(Unanswered::Status(StatusCode::NOT_FOUND)) => not_found(),
		Err(e) => {
			tracing::warn!(error = %e, file, "the cabinet gave no mfe file");
			json_error(StatusCode::BAD_GATEWAY, "the cabinet did not answer")
		}
	}
}

/// The file, whole, of its type and of its pin, or why not. A redirect is not followed and
/// not passed down: its `Location` would be the cabinet's address inside the cluster. A 304
/// passes unchecked: it carries no bytes, and the browser's copy came from here, checked.
async fn fetch(c: &Cabinet, up: Request, types: &[&str], pin: &[u8; 32]) -> Result<Response, Unanswered> {
	let (parts, body) = tokio::time::timeout(CABINET_TIMEOUT, async {
		let (parts, body) = c.client.request(up).await?.into_parts();
		let body = Limited::new(body, CABINET_MAX_BYTES).collect().await.map_err(Unanswered::Body)?.to_bytes();
		Ok::<_, Unanswered>((parts, body))
	})
	.await
	.map_err(|_elapsed| Unanswered::Timeout)??;
	match parts.status {
		StatusCode::OK => {
			let ct = parts.headers.get(header::CONTENT_TYPE);
			let essence = ct.and_then(|v| v.to_str().ok()).map(|v| v.split(';').next().unwrap_or_default().trim().to_ascii_lowercase());
			if !essence.is_some_and(|e| types.contains(&e.as_str())) {
				return Err(Unanswered::Type(ct.cloned()));
			}
			if let Some(enc) = parts.headers.get(header::CONTENT_ENCODING).filter(|v| *v != "identity") {
				return Err(Unanswered::Encoded(enc.clone()));
			}
			let got = Sha256::digest(&body);
			if got.as_slice() != pin.as_slice() {
				return Err(Unanswered::Pin { got: encode_digest(&got) });
			}
		}
		StatusCode::NOT_MODIFIED => {}
		other => return Err(Unanswered::Status(other)),
	}
	let mut res = Response::new(Body::from(body));
	*res.status_mut() = parts.status;
	let h = res.headers_mut();
	for name in &CABINET_DOWN {
		for v in parts.headers.get_all(name) {
			h.append(name, v.clone());
		}
	}
	h.insert(header::CACHE_CONTROL, HeaderValue::from_static(CABINET_CACHE));
	h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
	Ok(res)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_the_bundles_plain_file_names_pass() {
		assert_eq!(bundle_types("account-chip.js"), Some(BUNDLE[0].1));
		assert_eq!(bundle_types("account-chip.css"), Some(BUNDLE[1].1));
		assert_eq!(bundle_types("chunks/a_1.min.js"), Some(BUNDLE[0].1));
		for f in [
			"",
			"account-chip.js.map",
			"account-chip",
			"index.html",
			"a.b/c",
			"../x.js",
			"a/../x.js",
			"./x.js",
			".x.js",
			"a//x.js",
			"a/x.js/",
			"%2e%2e/x.js",
			"..%2fx.js",
			"%252e%252e/x.js",
			"a%2fx.js",
			"x.js%00",
			"a\\x.js",
			"x .js",
		] {
			assert_eq!(bundle_types(f), None, "{f:?}");
		}
	}

	#[test]
	fn pins_are_file_names_with_lowercase_sha256_hex() {
		let (js, css) = ("0f".repeat(32), "a1".repeat(32));
		let pins = CabinetPins::parse(&format!(" account-chip.js = {js} ,account-chip.css={css}, ")).unwrap();
		assert_eq!(pins.get("account-chip.js"), Some(&[0x0f; 32]));
		assert_eq!(pins.get("account-chip.css"), Some(&[0xa1; 32]));
		assert_eq!(pins.get("other.js"), None);
		assert_eq!(encode_digest(&[0xa1; 32]), css);

		for (raw, want) in [
			("", BadPins::Empty),
			(" , ", BadPins::Empty),
			(&*format!("account-chip.js {js}"), BadPins::Shape(format!("account-chip.js {js}"))),
			(&*format!("account-chip.js.map={js}"), BadPins::File("account-chip.js.map".into())),
			(&*format!("../x.js={js}"), BadPins::File("../x.js".into())),
			(&*format!("={js}"), BadPins::File(String::new())),
			(&*format!("a.js={}", js.to_uppercase()), BadPins::Digest("a.js".into())),
			(&*format!("a.js={}", &js[1..]), BadPins::Digest("a.js".into())),
			(&*format!("a.js={js}0"), BadPins::Digest("a.js".into())),
			("a.js=zz", BadPins::Digest("a.js".into())),
			(&*format!("a.js={js},a.js={css}"), BadPins::Twice("a.js".into())),
		] {
			assert_eq!(CabinetPins::parse(raw), Err(want), "{raw:?}");
		}
	}

	#[test]
	fn the_first_route_matching_by_whole_segments_wins() {
		let route = |p: &str| ROUTES.iter().find(|r| under(p, r.prefix)).map(|r| (r.prefix, r.gate));
		assert_eq!(route("/playbook_mcp/authorize"), Some(("/playbook_mcp/authorize", Gate::Session { csrf: false })));
		assert_eq!(route("/playbook_mcp/token"), Some(("/playbook_mcp", Gate::Open)));
		assert_eq!(route("/playbook_mcp/authorizer"), Some(("/playbook_mcp", Gate::Open)));
		assert_eq!(route("/api/review_archive/me"), Some(("/api/review_archive", Gate::Session { csrf: true })));
		assert_eq!(route("/api/review_archived"), None);
		assert_eq!(route("/review_archive"), None, "the page is the front end's");
	}
}

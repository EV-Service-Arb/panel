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
//! ```

use std::time::Duration;

use axum::{
	Json, Router,
	body::Body,
	extract::{Request, State},
	http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
	response::{IntoResponse, Redirect, Response},
	routing::any,
};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use sa_auth::{Assertion, Service, Signer};
use serde_json::json;

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

#[cfg(test)]
mod tests {
	use super::*;

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

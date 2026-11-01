//! The services behind the conductor's origin (docs/ARCHITECTURE.md, Forward): a prefix of ours
//! is streamed to a service both ways, and who is calling is told to it by an assertion the
//! conductor signs for that one request ([`sa_auth`]), never by the browser's cookies.
//!
//! ```text
//! /api/sites/*                     → sites /api/v1/*            session + assertion; CSRF on writes
//! /api/hooks/booking/*             → sites (same path)          open: the providers' webhooks
//! /sites/mfe/*                     → sites /mfe/*               open: the sites page's bundle
//! /api/review_archive/grafana/*    → review_archive /grafana/*  session + assertion; a POST is a read
//! /api/review_archive/*            → review_archive /*          session + assertion; CSRF on writes
//! /review_archive/mfe/*            → review_archive /mfe/*      open: the dashboard's bundle
//! /playbook_mcp/authorize          → playbook (same path)       session + assertion; its consent form
//!                                                               POST carries playbook's own nonce
//! /playbook_mcp/*, two .well-known → playbook (same path)       open: OAuth and bearer, no cookie
//! ```
//!
//! [`Services`] is the one client and signing key every call to a service goes through: the
//! forward's, ingest's, Grafana's ([`crate::grafana`]), and the hubs' own reads of sites.

use std::{sync::Arc, time::Duration};

use axum::{
	Json, Router,
	body::{Body, to_bytes},
	extract::{Request, State},
	http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
	response::{IntoResponse, Redirect, Response},
	routing::any,
};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use sa_auth::{Assertion, Principal, Service, Signer, User};
use serde_json::json;

use crate::{
	cookies,
	signin::{self, Caller, Denied, Freshness, SignIn},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Gate {
	/// Signed in, and the assertion added.
	Session(Writes),
	Open,
}

/// What a method but GET, HEAD and OPTIONS is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Writes {
	/// A write: the panel's CSRF header, and concierge asked afresh.
	Csrf,
	/// A write the service guards with a nonce of its own: concierge asked afresh.
	ServiceNonce,
	/// Not a write: Grafana's queries are POSTs.
	Reads,
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
const ROUTES: [Route; 10] = [
	Route {
		prefix: "/api/sites",
		upstream: "/api/v1",
		service: Service::Sites,
		gate: Gate::Session(Writes::Csrf),
	},
	Route {
		prefix: "/api/hooks/booking",
		upstream: "/api/hooks/booking",
		service: Service::Sites,
		gate: Gate::Open,
	},
	Route {
		prefix: "/sites/mfe",
		upstream: "/mfe",
		service: Service::Sites,
		gate: Gate::Open,
	},
	Route {
		prefix: "/api/review_archive/grafana",
		upstream: "/grafana",
		service: Service::ReviewArchive,
		gate: Gate::Session(Writes::Reads),
	},
	Route {
		prefix: "/api/review_archive",
		upstream: "",
		service: Service::ReviewArchive,
		gate: Gate::Session(Writes::Csrf),
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
		gate: Gate::Session(Writes::ServiceNonce),
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

/// The services that serve their Grafana dashboards at `GET /dashboards` (docs/ARCHITECTURE.md,
/// Grafana).
pub(crate) const DASHBOARDS: [Service; 1] = [Service::ReviewArchive];

/// Every path the forward owns: never the front end's.
pub fn prefixes() -> impl Iterator<Item = &'static str> {
	ROUTES.iter().map(|r| r.prefix)
}

/// Each service's API, `/api/<service>`, with its prefix: what a viewer's token reaches.
pub(crate) fn apis() -> impl Iterator<Item = (Service, &'static str)> {
	ROUTES.iter().filter(|r| r.prefix == api_prefix(r.service)).map(|r| (r.service, r.prefix))
}

fn api_prefix(service: Service) -> String {
	format!("/api/{}", name(service))
}

/// The service's serde name: its API's path segment, its Grafana datasource's uid.
pub(crate) fn name(service: Service) -> String {
	serde_json::to_value(service)
		.expect("a unit variant serializes")
		.as_str()
		.expect("a unit variant is a string")
		.to_owned()
}

/// Whether `path` is `prefix` or below it, by whole segments.
pub fn under(path: &str, prefix: &str) -> bool {
	path.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Hop-by-hop headers (RFC 9110 §7.6.1) and what only the panel may say: never passed on.
pub(crate) const DROPPED_UP: [&str; 11] = [
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
	crate::grafana::VIEWER_HEADER,
];
pub(crate) const DROPPED_DOWN: [&str; 8] = ["connection", "keep-alive", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade", "set-cookie"];

/// Where each service is, and the key the conductor vouches with.
pub struct Upstreams {
	pub review_archive: Uri,
	pub playbook: Uri,
	pub sites: Uri,
	pub signer: Signer,
}

/// The services, and how the conductor calls them.
#[derive(Clone)]
pub struct Services {
	upstreams: Arc<Upstreams>,
	client: Client<HttpConnector, Body>,
}

impl Services {
	pub fn new(upstreams: Upstreams) -> Self {
		let mut connector = HttpConnector::new();
		connector.set_connect_timeout(Some(Duration::from_secs(5)));
		Self {
			upstreams: Arc::new(upstreams),
			client: Client::builder(hyper_util::rt::TokioExecutor::new()).build(connector),
		}
	}

	/// `method path[?query]` to `service`, `principal` vouched for by an assertion bound to the
	/// path; what only the conductor may say, and hop-by-hop headers, are not passed either way.
	/// The answer streams.
	pub(crate) async fn send(
		&self,
		service: Service,
		method: Method,
		path_and_query: &str,
		mut headers: HeaderMap,
		principal: Option<Principal>,
		body: Body,
	) -> Result<Response, hyper_util::client::legacy::Error> {
		let path = path_and_query.split_once('?').map_or(path_and_query, |(path, _)| path);
		let base = match service {
			Service::ReviewArchive => &self.upstreams.review_archive,
			Service::Playbook => &self.upstreams.playbook,
			Service::Sites => &self.upstreams.sites,
		};
		let uri: Uri = format!("{}{path_and_query}", base.to_string().trim_end_matches('/'))
			.parse()
			.expect("a path and query that arrived in a URI stay one under a bare origin");
		strip(&mut headers, &DROPPED_UP);
		headers.remove(header::HOST);
		if let Some(principal) = principal {
			let token = self.upstreams.signer.sign(&Assertion {
				aud: service,
				principal,
				method: method.to_string(),
				path: path.to_owned(),
				exp: jiff::Timestamp::now().as_second() + sa_auth::TTL,
			});
			headers.insert(HeaderName::from_static(sa_auth::HEADER), HeaderValue::from_str(&token).expect("a JWS is base64url and dots"));
		}
		let mut req = Request::new(body);
		*req.method_mut() = method;
		*req.uri_mut() = uri;
		*req.headers_mut() = headers;
		let res = self.client.request(req).await?;
		let (mut parts, body) = res.into_parts();
		strip(&mut parts.headers, &DROPPED_DOWN);
		Ok(Response::from_parts(parts, Body::new(body)))
	}

	/// `GET path?query` of sites as the conductor, its JSON answer.
	pub(crate) async fn conductor_get<T: serde::de::DeserializeOwned>(&self, path: &str) -> eyre::Result<T> {
		let call = self.send(Service::Sites, Method::GET, path, HeaderMap::new(), Some(Principal::Conductor), Body::empty());
		let res = tokio::time::timeout(SITES_TIMEOUT, call)
			.await
			.map_err(|_| eyre::eyre!("sites did not answer {path} in time"))??;
		eyre::ensure!(res.status() == StatusCode::OK, "sites answered {path} with {}", res.status());
		let body = to_bytes(res.into_body(), SITES_ANSWER).await.map_err(|e| eyre::eyre!("reading sites' answer to {path}: {e}"))?;
		serde_json::from_slice(&body).map_err(|e| eyre::eyre!("sites' answer to {path}: {e}"))
	}
}

/// How long a call of the conductor's own to sites may take.
pub(crate) const SITES_TIMEOUT: Duration = Duration::from_secs(10);

/// The most of sites' answer the conductor reads: a page of notes, with room.
const SITES_ANSWER: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct Forward {
	sign_in: SignIn,
	services: Services,
}

impl Forward {
	pub fn new(sign_in: SignIn, services: Services) -> Self {
		Self { sign_in, services }
	}

	pub fn routes(self) -> Router {
		let mut router = Router::new();
		for r in &ROUTES {
			router = router.route(r.prefix, any(forward)).route(&format!("{}/{{*rest}}", r.prefix), any(forward));
		}
		router.with_state(self)
	}

	pub(crate) fn panel(&self) -> &panel::Panel {
		&self.sign_in.panel
	}

	pub(crate) fn sign_in(&self) -> &SignIn {
		&self.sign_in
	}

	pub(crate) fn services(&self) -> &Services {
		&self.services
	}
}

pub(crate) fn json_error(status: StatusCode, msg: &str) -> Response {
	(status, Json(json!({ "error": msg }))).into_response()
}

async fn forward(State(f): State<Forward>, req: Request) -> Response {
	let route = ROUTES.iter().find(|r| under(req.uri().path(), r.prefix)).expect("mounted under one of ROUTES");
	let principal = match route.gate {
		Gate::Open => None,
		Gate::Session(writes) => {
			let write = writes != Writes::Reads && !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
			if write && writes == Writes::Csrf && !f.sign_in.cookies().csrf_ok(req.headers()) {
				return json_error(StatusCode::FORBIDDEN, "csrf");
			}
			// a write must not outlive a revoked grant by even the cached GetMe's minute
			let freshness = if write { Freshness::Fresh } else { Freshness::Cached };
			match signin::check(&f.sign_in, req.headers(), freshness).await {
				Ok(caller) => Some(Principal::User(user(caller, route.service))),
				// a page the browser navigated to goes to sign in and comes back; a call is told
				Err(Denied::Unauthenticated) if route.service == Service::Playbook && *req.method() == Method::GET => {
					let back = req.uri().path_and_query().map_or(req.uri().path(), |pq| pq.as_str());
					return Redirect::to(&format!("/auth/login?return_to={}", signin::encode(back))).into_response();
				}
				Err(denied) => return denied.into_response(&f.sign_in),
			}
		}
	};
	f.pass(route, req, principal).await
}

/// `caller`, as `service` is told of them: its slice of their permissions only.
fn user(caller: Caller, service: Service) -> User {
	User {
		sub: caller.user_id.to_string(),
		email: caller.email,
		email_verified: caller.email_verified,
		name: caller.preferred_name,
		permissions: caller.permissions.iter().filter(|p| p.starts_with(service.prefix())).collect(),
	}
}

impl Forward {
	/// A GET Grafana makes for `viewer` to a service's API: the forward's, as that viewer.
	pub(crate) async fn as_viewer(&self, viewer: Caller, req: Request) -> Response {
		match apis().find(|(_, prefix)| under(req.uri().path(), prefix)) {
			Some((service, prefix)) => {
				let route = ROUTES.iter().find(|r| r.prefix == prefix).expect("apis() are ROUTES");
				self.pass(route, req, Some(Principal::User(user(viewer, service)))).await
			}
			None => json_error(StatusCode::NOT_FOUND, "not a service's API"),
		}
	}

	async fn pass(&self, route: &Route, req: Request, principal: Option<Principal>) -> Response {
		let rest = &req.uri().path()[route.prefix.len()..];
		// decoded as a service might, so no route reaches above its upstream prefix
		let decoded = rest.to_ascii_lowercase().replace("%2e", ".").replace("%2f", "/").replace("%5c", "/").replace('\\', "/");
		if decoded.split('/').any(|s| s == "." || s == "..") {
			return json_error(StatusCode::BAD_REQUEST, "not a forwardable path");
		}
		let upstream_path = format!("{}{rest}", route.upstream);
		let upstream_path = if upstream_path.is_empty() { "/".to_owned() } else { upstream_path };
		let upstream = match req.uri().query() {
			Some(q) => format!("{upstream_path}?{q}"),
			None => upstream_path,
		};
		let (parts, body) = req.into_parts();
		match self.services.send(route.service, parts.method, &upstream, parts.headers, principal, body).await {
			Ok(res) => res,
			Err(e) => {
				tracing::warn!(error = %e, service = ?route.service, "a forward failed");
				json_error(StatusCode::BAD_GATEWAY, "the service did not answer")
			}
		}
	}
}

pub(crate) fn strip(headers: &mut HeaderMap, names: &[&str]) {
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
		assert_eq!(route("/playbook_mcp/authorize"), Some(("/playbook_mcp/authorize", Gate::Session(Writes::ServiceNonce))));
		assert_eq!(route("/playbook_mcp/token"), Some(("/playbook_mcp", Gate::Open)));
		assert_eq!(route("/playbook_mcp/authorizer"), Some(("/playbook_mcp", Gate::Open)));
		assert_eq!(route("/api/review_archive/me"), Some(("/api/review_archive", Gate::Session(Writes::Csrf))));
		assert_eq!(
			route("/api/review_archive/grafana/api/ds/query"),
			Some(("/api/review_archive/grafana", Gate::Session(Writes::Reads)))
		);
		assert_eq!(route("/api/review_archive/grafanas"), Some(("/api/review_archive", Gate::Session(Writes::Csrf))));
		assert_eq!(route("/api/review_archived"), None);
		assert_eq!(route("/review_archive"), None, "the page is the front end's");
		assert_eq!(route("/api/sites/leads"), Some(("/api/sites", Gate::Session(Writes::Csrf))));
		assert_eq!(route("/sites/mfe/mfe-sites.js"), Some(("/sites/mfe", Gate::Open)));
		assert_eq!(route("/sites/leads"), None, "the page is the front end's");
	}
}

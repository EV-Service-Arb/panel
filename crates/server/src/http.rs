//! The HTTP surface: `/health`, the sign-in (`/auth/*`, [`crate::signin`]), the conductor's own
//! `/api/v1` ([`crate::api`], [`crate::access`], [`crate::gmbs`], [`crate::tokens`],
//! [`crate::alerts`]) and its live socket
//! (`/api/v1/live`, [`crate::live`]); with the forward configured, ingest (`ingest.rs`) and
//! the services behind the origin ([`crate::forward`]).

use std::{sync::Arc, time::Duration};

use axum::{
	Json, Router,
	error_handling::HandleErrorLayer,
	extract::FromRef,
	handler::Handler,
	http::{HeaderValue, Method, StatusCode, header},
	middleware,
	response::{IntoResponse, Response},
	routing::{MethodFilter, MethodRouter, get, on, post},
};
use sa_auth::{PermissionSet, Sources, Tokens};
use sa_panel::Panel;
use serde_json::json;
use tower::{BoxError, ServiceBuilder, limit::GlobalConcurrencyLimitLayer};
use tower_http::timeout::TimeoutLayer;

use crate::{
	access,
	alerts::{self, Telegram},
	api, connected,
	forward::{Forward, Services},
	gmbs,
	grafana::Grafana,
	ingest,
	live::{self, Live, LiveLimits},
	signin::{self, Freshness, Gate, SignIn},
	tokens,
};

/// A part of the conductor's API, and the permission that opens it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
	/// Sources and their keys.
	Admin,
	/// Everyone's tokens.
	Tokens,
}

impl Section {
	pub fn may(self, permissions: &PermissionSet) -> bool {
		match self {
			Self::Admin => permissions.may(Sources::Manage),
			Self::Tokens => permissions.may(Tokens::Grant),
		}
	}
}

/// One `/api/v1` route, and who passes its gate.
pub struct ApiRoute {
	pub method: Method,
	/// Under `/api/v1`.
	pub path: &'static str,
	/// `None`: every signed-in user.
	pub section: Option<Section>,
	freshness: Freshness,
	handler: MethodRouter<ApiState>,
}
impl ApiRoute {
	pub(crate) fn new<H: Handler<T, ApiState>, T: 'static>(method: Method, path: &'static str, section: Option<Section>, freshness: Freshness, handler: H) -> Self {
		let filter = MethodFilter::try_from(method.clone()).expect("the routes name standard methods");
		Self {
			method,
			path,
			section,
			freshness,
			handler: on(filter, handler),
		}
	}
}

/// Every `/api/v1` route on `ApiState`.
pub fn api_routes() -> Vec<ApiRoute> {
	[api::routes(), access::routes(), alerts::routes(), gmbs::routes(), tokens::routes()]
		.into_iter()
		.flatten()
		.collect()
}

/// What a request may cost.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
	/// Ingest requests served at once; past it, `503` rather than a queue.
	pub max_concurrent: usize,
	/// How long a body may take to arrive.
	pub body_timeout: Duration,
	/// How long a request may take in all: 500 short transactions, with room to spare.
	pub request_timeout: Duration,
	/// `/auth/*` requests at once: each may wait on concierge, so few.
	pub auth_concurrent: usize,
	/// `/auth/*`: a code exchange and a session write, concierge's 5 s included.
	pub auth_timeout: Duration,
	/// `/api/v1` requests at once.
	pub api_concurrent: usize,
	/// `/api/v1`: a rotation (which may wait on another replica's) and a few queries.
	pub api_timeout: Duration,
	/// `/api/v1/live`.
	pub live: LiveLimits,
}

/// Everything the conductor answers but the front end's files; ingest and the forward only with
/// `forward`, `/grafana/` only with `grafana`.
pub fn app(sign_in: SignIn, forward: Option<Forward>, grafana: Option<&Grafana>, limits: Limits, telegram: Option<Telegram>, map: Option<gmbs::MapDto>) -> Router {
	let auth = Router::new()
		.route("/auth/login", get(signin::login))
		.route("/auth/callback", get(signin::callback))
		.route("/auth/logout", post(signin::logout));
	let auth = bounded(auth, limits.auth_concurrent, limits.auth_timeout).with_state(sign_in.clone());
	let gate = |freshness, section| {
		let gate = Gate {
			sign_in: sign_in.clone(),
			freshness,
			section,
		};
		middleware::from_fn_with_state(gate, signin::gate)
	};
	let routes = api_routes().into_iter().fold(Router::new(), |routes, r| {
		routes.merge(Router::new().route(r.path, r.handler).route_layer(gate(r.freshness, r.section)))
	});
	let state = ApiState {
		panel: sign_in.panel.clone(),
		sites: forward.as_ref().map(|f| f.services().clone()),
		probe: connected::probe_client(),
		telegram,
		map,
	};
	let api = bounded(routes.with_state(state), limits.api_concurrent, limits.api_timeout);
	// Its own budget: the bound and the timeout cover the handshake only (the socket lives on
	// after the 101), and a burst of reconnecting tabs must not shed the API's requests.
	let live = Router::new().route("/api/v1/live", get(live::upgrade)).with_state(Live::new(sign_in.clone(), limits.live));
	let live = bounded(live, limits.live.upgrade_concurrent, limits.live.upgrade_timeout);
	let app = Router::new().route("/health", get(|| async { "ok" })).merge(auth).merge(live).nest("/api/v1", api);
	let app = match forward {
		Some(forward) => app.merge(ingest::routes(forward.clone(), &limits)).merge(forward.routes()),
		None => app,
	};
	let app = match grafana {
		Some(grafana) => app.merge(grafana.routes()),
		None => app,
	};
	app.layer(middleware::map_response(nosniff))
}

/// What the `/api/v1` routes are served on.
#[derive(Clone)]
pub(crate) struct ApiState {
	pub(crate) panel: Panel,
	/// With the forward configured.
	pub(crate) sites: Option<Services>,
	/// What the Connected sites page asks the landings with.
	pub(crate) probe: reqwest::Client,
	/// With `TELEGRAM_BOT_TOKEN`.
	pub(crate) telegram: Option<Telegram>,
	/// With `GOOGLE_MAPS_BROWSER_KEY`.
	pub(crate) map: Option<gmbs::MapDto>,
}

impl FromRef<ApiState> for Panel {
	fn from_ref(s: &ApiState) -> Self {
		s.panel.clone()
	}
}

impl Default for Limits {
	fn default() -> Self {
		Self {
			max_concurrent: 32,
			body_timeout: Duration::from_secs(10),
			request_timeout: Duration::from_secs(30),
			auth_concurrent: 8,
			auth_timeout: Duration::from_secs(10),
			api_concurrent: 32,
			api_timeout: Duration::from_secs(15),
			live: LiveLimits::default(),
		}
	}
}

/// At most `concurrent` at once across `routes`, past it `503` rather than a queue; at most
/// `timeout` each. The semaphore is shared: `Router::layer` wraps every route on its own, and
/// a plain concurrency limit would give each route a budget of its own.
fn bounded<S: Clone + Send + Sync + 'static>(routes: Router<S>, concurrent: usize, timeout: Duration) -> Router<S> {
	let permits = Arc::new(tokio::sync::Semaphore::new(concurrent));
	routes.layer(
		ServiceBuilder::new()
			// The only error the layers below raise is the shed: everything else is a response.
			.layer(HandleErrorLayer::new(|_: BoxError| async { error(StatusCode::SERVICE_UNAVAILABLE, "busy, try again") }))
			.load_shed()
			.layer(GlobalConcurrencyLimitLayer::with_semaphore(permits))
			.layer(TimeoutLayer::with_status_code(StatusCode::SERVICE_UNAVAILABLE, timeout)),
	)
}

/// Every answer is what its `Content-Type` says, never sniffed into something else.
async fn nosniff(mut res: Response) -> Response {
	res.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
	res
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
	(status, Json(json!({ "error": msg.into() }))).into_response()
}

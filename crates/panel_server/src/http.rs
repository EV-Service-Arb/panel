//! The HTTP API: ingest, `/health`, the sites' reads of a place's live settings, a brand's
//! pricing and its experiments' overrides (`/api/internal`, [`crate::places`], [`crate::pricing`],
//! [`crate::experiments`]), and — when signing in is
//! configured — the sign-in (`/auth/*`, [`crate::signin`]), the operator API (`/api/v1/*`,
//! [`crate::api`]) and its live socket (`/api/v1/live`, [`crate::live`]).
//!
//! `POST /api/ingest/v1/events` is authenticated by the source's signature alone (see
//! `panel_core::signature`): no cookie, no CSRF, nothing but the key id and the MAC over the
//! raw body. It is meant for the cluster's own sources and is not published by the ingress
//! (docs/ARCHITECTURE.md, Deploy). The handler only translates; what is accepted and why is
//! the engine's.
//!
//! Before a byte of the body is read, the headers are checked for what costs nothing — the
//! key id's shape, the timestamp's window — and the body is then read up to [`MAX_BODY`]
//! only. The route sheds load past a number of requests at once, and bounds how long a body
//! may take to arrive and a request in all ([`Limits`]).
//!
//! `GET /api/ingest/v1/leads/by-ref/{brand}/{ref}` is a bot's lookup of the lead a messenger
//! ref names, signed the same way over `GET <path>` in place of a body (docs/BOT-API.md): `200` with the lead,
//! `404` when no lead of the brand carries the ref, `403` for a key that is not a bot's of that
//! brand, `400` for a brand or ref that cannot be one, `401` as for ingest.
//!
//! Answers: `207` with a verdict per event (protojson `sa.v1.IngestResponse`); `409` with
//! `Retry-After` and the same verdicts when an event names what has not arrived yet (a
//! `booking.requested` before its `lead.created`: `deferred`) — kitstart's outbox retries a
//! 409 for `booking.requested`, and what was journaled of the batch is a duplicate then;
//! `401` for a
//! batch whose key or signature is refused; `400` for a body that is not a batch; `408` for
//! a body too slow to arrive; `413` past [`MAX_BODY`]; `503` when shedding load; `500` for
//! our own failures, reported.

use std::{sync::Arc, time::Duration};

use axum::{
	Json, Router,
	body::{Body, to_bytes},
	error_handling::HandleErrorLayer,
	extract::{OriginalUri, Path, State},
	handler::Handler,
	http::{HeaderMap, HeaderValue, Method, StatusCode, header},
	middleware,
	response::{IntoResponse, Response},
	routing::{MethodFilter, MethodRouter, get, on, post},
};
use panel::{IngestError, Outcome, Panel, SignedBatch, booking::PushSources};
use panel_contracts::v1::{EventResult, IngestResponse};
use panel_core::signature::{self, SignatureError};
use sa_auth::{Analysis, PermissionSet, Sources, Work};
use serde_json::{Value, json};
use tower::{BoxError, ServiceBuilder, limit::GlobalConcurrencyLimitLayer};
use tower_http::timeout::{RequestBodyTimeoutLayer, TimeoutError, TimeoutLayer};

use crate::{
	access, api, booking, experiments, invoices,
	live::{self, Live, LiveLimits},
	places, pricing,
	signin::{self, Freshness, Gate, SignIn},
	telegram::{self, BotName, TelegramState},
};

/// A part of the panel, as its screens are grouped, and the permission that opens it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
	/// Leads, bookings, places, pricing, the overview: the daily work.
	Work,
	/// Experiments.
	Analysis,
	/// Sources and their keys.
	Admin,
}

impl Section {
	pub fn may(self, permissions: &PermissionSet) -> bool {
		match self {
			Self::Work => permissions.may(Work::Read),
			Self::Analysis => permissions.may(Analysis::Read),
			Self::Admin => permissions.may(Sources::Manage),
		}
	}
}

/// One `/api/v1` route served on the engine's state, and who passes its gate.
pub struct ApiRoute {
	pub method: Method,
	/// Under `/api/v1`.
	pub path: &'static str,
	/// `None`: every signed-in user.
	pub section: Option<Section>,
	freshness: Freshness,
	handler: MethodRouter<Panel>,
}

impl ApiRoute {
	pub(crate) fn new<H: Handler<T, Panel>, T: 'static>(method: Method, path: &'static str, section: Option<Section>, freshness: Freshness, handler: H) -> Self {
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

/// Every `/api/v1` route on the engine's state; the profile's Telegram routes, open to every
/// signed-in user, are [`crate::telegram`]'s.
pub fn api_routes() -> Vec<ApiRoute> {
	[
		api::routes(),
		access::routes(),
		booking::routes(),
		places::routes(),
		pricing::routes(),
		experiments::routes(),
		invoices::routes(),
	]
	.into_iter()
	.flatten()
	.collect()
}

/// 500 events of about 8 KiB each: a batch is refused well before it strains anything.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// What one ingest request may cost.
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

pub fn router(panel: Panel) -> Router {
	router_with(panel, Limits::default())
}

/// Ingest, and the sign-in with the operator API behind it.
pub fn app(sign_in: SignIn) -> Router {
	app_with(sign_in, Limits::default())
}

pub fn app_with(sign_in: SignIn, limits: Limits) -> Router {
	app_with_telegram(sign_in, limits, BotName::off())
}

/// [`app_with`], with the profile's Telegram routes knowing the bot.
pub fn app_with_telegram(sign_in: SignIn, limits: Limits, bot: BotName) -> Router {
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
	let telegram_state = TelegramState { panel: sign_in.panel.clone(), bot };
	let telegram = telegram::routes()
		.route_layer(gate(Freshness::Cached, None))
		.merge(telegram::link_routes().route_layer(gate(Freshness::Fresh, None)))
		.with_state(telegram_state);
	let api = bounded(routes.with_state(sign_in.panel.clone()).merge(telegram), limits.api_concurrent, limits.api_timeout);
	// Its own budget: the bound and the timeout cover the handshake only (the socket lives on
	// after the 101), and a burst of reconnecting tabs must not shed the API's requests.
	let live = Router::new().route("/api/v1/live", get(live::upgrade)).with_state(Live::new(sign_in.clone(), limits.live));
	let live = bounded(live, limits.live.upgrade_concurrent, limits.live.upgrade_timeout);
	router_with(sign_in.panel, limits)
		.merge(auth)
		.merge(live)
		.nest("/api/v1", api)
		.layer(middleware::map_response(nosniff))
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

pub fn router_with(panel: Panel, limits: Limits) -> Router {
	router_with_hooks(panel, limits, PushSources::default())
}

/// [`router_with`], answering the booking webhooks of `hooks` (`POST /api/hooks/booking/…`);
/// a provider not among them is a 404.
pub fn router_with_hooks(panel: Panel, limits: Limits, hooks: PushSources) -> Router {
	let layers = ServiceBuilder::new()
		// The only error the layers below raise is the shed: everything else is a response.
		.layer(HandleErrorLayer::new(|_: BoxError| async { error(StatusCode::SERVICE_UNAVAILABLE, "busy, try again") }))
		.load_shed()
		.concurrency_limit(limits.max_concurrent)
		.layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, limits.request_timeout))
		.layer(RequestBodyTimeoutLayer::new(limits.body_timeout));
	Router::new()
		.route("/health", get(|| async { "ok" }))
		.route("/api/ingest/v1/events", post(ingest).layer(layers.clone()))
		.route("/api/ingest/v1/leads/by-ref/{brand}/{message_ref}", get(lead_by_ref).layer(layers))
		.merge(places::internal())
		.with_state(panel.clone())
		.merge(booking::hooks(panel, hooks))
		.layer(middleware::map_response(nosniff))
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
	(status, Json(json!({ "error": msg.into() }))).into_response()
}

/// What a `GET` is signed over in place of a body: `GET <path?query>`, the path as it arrived.
pub fn signed_get(uri: &axum::http::Uri) -> String {
	format!("GET {}", uri.path_and_query().map_or_else(|| uri.path(), |pq| pq.as_str()))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
	headers.get(name).and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty())
}

/// When a batch with a deferred event is to come again, in seconds: a lead's own event is
/// usually a moment behind.
const DEFERRED_RETRY_AFTER: &str = "30";

const STALE_MESSAGE: &str = "timestamp outside the replay window";
const REFUSED_MESSAGE: &str = "invalid key or signature";

/// What went wrong reading a body: too big, too slow, or cut off.
fn body_error(e: axum::Error) -> Response {
	let inner = e.into_inner();
	let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(inner.as_ref());
	while let Some(c) = cause {
		if c.is::<http_body_util::LengthLimitError>() {
			return error(StatusCode::PAYLOAD_TOO_LARGE, format!("the body is larger than {MAX_BODY} bytes"));
		}
		if c.is::<TimeoutError>() {
			return error(StatusCode::REQUEST_TIMEOUT, "the body took too long to arrive");
		}
		cause = c.source();
	}
	error(StatusCode::BAD_REQUEST, "the body could not be read")
}

/// `Body` stays the last extractor and is read by hand: the signature is over the body
/// exactly as it came, and nothing is read before the headers pass.
async fn ingest(State(panel): State<Panel>, headers: HeaderMap, body: Body) -> Response {
	let (Some(key_id), Some(timestamp), Some(signature)) = (header(&headers, "x-sa-key-id"), header(&headers, "x-sa-timestamp"), header(&headers, "x-sa-signature")) else {
		return error(StatusCode::UNAUTHORIZED, "x-sa-key-id, x-sa-timestamp and x-sa-signature are required");
	};
	let now = jiff::Timestamp::now();
	if let Err(e @ (SignatureError::Stale | SignatureError::MalformedTimestamp)) = signature::check_window(timestamp, now) {
		tracing::debug!(error = %e, "ingest: refused on the timestamp");
		return error(StatusCode::UNAUTHORIZED, STALE_MESSAGE);
	}
	if !panel_core::ids::is_slug(key_id) {
		return error(StatusCode::UNAUTHORIZED, REFUSED_MESSAGE);
	}
	let body = match to_bytes(body, MAX_BODY).await {
		Ok(body) => body,
		Err(e) => return body_error(e),
	};
	let batch = SignedBatch {
		key_id,
		timestamp,
		signature,
		body: &body,
	};
	match panel.ingest(batch, now).await {
		Ok(verdicts) => {
			let deferred = verdicts.iter().any(|v| matches!(v.outcome, Outcome::Deferred(_)));
			let results = verdicts
				.into_iter()
				.map(|v| {
					let (status, reason) = match v.outcome {
						Outcome::Accepted { unregistered: false } => ("accepted", None),
						Outcome::Accepted { unregistered: true } => ("accepted", Some("type not registered: stored, not projected".to_owned())),
						Outcome::Duplicate => ("duplicate", None),
						Outcome::Rejected(e) => ("rejected", Some(e.0)),
						Outcome::Deferred(e) => ("deferred", Some(e.0)),
					};
					EventResult {
						// `wire::MAX_BATCH` keeps this far below u32::MAX.
						index: u32::try_from(v.index).unwrap_or(u32::MAX),
						id: v.id,
						status: status.to_owned(),
						reason,
					}
				})
				.collect();
			if deferred {
				// A 207's `rejected` is final to kitstart's outbox; a 409 (or 425) on
				// `booking.requested` it sends again (lib#185). Not a 5xx: nothing failed here.
				let mut res = (StatusCode::CONFLICT, Json(IngestResponse { results })).into_response();
				res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static(DEFERRED_RETRY_AFTER));
				return res;
			}
			(StatusCode::MULTI_STATUS, Json(IngestResponse { results })).into_response()
		}
		Err(e) => ingest_error(e, "ingest failed"),
	}
}

/// A refusal of the whole request, ingest's or a lookup's. An unknown key and a bad signature
/// read the same, so key ids cannot be probed for; only a timestamp outside the window is told
/// apart, and it says nothing about keys.
fn ingest_error(e: IngestError, what: &'static str) -> Response {
	match e {
		IngestError::Unauthorized(why) => error(StatusCode::UNAUTHORIZED, if why == panel::STALE { STALE_MESSAGE } else { REFUSED_MESSAGE }),
		IngestError::Forbidden(why) => error(StatusCode::FORBIDDEN, why),
		IngestError::BadRequest(e) => error(StatusCode::BAD_REQUEST, e.0),
		IngestError::Internal(e) => {
			crate::report(&e, what);
			error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
		}
	}
}

/// A bot's lookup of the lead a ref names: the same headers as ingest, the signature over the
/// method and the path as sent (with its query, if any) in place of a body, so a signature opens
/// that one lookup and no other. Never the customer's name or phone.
async fn lead_by_ref(State(panel): State<Panel>, Path((brand, message_ref)): Path<(String, String)>, OriginalUri(uri): OriginalUri, headers: HeaderMap) -> Response {
	let (Some(key_id), Some(timestamp), Some(signature)) = (header(&headers, "x-sa-key-id"), header(&headers, "x-sa-timestamp"), header(&headers, "x-sa-signature")) else {
		return error(StatusCode::UNAUTHORIZED, "x-sa-key-id, x-sa-timestamp and x-sa-signature are required");
	};
	let signed = signed_get(&uri);
	let request = SignedBatch {
		key_id,
		timestamp,
		signature,
		body: signed.as_bytes(),
	};
	match panel.lead_by_ref(request, &brand, &message_ref, jiff::Timestamp::now()).await {
		Ok(Some(lead)) => {
			let mut body = json!({
				"lead_id": lead.lead_id,
				"channel": lead.channel,
				"stage": lead.stage,
				"created_at": lead.created_at.map(|t| t.to_string()),
				"message_ref": lead.message_ref,
			});
			let optional = [
				("need", lead.need.map(Value::from)),
				("locality", lead.locality.map(Value::from)),
				("quoted_cents", lead.quoted_cents.map(Value::from)),
				("flow", lead.flow.map(Value::from)),
				("messaged_at", lead.messaged_at.map(|t| Value::from(t.to_string()))),
				("messaged_channel", lead.messaged_channel.map(Value::from)),
			];
			for (k, v) in optional {
				if let Some(v) = v {
					body[k] = v;
				}
			}
			(StatusCode::OK, Json(body)).into_response()
		}
		Ok(None) => error(StatusCode::NOT_FOUND, "no lead of the brand carries that ref"),
		Err(e) => ingest_error(e, "lookup by ref failed"),
	}
}

//! Ingest's front half: who signed a request, then sites, told by a Source assertion.
//!
//! ```text
//! POST /api/ingest/v1/events                      at most 32 at once (else 503), 30 s in all (else 408)
//!   headers present, |now − x-sa-timestamp| ≤ 5 min, x-sa-key-id a slug     else 401, body unread
//!   body ≤ 4 MiB within 10 s                                                 else 413 / 408
//!   key active → secret unsealed → HMAC over "sa-ingest/v1.<ts>.<body>"     else 401
//!   → sites, same path, the body as it came, Source{key_id, kind, brands, body digest}
//! GET /api/ingest/v1/leads/by-ref/{brand}/{ref}   the same, the MAC over "GET <path?query>";
//!   → sites, same path and query, Source over the empty body
//! ```
//!
//! What the key may write, and every verdict, is sites': its answer goes back as it came. An
//! unknown key and a bad signature read the same, so key ids cannot be probed for; only a
//! timestamp outside the window is told apart, and it says nothing about keys.

use axum::{
	Json, Router,
	body::{Body, to_bytes},
	error_handling::HandleErrorLayer,
	extract::{OriginalUri, State},
	http::{HeaderMap, Method, StatusCode},
	response::{IntoResponse, Response},
	routing::{get, post},
};
use panel::{Grant, IngestError, SignedBatch};
use panel_core::signature::{self, SignatureError};
use sa_auth::{Principal, Service, Source};
use serde_json::json;
use tower::{BoxError, ServiceBuilder};
use tower_http::timeout::{RequestBodyTimeoutLayer, TimeoutError, TimeoutLayer};

use crate::{forward::Forward, http::Limits};

/// 500 events of about 8 KiB each: a batch is refused well before it strains anything.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

const EVENTS: &str = "/api/ingest/v1/events";

const STALE_MESSAGE: &str = "timestamp outside the replay window";
const REFUSED_MESSAGE: &str = "invalid key or signature";

pub(crate) fn routes(forward: Forward, limits: &Limits) -> Router {
	let layers = ServiceBuilder::new()
		// The only error the layers below raise is the shed: everything else is a response.
		.layer(HandleErrorLayer::new(|_: BoxError| async { error(StatusCode::SERVICE_UNAVAILABLE, "busy, try again") }))
		.load_shed()
		.concurrency_limit(limits.max_concurrent)
		.layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, limits.request_timeout))
		.layer(RequestBodyTimeoutLayer::new(limits.body_timeout));
	Router::new()
		.route(EVENTS, post(events).layer(layers.clone()))
		.route("/api/ingest/v1/leads/by-ref/{brand}/{message_ref}", get(lead_by_ref).layer(layers))
		.with_state(forward)
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
	(status, Json(json!({ "error": msg.into() }))).into_response()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
	headers.get(name).and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty())
}

/// The signature's three headers, when all are there.
fn signed(headers: &HeaderMap) -> Option<(&str, &str, &str)> {
	Some((header(headers, "x-sa-key-id")?, header(headers, "x-sa-timestamp")?, header(headers, "x-sa-signature")?))
}

fn unsigned() -> Response {
	error(StatusCode::UNAUTHORIZED, "x-sa-key-id, x-sa-timestamp and x-sa-signature are required")
}

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

fn refused(e: IngestError) -> Response {
	match e {
		IngestError::Unauthorized(why) => error(StatusCode::UNAUTHORIZED, if why == panel::STALE { STALE_MESSAGE } else { REFUSED_MESSAGE }),
		IngestError::Internal(e) => {
			crate::report(&e, "authenticating a signed request");
			error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
		}
	}
}

fn source(grant: Grant, body: &[u8]) -> Principal {
	Principal::Source(Source::new(grant.key_id, grant.kind, grant.brands.iter().map(|b| b.as_str().to_owned()).collect(), body))
}

/// Sites' answer, as it came; a 502 when it did not come.
async fn to_sites(f: &Forward, method: Method, path_and_query: &str, headers: HeaderMap, principal: Principal, body: Body) -> Response {
	match f.services().send(Service::Sites, method, path_and_query, headers, Some(principal), body).await {
		Ok(res) => res,
		Err(e) => {
			tracing::warn!(error = %e, path_and_query, "ingest: sites did not answer");
			error(StatusCode::BAD_GATEWAY, "the service did not answer")
		}
	}
}

/// `Body` stays the last extractor and is read by hand: the signature is over the body
/// exactly as it came, and nothing is read before the headers pass.
async fn events(State(f): State<Forward>, headers: HeaderMap, body: Body) -> Response {
	let Some((key_id, timestamp, signature)) = signed(&headers) else {
		return unsigned();
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
	let grant = match f.panel().authenticate(batch, now).await {
		Ok(g) => g,
		Err(e) => return refused(e),
	};
	let principal = source(grant, &body);
	to_sites(&f, Method::POST, EVENTS, headers, principal, Body::from(body)).await
}

/// A bot's lookup of the lead a ref names: the same headers as ingest, the signature over the
/// method and the path as sent (with its query, if any) in place of a body, so a signature opens
/// that one lookup and no other.
async fn lead_by_ref(State(f): State<Forward>, OriginalUri(uri): OriginalUri, headers: HeaderMap) -> Response {
	let Some((key_id, timestamp, signature)) = signed(&headers) else {
		return unsigned();
	};
	let path_and_query = uri.path_and_query().map_or_else(|| uri.path(), |pq| pq.as_str());
	let signed = format!("GET {path_and_query}");
	let request = SignedBatch {
		key_id,
		timestamp,
		signature,
		body: signed.as_bytes(),
	};
	let grant = match f.panel().authenticate(request, jiff::Timestamp::now()).await {
		Ok(g) => g,
		Err(e) => return refused(e),
	};
	to_sites(&f, Method::GET, path_and_query, headers, source(grant, b""), Body::empty()).await
}

//! The conductor's own `/api/v1`: `/me`, the source keys and the connected sites
//! (`connected`). JSON in and out; every request has
//! passed the sign-in gate (`signin::gate`), so a [`Caller`] is in its extensions.

use std::collections::BTreeSet;

use axum::{
	Extension, Json,
	extract::{Path, State, rejection::JsonRejection},
	http::{HeaderValue, Method, StatusCode, header},
	response::{IntoResponse, Response},
};
use sa_auth::SourceKind;
use sa_core::{
	Invalid,
	ids::{BrandId, SiteOrigin},
};
use sa_panel::{Panel, Refused};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
	connected,
	http::{ApiRoute, Section},
	signin::{Caller, Freshness},
};

pub(crate) fn routes() -> Vec<ApiRoute> {
	use Freshness::{Cached, Fresh};
	use Method as M;
	let admin = Some(Section::Admin);
	vec![
		ApiRoute::new(M::GET, "/me", None, Cached, me),
		ApiRoute::new(M::GET, "/sources", admin, Cached, sources),
		ApiRoute::new(M::GET, "/sources/sites", admin, Cached, connected::sites),
		// A key must not be minted or revoked on a permission revoked a moment ago.
		ApiRoute::new(M::POST, "/sources", admin, Fresh, add_source),
		ApiRoute::new(M::DELETE, "/sources/{key_id}", admin, Fresh, revoke_source),
	]
}

/// Why a request failed, as the front end is told.
#[derive(Debug)]
pub enum ApiError {
	BadRequest(String),
	Forbidden,
	NotFound,
	Conflict(String),
	/// Not now: the message says until when.
	Unavailable(String),
	Internal(eyre::Report),
}

impl IntoResponse for ApiError {
	fn into_response(self) -> Response {
		let (status, msg) = match self {
			Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
			Self::Forbidden => (StatusCode::FORBIDDEN, "your permissions do not allow this".to_owned()),
			Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
			Self::Conflict(m) => (StatusCode::CONFLICT, m),
			Self::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m),
			Self::Internal(e) => {
				crate::report(&e, "the conductor's API");
				(StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_owned())
			}
		};
		(status, Json(json!({ "error": msg }))).into_response()
	}
}

impl From<eyre::Report> for ApiError {
	fn from(e: eyre::Report) -> Self {
		Self::Internal(e)
	}
}

impl From<Refused> for ApiError {
	fn from(r: Refused) -> Self {
		match r {
			Refused::Invalid(m) => Self::BadRequest(m),
			Refused::NotFound => Self::NotFound,
			Refused::Conflict(m) => Self::Conflict(m),
			Refused::Closed => Self::Unavailable(r.to_string()),
		}
	}
}

impl From<Invalid> for ApiError {
	fn from(e: Invalid) -> Self {
		Self::BadRequest(e.0)
	}
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

/// A JSON body, its rejection turned into our error shape.
pub(crate) fn body<T: DeserializeOwned>(b: Result<Json<T>, JsonRejection>) -> ApiResult<T> {
	b.map(|Json(v)| v).map_err(|e| ApiError::BadRequest(e.body_text()))
}

async fn me(Extension(caller): Extension<Caller>) -> Json<Caller> {
	Json(caller)
}

// ── sources (admin) ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SourceDto {
	key_id: String,
	kind: &'static str,
	brands: Vec<String>,
	created_at: String,
	revoked_at: Option<String>,
	origin: Option<String>,
}

async fn sources(State(panel): State<Panel>) -> ApiResult<Json<Value>> {
	let sources: Vec<SourceDto> = panel
		.store()
		.sources()
		.await?
		.into_iter()
		.map(|s| SourceDto {
			key_id: s.grant.key_id,
			kind: s.grant.kind.as_str(),
			brands: s.grant.brands.iter().map(|b| b.as_str().to_owned()).collect(),
			created_at: s.created_at.to_string(),
			revoked_at: s.revoked_at.map(|t| t.to_string()),
			origin: s.origin.map(|o| o.to_string()),
		})
		.collect();
	Ok(Json(json!({ "sources": sources })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddSource {
	key_id: String,
	kind: String,
	brands: Vec<String>,
	origin: Option<String>,
}

async fn add_source(State(panel): State<Panel>, Extension(caller): Extension<Caller>, b: Result<Json<AddSource>, JsonRejection>) -> ApiResult<Response> {
	let b = body(b)?;
	let kind: SourceKind = b.kind.parse().map_err(Invalid::from)?;
	// A key of kind panel would only let something outside pass its writes off as typed in by hand.
	if !kind.keyed() {
		return Err(ApiError::BadRequest(format!("a key of kind {kind} is not issued: it is written without one")));
	}
	let brands = b.brands.iter().map(|b| BrandId::parse(b)).collect::<Result<BTreeSet<_>, _>>()?;
	if !sa_core::ids::is_slug(&b.key_id) {
		return Err(ApiError::BadRequest("key_id is a lowercase slug of 1–64 of [a-z0-9_-]".into()));
	}
	if brands.is_empty() {
		return Err(ApiError::BadRequest("a source writes for at least one brand".into()));
	}
	let origin = b.origin.as_deref().map(SiteOrigin::parse).transpose()?;
	if origin.is_some() != (kind == SourceKind::Site) {
		return Err(ApiError::BadRequest("a site key has an origin, and no other kind has one".into()));
	}
	let Some(added) = panel.add_source(&b.key_id, kind, brands, origin).await? else {
		return Err(ApiError::Conflict(format!("a source {} exists already", b.key_id)));
	};
	tracing::info!(user_id = %caller.user_id, key_id = added.key_id, kind = %kind, "source added from the panel");
	// Shown once: the answer is the only place the secret ever is.
	let mut res = (StatusCode::CREATED, Json(json!({ "key_id": added.key_id, "secret": added.secret.as_str() }))).into_response();
	res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	Ok(res)
}

async fn revoke_source(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path(key_id): Path<String>) -> ApiResult<StatusCode> {
	if !panel.revoke_source(&key_id).await? {
		return Err(ApiError::NotFound);
	}
	tracing::info!(user_id = %caller.user_id, key_id, "source revoked from the panel");
	Ok(StatusCode::NO_CONTENT)
}

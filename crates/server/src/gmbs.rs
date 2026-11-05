//! The caller's gmails and the GMBs they manage (`sa_panel::gmbs`), open to every signed-in user:
//! each sees and changes only their own.
//!
//! ```text
//! GET    /api/v1/gmails               Gmails: theirs, each with its GMBs; the removed left out
//! POST   /api/v1/gmails {gmail}       → 201 Gmail; 409 theirs already
//! DELETE /api/v1/gmails/{id}          → 204 (removed; its GMBs with it)
//! GET    /api/v1/gmbs/resolve?place=  Candidates
//! POST   /api/v1/gmails/{id}/gmbs {place_id}  → 201 Gmb; 409 that gmail has it
//! DELETE /api/v1/gmbs/{id}            → 204
//! PUT    /api/v1/gmbs/enabled {gmbs, enabled}  → 204; 404 one is not theirs, and none switched
//! GET    /api/v1/map                  Map: what the browser loads Maps JS with; 404 without it
//! ```
//!
//! Before the import from review_archive, the writes answer 503.

use axum::{
	Extension, Json,
	extract::{
		Path, Query, State,
		rejection::{JsonRejection, QueryRejection},
	},
	http::{Method, StatusCode},
	response::{IntoResponse, Response},
};
use jiff::Timestamp;
use sa_panel::{
	Panel,
	gmbs::{Gmail, Gmb},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
	api::{ApiError, ApiResult, body},
	http::{ApiRoute, ApiState},
	signin::{Caller, Freshness},
};

pub(crate) fn routes() -> Vec<ApiRoute> {
	use Freshness::Cached;
	use Method as M;
	vec![
		ApiRoute::new(M::GET, "/gmails", None, Cached, gmails),
		ApiRoute::new(M::POST, "/gmails", None, Cached, add_gmail),
		ApiRoute::new(M::DELETE, "/gmails/{id}", None, Cached, remove_gmail),
		ApiRoute::new(M::GET, "/gmbs/resolve", None, Cached, resolve),
		ApiRoute::new(M::POST, "/gmails/{id}/gmbs", None, Cached, add_gmb),
		ApiRoute::new(M::DELETE, "/gmbs/{id}", None, Cached, remove_gmb),
		ApiRoute::new(M::PUT, "/gmbs/enabled", None, Cached, switch),
		ApiRoute::new(M::GET, "/map", None, Cached, map),
	]
}

/// `GET /map`: the browser's Maps JS key, referrer-restricted, and the map's id.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "MapConfig")]
pub struct MapDto {
	pub key: String,
	pub map_id: String,
}

/// A GMB under one of the caller's gmails.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Gmb")]
pub struct GmbDto {
	#[ts(type = "number")]
	pub id: i64,
	/// Google's place id.
	pub place_id: String,
	pub name: Option<String>,
	pub address: Option<String>,
	pub gbp_account: Option<String>,
	pub gbp_location: Option<String>,
	/// RFC 3339.
	pub created_at: String,
	/// The caller's switch; review_archive inherits it.
	pub enabled: bool,
	/// `null`: added without a Places key, and not located since.
	#[ts(type = "{ lat: number, lng: number } | null")]
	pub location: Option<sa_places::LatLng>,
}

/// A gmail of the caller's, with the GMBs it manages.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Gmail")]
pub struct GmailDto {
	#[ts(type = "number")]
	pub id: i64,
	pub gmail: String,
	/// RFC 3339.
	pub created_at: String,
	pub gmbs: Vec<GmbDto>,
}

/// `GET /gmails`.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Gmails")]
pub struct GmailsDto {
	pub gmails: Vec<GmailDto>,
}

/// A place a pasted input may mean.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Place")]
pub struct PlaceDto {
	pub place_id: String,
	/// Named and addressed only with the conductor's Places API key.
	pub name: Option<String>,
	pub address: Option<String>,
}

/// `GET /gmbs/resolve`.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Candidates")]
pub struct CandidatesDto {
	pub candidates: Vec<PlaceDto>,
}

impl From<Gmb> for GmbDto {
	fn from(b: Gmb) -> Self {
		Self {
			id: b.id,
			place_id: b.place_id,
			name: b.name,
			address: b.address,
			gbp_account: b.gbp_account,
			gbp_location: b.gbp_location,
			created_at: b.created_at.to_string(),
			enabled: b.enabled,
			location: b.location,
		}
	}
}

impl From<(Gmail, Vec<Gmb>)> for GmailDto {
	fn from((g, gmbs): (Gmail, Vec<Gmb>)) -> Self {
		Self {
			id: g.id,
			gmail: g.gmail,
			created_at: g.created_at.to_string(),
			gmbs: gmbs.into_iter().map(GmbDto::from).collect(),
		}
	}
}

/// A path's id: one that is no number is no id of the caller's.
fn id(raw: &str) -> ApiResult<i64> {
	raw.parse().map_err(|_| ApiError::NotFound)
}

async fn gmails(State(panel): State<Panel>, Extension(caller): Extension<Caller>) -> ApiResult<Json<GmailsDto>> {
	let gmails = panel.gmails(caller.user_id).await?;
	Ok(Json(GmailsDto {
		gmails: gmails.into_iter().map(GmailDto::from).collect(),
	}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewGmail {
	gmail: String,
}

async fn add_gmail(State(panel): State<Panel>, Extension(caller): Extension<Caller>, b: Result<Json<NewGmail>, JsonRejection>) -> ApiResult<Response> {
	let added = panel.add_gmail(caller.user_id, &body(b)?.gmail, Timestamp::now()).await??;
	tracing::info!(user_id = %caller.user_id, gmail_id = added.id, "gmail added");
	Ok((StatusCode::CREATED, Json(GmailDto::from((added, Vec::new())))).into_response())
}

async fn remove_gmail(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path(raw): Path<String>) -> ApiResult<StatusCode> {
	let gmail = id(&raw)?;
	panel.remove_gmail(caller.user_id, gmail, Timestamp::now()).await??;
	tracing::info!(user_id = %caller.user_id, gmail_id = gmail, "gmail removed");
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
	place: String,
}

async fn resolve(State(panel): State<Panel>, q: Result<Query<Resolve>, QueryRejection>) -> ApiResult<Json<CandidatesDto>> {
	let Query(q) = q.map_err(|e| ApiError::BadRequest(e.body_text()))?;
	let candidates = panel.place_candidates(&q.place).await??;
	Ok(Json(CandidatesDto {
		candidates: candidates
			.into_iter()
			.map(|(place_id, p)| PlaceDto {
				place_id,
				name: p.as_ref().and_then(|p| p.name.clone()),
				address: p.and_then(|p| p.address),
			})
			.collect(),
	}))
}

async fn map(State(s): State<ApiState>) -> ApiResult<Json<MapDto>> {
	s.map.map(Json).ok_or(ApiError::NotFound)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewGmb {
	place_id: String,
}

async fn add_gmb(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path(raw): Path<String>, b: Result<Json<NewGmb>, JsonRejection>) -> ApiResult<Response> {
	let gmail = id(&raw)?;
	let place_id = body(b)?.place_id;
	if !sa_places::parse::looks_like_place_id(&place_id) {
		return Err(ApiError::BadRequest(format!("{place_id:?} is not a place id")));
	}
	let added = panel.add_gmb(Some(caller.user_id), gmail, &place_id, Timestamp::now()).await??;
	tracing::info!(user_id = %caller.user_id, gmail_id = gmail, gmb_id = added.id, "GMB added");
	Ok((StatusCode::CREATED, Json(GmbDto::from(added))).into_response())
}

async fn remove_gmb(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path(raw): Path<String>) -> ApiResult<StatusCode> {
	let gmb = id(&raw)?;
	panel.remove_gmb(caller.user_id, gmb, Timestamp::now()).await??;
	tracing::info!(user_id = %caller.user_id, gmb_id = gmb, "GMB removed");
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Switch {
	gmbs: Vec<i64>,
	enabled: bool,
}

async fn switch(State(panel): State<Panel>, Extension(caller): Extension<Caller>, b: Result<Json<Switch>, JsonRejection>) -> ApiResult<StatusCode> {
	let Switch { gmbs, enabled } = body(b)?;
	panel.switch_gmbs(caller.user_id, &gmbs, enabled).await??;
	tracing::info!(user_id = %caller.user_id, ?gmbs, enabled, "GMBs switched");
	Ok(StatusCode::NO_CONTENT)
}

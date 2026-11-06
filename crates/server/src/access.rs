//! A signed-in user asking the admins for access (`sa_panel::access`). Open to every signed-in
//! user: whoever asks usually holds nothing yet.
//!
//! ```text
//! POST /api/v1/access/requests     {need} → 201 AccessRequest, made now; 200 the one standing
//!                                  (asked within the day); 400 a need outside the sa catalog;
//!                                  409 a need the caller holds already
//! GET  /api/v1/access/requests/mine  {requests: [AccessRequest]}
//! ```
//!
//! A need is a permission or an alias of the `sa` catalog; held is holding every permission it
//! stands for. A request whose need the caller holds is dropped whenever they call either.

use std::sync::LazyLock;

use axum::{
	Extension, Json,
	extract::{State, rejection::JsonRejection},
	http::{Method, StatusCode},
	response::{IntoResponse, Response},
};
use jiff::Timestamp;
use sa_auth::{Catalog, PermissionSet};
use sa_panel::{Panel, access::AccessRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::{
	api::{ApiError, ApiResult, body},
	http::ApiRoute,
	signin::{Caller, Freshness},
};

/// An access request of the caller's, standing.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "AccessRequest")]
pub struct AccessRequestDto {
	/// A permission or an alias of the `sa` catalog.
	#[ts(type = "Permission | keyof typeof ALIASES")]
	pub need: String,
	/// RFC 3339.
	pub requested_at: String,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| Catalog::collect("sa", 0));

pub(crate) fn routes() -> Vec<ApiRoute> {
	vec![
		ApiRoute::new(Method::POST, "/access/requests", None, Freshness::Cached, request),
		ApiRoute::new(Method::GET, "/access/requests/mine", None, Freshness::Cached, mine),
	]
}

impl From<AccessRequest> for AccessRequestDto {
	fn from(r: AccessRequest) -> Self {
		Self {
			need: r.need,
			requested_at: r.requested_at.to_string(),
		}
	}
}

/// Whether `permissions` hold everything `need` stands for; `None` for a need outside the catalog.
fn holds(permissions: &PermissionSet, need: &str) -> Option<bool> {
	let has = |p: &str| permissions.iter().any(|q| q == p);
	if CATALOG.permissions.contains(need) {
		return Some(has(need));
	}
	CATALOG.aliases.get(need).map(|members| members.iter().all(|m| has(m)))
}

#[derive(Deserialize)]
struct Ask {
	need: String,
}

async fn request(State(panel): State<Panel>, Extension(caller): Extension<Caller>, b: Result<Json<Ask>, JsonRejection>) -> ApiResult<Response> {
	let need = body(b)?.need;
	let held = holds(&caller.permissions, &need).ok_or_else(|| ApiError::BadRequest(format!("{need:?} is no permission or alias of the sa catalog")))?;
	panel.pending_access(caller.user_id, |n| holds(&caller.permissions, n) == Some(true)).await?;
	if held {
		return Err(ApiError::Conflict(format!("you hold {need} already")));
	}
	let (r, made) = panel.request_access(caller.user_id, &caller.email, &caller.preferred_name, &need, Timestamp::now()).await?;
	let status = if made { StatusCode::CREATED } else { StatusCode::OK };
	Ok((status, Json(AccessRequestDto::from(r))).into_response())
}

async fn mine(State(panel): State<Panel>, Extension(caller): Extension<Caller>) -> ApiResult<Json<Value>> {
	let pending = panel.pending_access(caller.user_id, |n| holds(&caller.permissions, n) == Some(true)).await?;
	Ok(Json(json!({ "requests": pending.into_iter().map(AccessRequestDto::from).collect::<Vec<_>>() })))
}

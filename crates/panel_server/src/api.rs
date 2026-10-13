//! The operator API, `/api/v1`, for the panel's front end. JSON in and out; every request has
//! passed the sign-in gate (`signin::gate`), so a [`Caller`] is in its extensions. What an action
//! means is the engine's (`panel::operator`); this only translates and asks the caller's
//! permissions.
//!
//! Timestamps are RFC 3339 strings, money is minor units of its currency.

use std::collections::BTreeSet;

use axum::{
	Extension, Json,
	extract::{Path, Query, State, rejection::JsonRejection},
	http::{HeaderMap, HeaderValue, Method, StatusCode, header},
	response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};
use panel::{
	Panel,
	operator::{ActionError, Actor, CallOutcome, EventView, FunnelBy, FunnelSlice, LeadQuery, LeadView, NewLead, Payment, Pii, StageMove, SuspectFilter},
};
use panel_core::{
	Invalid,
	event::SourceKind,
	fact::{LeadChannel, LeadFlow, MessageRef, Messenger},
	funnel::{MIN_SAMPLE, Share},
	ids::{BrandId, JobId, LeadId, LocationId},
	lead::Stage,
};
use sa_auth::{Leads, Pii as SeesPii};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
	http::{ApiRoute, Section},
	signin::{Caller, Freshness},
};

/// The longest span the funnel sums over.
const MAX_FUNNEL_DAYS: i32 = 366;

/// The largest amount, in minor units, a quote or a payment may name: €100M, far past any
/// real job, well short of anything that overflows a sum.
const MAX_MINOR: i64 = 10_000_000_000;

/// The currencies a quote or a payment may be in. Checked here, not in the registry: a rebuild
/// must not re-judge what was journaled under an older list.
pub const CURRENCIES: [&str; 4] = ["EUR", "USD", "GBP", "AUD"];

/// The header a client sets to make a write safe to retry.
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";

fn amount(what: &str, v: i64) -> ApiResult<i64> {
	if v.unsigned_abs() > MAX_MINOR.unsigned_abs() {
		return Err(ApiError::BadRequest(format!("{what} is more than {MAX_MINOR} minor units")));
	}
	Ok(v)
}

fn currency(v: &str) -> ApiResult<String> {
	if !CURRENCIES.contains(&v) {
		return Err(ApiError::BadRequest(format!("currency is one of {}", CURRENCIES.join(", "))));
	}
	Ok(v.to_owned())
}

/// The client's idempotency key: 1–128 visible ASCII characters, or none.
pub(crate) fn idempotency_key(headers: &HeaderMap) -> ApiResult<Option<String>> {
	let Some(v) = headers.get(IDEMPOTENCY_KEY) else { return Ok(None) };
	let v = v.to_str().ok().filter(|v| (1..=128).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_graphic()));
	v.map(|v| Some(v.to_owned()))
		.ok_or_else(|| ApiError::BadRequest("Idempotency-Key is 1–128 visible ASCII characters".into()))
}

/// `201` for what was recorded now, `200` for a retry answered with what the first recorded.
fn recorded(replayed: bool, v: Value) -> Response {
	let status = if replayed { StatusCode::OK } else { StatusCode::CREATED };
	(status, Json(v)).into_response()
}

pub(crate) fn routes() -> Vec<ApiRoute> {
	use Freshness::{Cached, Fresh};
	use Method as M;
	let work = Some(Section::Work);
	let admin = Some(Section::Admin);
	vec![
		ApiRoute::new(M::GET, "/me", None, Cached, me),
		ApiRoute::new(M::GET, "/leads", work, Cached, leads),
		ApiRoute::new(M::POST, "/leads", work, Cached, create_lead),
		ApiRoute::new(M::GET, "/leads/counts", work, Cached, lead_counts),
		ApiRoute::new(M::GET, "/leads/{brand}/{lead}", work, Cached, lead),
		ApiRoute::new(M::POST, "/leads/{brand}/{lead}/stage", work, Cached, stage),
		ApiRoute::new(M::POST, "/leads/{brand}/{lead}/messaged", work, Cached, messaged),
		ApiRoute::new(M::POST, "/leads/{brand}/{lead}/calls/attempt", work, Cached, attempt_call),
		ApiRoute::new(M::POST, "/leads/{brand}/{lead}/calls/{attempt}/outcome", work, Cached, call_outcome),
		ApiRoute::new(M::POST, "/leads/{brand}/{lead}/payments", work, Cached, payment),
		ApiRoute::new(M::GET, "/funnel", work, Cached, funnel),
		ApiRoute::new(M::GET, "/places", work, Cached, places),
		ApiRoute::new(M::GET, "/sources", admin, Cached, sources),
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
	Internal(eyre::Report),
}

impl IntoResponse for ApiError {
	fn into_response(self) -> Response {
		let (status, msg) = match self {
			Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
			Self::Forbidden => (StatusCode::FORBIDDEN, "your permissions do not allow this".to_owned()),
			Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
			Self::Conflict(m) => (StatusCode::CONFLICT, m),
			Self::Internal(e) => {
				crate::report(&e, "operator API");
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

impl From<Invalid> for ApiError {
	fn from(e: Invalid) -> Self {
		Self::BadRequest(e.0)
	}
}

impl From<ActionError> for ApiError {
	fn from(e: ActionError) -> Self {
		match e {
			ActionError::NotFound => Self::NotFound,
			ActionError::Conflict(m) => Self::Conflict(m.to_owned()),
			ActionError::Invalid(e) => Self::BadRequest(e.0),
			ActionError::Internal(e) => Self::Internal(e),
		}
	}
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

/// A JSON body, its rejection turned into our error shape.
pub(crate) fn body<T: DeserializeOwned>(b: Result<Json<T>, JsonRejection>) -> ApiResult<T> {
	b.map(|Json(v)| v).map_err(|e| ApiError::BadRequest(e.body_text()))
}

fn allow(ok: bool) -> ApiResult<()> {
	if ok { Ok(()) } else { Err(ApiError::Forbidden) }
}

/// The point where the caller's permissions decide whether PII is shown.
pub(crate) fn pii(caller: &Caller) -> Pii {
	if caller.permissions.may(SeesPii::See) { Pii::Reveal } else { Pii::Withhold }
}

fn ids(brand: &str, lead: &str) -> ApiResult<(BrandId, LeadId)> {
	Ok((BrandId::parse(brand)?, LeadId::parse(lead)?))
}

fn ts(t: Option<Timestamp>) -> Option<String> {
	t.map(|t| t.to_string())
}

/// A UTC day, `2026-09-30`.
fn day(raw: &str, what: &str) -> ApiResult<Date> {
	raw.parse::<Date>().map_err(|_| ApiError::BadRequest(format!("{what} is not a date like 2026-09-30")))
}

/// `from` and `to`, UTC days, both included — `to` not before `from`.
fn days(from: Option<&str>, to: Option<&str>, names: [&str; 2]) -> ApiResult<(Option<Date>, Option<Date>)> {
	let from = from.map(|d| day(d, names[0])).transpose()?;
	let to = to.map(|d| day(d, names[1])).transpose()?;
	if let (Some(f), Some(t)) = (from, to)
		&& f > t
	{
		return Err(ApiError::BadRequest(format!("{} is after {}", names[0], names[1])));
	}
	Ok((from, to))
}

/// `from` and `to` of a report, UTC days, both included: by default the last 30 days, at
/// most [`MAX_FUNNEL_DAYS`].
pub(crate) fn window(from: Option<&str>, to: Option<&str>) -> ApiResult<(Date, Date)> {
	let (from, to) = days(from, to, ["from", "to"])?;
	let to = to.unwrap_or_else(|| Timestamp::now().to_zoned(TimeZone::UTC).date());
	let from = match from {
		Some(d) => d,
		None => to.checked_sub(SignedDuration::from_hours(24 * 29)).map_err(|e| ApiError::Internal(e.into()))?,
	};
	if from > to {
		return Err(ApiError::BadRequest("from is after to".into()));
	}
	if (to - from).get_days() >= MAX_FUNNEL_DAYS {
		return Err(ApiError::BadRequest(format!("at most {MAX_FUNNEL_DAYS} days at once")));
	}
	Ok((from, to))
}

/// The first instant of a UTC day.
fn day_start(d: Date) -> ApiResult<Timestamp> {
	Ok(d.to_zoned(TimeZone::UTC).map_err(|e| ApiError::Internal(e.into()))?.timestamp())
}

// ── /me ─────────────────────────────────────────────────────────────────────────────────

async fn me(Extension(caller): Extension<Caller>) -> Json<Caller> {
	Json(caller)
}

// ── leads ───────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct LeadDto {
	brand: String,
	lead_id: String,
	location: Option<String>,
	job_id: Option<String>,
	stage: &'static str,
	/// form | phone_inbound | callback | whatsapp | telegram; null for a lead seen before its
	/// creation.
	channel: Option<String>,
	/// The ref the customer carries into a messenger (`AQ-7K3F`), when the landing made one.
	message_ref: Option<String>,
	/// When the customer first wrote on a messenger, and on which (whatsapp | telegram); null
	/// while they have not.
	messaged_at: Option<String>,
	messaged_channel: Option<&'static str>,
	/// `rate_limited` | `too_fast` when the landing's antispam doubted it; null otherwise.
	suspect: Option<String>,
	manual: bool,
	created_at: Option<String>,
	contacted_at: Option<String>,
	quoted_at: Option<String>,
	won_at: Option<String>,
	completed_at: Option<String>,
	paid_at: Option<String>,
	lost_at: Option<String>,
	lost_reason: Option<String>,
	/// `quote` | `estimate` | `fixed`: the landing's flow; null when it said none.
	flow: Option<String>,
	/// The price an estimate or a fixed price showed, integer cents EUR TTC.
	quoted_cents: Option<i64>,
	/// `YYYY-MM-DD`: when the pricing model behind `quoted_cents` took effect.
	pricing_valid_from: Option<String>,
	/// An estimate's inputs, input id → value id.
	estimate_inputs: Option<Value>,
	/// Its booking; `status: none` when it has none.
	booking: BookingDto,
	last_event_at: String,
	/// Set while it waits for its first contact.
	sla: Option<SlaDto>,
	/// What the customer left (name, phone, need, …), for a caller who sees it; absent
	/// otherwise or when there is none.
	#[serde(skip_serializing_if = "Option::is_none")]
	pii: Option<Value>,
}

#[derive(Serialize)]
struct BookingDto {
	/// none | requested | booked | canceled | done | no_show
	status: &'static str,
	/// manual | link | google_calendar | cal_com
	provider: Option<&'static str>,
	start_at: Option<String>,
	end_at: Option<String>,
	/// The provider's booking, when the slot is one.
	external_ref: Option<String>,
	/// ref | contact | manual: how the slot joined the lead.
	r#match: Option<&'static str>,
	/// The visitor's wish (`YYYY-MM-DD`; morning | afternoon | evening).
	preferred_date: Option<String>,
	preferred_part: Option<&'static str>,
}

#[derive(Serialize)]
struct SlaDto {
	waiting_since: String,
	waiting_seconds: i64,
	overdue: bool,
}

fn lead_dto(v: LeadView, now: Timestamp) -> LeadDto {
	let r = v.row;
	LeadDto {
		sla: v.waiting.map(|w| SlaDto {
			waiting_since: w.since.to_string(),
			waiting_seconds: now.duration_since(w.since).as_secs(),
			overdue: w.overdue,
		}),
		pii: v.pii,
		brand: r.brand_id,
		lead_id: r.lead_id,
		location: r.location_id,
		job_id: r.job_id,
		stage: r.stage.as_str(),
		channel: r.channel,
		message_ref: r.message_ref,
		messaged_at: ts(r.messaged.map(|(at, _)| at)),
		messaged_channel: r.messaged.map(|(_, m)| m.as_str()),
		suspect: r.suspect,
		manual: r.manual,
		created_at: ts(r.created_at),
		contacted_at: ts(r.contacted_at),
		quoted_at: ts(r.quoted_at),
		won_at: ts(r.won_at),
		completed_at: ts(r.completed_at),
		paid_at: ts(r.paid_at),
		lost_at: ts(r.lost_at),
		lost_reason: r.lost_reason,
		flow: r.flow,
		quoted_cents: r.quoted_cents,
		pricing_valid_from: r.pricing_valid_from,
		estimate_inputs: r.estimate_inputs,
		booking: BookingDto {
			status: r.booking.status.as_str(),
			provider: r.booking.provider.map(|p| p.as_str()),
			start_at: ts(r.booking.start_at),
			end_at: ts(r.booking.end_at),
			external_ref: r.booking.external_ref,
			r#match: r.booking.matched.map(|m| m.as_str()),
			preferred_date: r.booking.preferred_date.map(|d| d.to_string()),
			preferred_part: r.booking.preferred_part.map(|p| p.as_str()),
		},
		last_event_at: r.last_event_at.to_string(),
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeadsQuery {
	stage: Option<String>,
	brand: Option<String>,
	location: Option<String>,
	overdue: Option<bool>,
	/// UTC days, both included.
	created_from: Option<String>,
	created_to: Option<String>,
	/// `only` | `exclude`; absent lists every lead.
	suspect: Option<String>,
	/// `quote` | `estimate` | `fixed`; absent lists every lead.
	flow: Option<String>,
	/// A booking status (`none`, `requested`, `booked`, …); absent lists every lead.
	booking: Option<String>,
	/// A lead channel (`form`, `whatsapp`, …); absent lists every lead.
	channel: Option<String>,
	/// A messenger ref as the customer quotes it or an operator pastes it (`Réf. aq 7k3f`; see
	/// `MessageRef::from_typed`): the leads carrying it.
	message_ref: Option<String>,
	cursor: Option<String>,
	limit: Option<u32>,
}

fn cursor_encode((at, brand, lead): &(Timestamp, String, String)) -> String {
	URL_SAFE_NO_PAD.encode(format!("{at}|{brand}|{lead}"))
}

fn cursor_decode(raw: &str) -> ApiResult<(Timestamp, String, String)> {
	let bad = || ApiError::BadRequest("cursor is not one this API gave".into());
	let text = String::from_utf8(URL_SAFE_NO_PAD.decode(raw).map_err(|_| bad())?).map_err(|_| bad())?;
	let mut parts = text.splitn(3, '|');
	let (Some(at), Some(brand), Some(lead)) = (parts.next(), parts.next(), parts.next()) else {
		return Err(bad());
	};
	Ok((at.parse().map_err(|_| bad())?, brand.to_owned(), lead.to_owned()))
}

async fn leads(State(panel): State<Panel>, Extension(caller): Extension<Caller>, q: Result<Query<LeadsQuery>, axum::extract::rejection::QueryRejection>) -> ApiResult<Json<Value>> {
	let Query(q) = q.map_err(|e| ApiError::BadRequest(e.body_text()))?;
	let (created_from, created_to) = days(q.created_from.as_deref(), q.created_to.as_deref(), ["created_from", "created_to"])?;
	let next_day = |d: Date| d.tomorrow().map_err(|e| ApiError::Internal(e.into())).and_then(day_start);
	let query = LeadQuery {
		stage: q.stage.as_deref().map(str::parse::<Stage>).transpose()?,
		brand: q.brand.as_deref().map(BrandId::parse).transpose()?,
		location: q.location.as_deref().map(LocationId::parse).transpose()?,
		overdue: q.overdue.unwrap_or(false),
		created_from: created_from.map(day_start).transpose()?,
		created_before: created_to.map(next_day).transpose()?,
		suspect: match q.suspect.as_deref() {
			None => SuspectFilter::All,
			Some("only") => SuspectFilter::Only,
			Some("exclude") => SuspectFilter::Exclude,
			Some(_) => return Err(ApiError::BadRequest("suspect is not one of only, exclude".into())),
		},
		flow: q
			.flow
			.as_deref()
			.map(|f| LeadFlow::parse(f).map_err(|_| ApiError::BadRequest("flow is not one of quote, estimate, fixed".into())))
			.transpose()?,
		booking: q
			.booking
			.as_deref()
			.map(|b| panel_core::booking::BookingStatus::parse(b).map_err(|e| ApiError::BadRequest(e.0)))
			.transpose()?,
		channel: q
			.channel
			.as_deref()
			.map(|c| LeadChannel::parse(c).map_err(|_| ApiError::BadRequest("channel is not one of form, phone_inbound, callback, whatsapp, telegram".into())))
			.transpose()?,
		message_ref: q
			.message_ref
			.as_deref()
			.map(|r| MessageRef::from_typed(r).map_err(|_| ApiError::BadRequest("message_ref is not like AQ-7K3F".into())))
			.transpose()?,
		after: q.cursor.as_deref().map(cursor_decode).transpose()?,
		limit: q.limit.unwrap_or(50),
	};
	let now = Timestamp::now();
	let page = panel.leads(&query, pii(&caller), now).await?;
	let next = page.next.as_ref().map(cursor_encode);
	let leads: Vec<LeadDto> = page.leads.into_iter().map(|v| lead_dto(v, now)).collect();
	Ok(Json(json!({ "leads": leads, "next_cursor": next })))
}

#[derive(Serialize)]
struct EventDto {
	id: String,
	r#type: String,
	type_version: i32,
	occurred_at: String,
	received_at: String,
	source_kind: String,
	source_id: String,
	manual: bool,
	job_id: Option<String>,
	status: String,
	status_reason: Option<String>,
	properties: Value,
	#[serde(skip_serializing_if = "Option::is_none")]
	pii: Option<Value>,
}

fn event_dto(e: EventView) -> EventDto {
	let r = e.row;
	EventDto {
		manual: r.source_kind == SourceKind::Panel.as_str(),
		id: r.id.to_string(),
		r#type: r.r#type,
		type_version: r.type_version,
		occurred_at: r.occurred_at.to_string(),
		received_at: r.received_at.to_string(),
		source_kind: r.source_kind,
		source_id: r.source_id,
		job_id: r.job_id,
		status: r.status,
		status_reason: r.status_reason,
		properties: r.properties,
		pii: e.pii,
	}
}

async fn lead(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path((brand, lead)): Path<(String, String)>) -> ApiResult<Json<Value>> {
	let (brand, lead) = ids(&brand, &lead)?;
	let now = Timestamp::now();
	let (view, events) = panel.lead_card(&brand, &lead, pii(&caller), now).await?.ok_or(ApiError::NotFound)?;
	let events: Vec<EventDto> = events.into_iter().map(event_dto).collect();
	Ok(Json(json!({ "lead": lead_dto(view, now), "events": events })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateLead {
	brand: String,
	location: String,
	need: String,
	phone: Option<String>,
	/// phone_inbound (the default) | whatsapp | telegram: a customer who wrote on a messenger
	/// without the landing.
	channel: Option<String>,
}

async fn create_lead(State(panel): State<Panel>, Extension(caller): Extension<Caller>, headers: HeaderMap, b: Result<Json<CreateLead>, JsonRejection>) -> ApiResult<Response> {
	allow(caller.permissions.may(Leads::Edit))?;
	let key = idempotency_key(&headers)?;
	let b = body(b)?;
	let new = NewLead {
		brand: BrandId::parse(&b.brand)?,
		location: LocationId::parse(&b.location)?,
		need: b.need,
		phone: b.phone,
		channel: match b.channel.as_deref() {
			None => LeadChannel::PhoneInbound,
			Some(c) => LeadChannel::parse(c)
				.ok()
				.filter(|c| LeadChannel::MANUAL.contains(c))
				.ok_or_else(|| ApiError::BadRequest("channel is one of phone_inbound, whatsapp, telegram".into()))?,
		},
	};
	let brand = new.brand.clone();
	let done = panel.create_lead_once(Actor(caller.user_id), new, Timestamp::now(), key.as_deref()).await?;
	let (lead, event) = done.value;
	Ok(recorded(
		done.replayed,
		json!({ "brand": brand.as_str(), "lead_id": lead.as_str(), "event_id": event.raw().to_string() }),
	))
}

fn created(v: Value) -> Response {
	(StatusCode::CREATED, Json(v)).into_response()
}

#[derive(Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
enum StageBody {
	Contacted { channel: Option<String> },
	Quoted { amount: Option<i64>, currency: Option<String> },
	Won { job_id: Option<String> },
	Lost { reason: String, note: Option<String> },
	Completed,
}

async fn stage(
	State(panel): State<Panel>,
	Extension(caller): Extension<Caller>,
	Path((brand, lead)): Path<(String, String)>,
	headers: HeaderMap,
	b: Result<Json<StageBody>, JsonRejection>,
) -> ApiResult<Response> {
	allow(caller.permissions.may(Leads::Edit))?;
	let key = idempotency_key(&headers)?;
	let (brand, lead) = ids(&brand, &lead)?;
	let to = match body(b)? {
		StageBody::Contacted { channel } => StageMove::Contacted { channel },
		StageBody::Quoted { amount: a, currency: c } => StageMove::Quoted {
			amount: a.map(|a| amount("amount", a)).transpose()?,
			currency: c.as_deref().map(currency).transpose()?,
		},
		StageBody::Won { job_id } => StageMove::Won {
			job_id: job_id.as_deref().map(JobId::parse).transpose()?,
		},
		StageBody::Lost { reason, note } => StageMove::Lost { reason, note },
		StageBody::Completed => StageMove::Completed,
	};
	let done = panel.move_lead_once(Actor(caller.user_id), &brand, &lead, to, Timestamp::now(), key.as_deref()).await?;
	Ok(recorded(done.replayed, json!({ "event_id": done.value.raw().to_string() })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessagedBody {
	/// whatsapp | telegram
	channel: String,
}

/// The customer wrote on a messenger, as the operator saw it (`lead.messaged` from the panel).
async fn messaged(
	State(panel): State<Panel>,
	Extension(caller): Extension<Caller>,
	Path((brand, lead)): Path<(String, String)>,
	headers: HeaderMap,
	b: Result<Json<MessagedBody>, JsonRejection>,
) -> ApiResult<Response> {
	allow(caller.permissions.may(Leads::Edit))?;
	let key = idempotency_key(&headers)?;
	let (brand, lead) = ids(&brand, &lead)?;
	let channel = Messenger::parse(&body(b)?.channel).map_err(|_| ApiError::BadRequest("channel is one of whatsapp, telegram".into()))?;
	let done = panel.mark_messaged_once(Actor(caller.user_id), &brand, &lead, channel, Timestamp::now(), key.as_deref()).await?;
	Ok(recorded(done.replayed, json!({ "event_id": done.value.raw().to_string() })))
}

async fn attempt_call(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path((brand, lead)): Path<(String, String)>) -> ApiResult<Response> {
	allow(caller.permissions.may(Leads::Edit))?;
	let (brand, lead) = ids(&brand, &lead)?;
	let attempt = panel.attempt_call(Actor(caller.user_id), &brand, &lead, Timestamp::now()).await?;
	Ok(created(json!({ "attempt_id": attempt.raw().to_string() })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeBody {
	outcome: String,
}

async fn call_outcome(
	State(panel): State<Panel>,
	Extension(caller): Extension<Caller>,
	Path((brand, lead, attempt)): Path<(String, String, String)>,
	b: Result<Json<OutcomeBody>, JsonRejection>,
) -> ApiResult<Response> {
	allow(caller.permissions.may(Leads::Edit))?;
	let (brand, lead) = ids(&brand, &lead)?;
	let attempt = Uuid::parse_str(&attempt).map_err(|_| ApiError::NotFound)?;
	let call = CallOutcome { attempt, outcome: body(b)?.outcome };
	let event = panel.log_call(Actor(caller.user_id), &brand, &lead, call, Timestamp::now()).await?;
	Ok(created(json!({ "event_id": event.raw().to_string() })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaymentBody {
	billed: i64,
	commission: i64,
	currency: String,
}

async fn payment(
	State(panel): State<Panel>,
	Extension(caller): Extension<Caller>,
	Path((brand, lead)): Path<(String, String)>,
	headers: HeaderMap,
	b: Result<Json<PaymentBody>, JsonRejection>,
) -> ApiResult<Response> {
	allow(caller.permissions.may(Leads::Edit))?;
	let key = idempotency_key(&headers)?;
	let (brand, lead) = ids(&brand, &lead)?;
	let b = body(b)?;
	let p = Payment {
		billed: amount("billed", b.billed)?,
		commission: amount("commission", b.commission)?,
		currency: currency(&b.currency)?,
	};
	let done = panel.record_payment_once(Actor(caller.user_id), &brand, &lead, p, Timestamp::now(), key.as_deref()).await?;
	Ok(recorded(done.replayed, json!({ "event_id": done.value.raw().to_string() })))
}

// ── funnel ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FunnelQuery {
	from: Option<String>,
	to: Option<String>,
	brand: Option<String>,
	/// `location`: one slice per location. Absent: the whole funnel.
	by: Option<String>,
}

/// A share as the front end may show it: `percent` only when `of` is large enough, and
/// `small_sample` saying so otherwise, so "12 of 17" is all that can be drawn.
#[derive(Serialize)]
pub(crate) struct ShareDto {
	n: u64,
	of: u64,
	percent: Option<u64>,
	small_sample: bool,
}

impl From<Share> for ShareDto {
	fn from(s: Share) -> Self {
		Self {
			n: s.n,
			of: s.of,
			percent: s.percent,
			small_sample: s.small_sample(),
		}
	}
}

#[derive(Serialize)]
struct StepDto {
	stage: &'static str,
	reached: u64,
	of_previous: Option<ShareDto>,
	of_leads: ShareDto,
}

/// What a slice's leads were paid in one currency, in its minor units.
#[derive(Serialize)]
struct PaidDto {
	currency: String,
	billed: i64,
	commission: i64,
	count: u64,
}

/// A slice's funnel: its steps, the lost and manual shares, and its payments.
fn slice_body(s: FunnelSlice) -> serde_json::Map<String, Value> {
	let totals = s.totals;
	let stages: Vec<StepDto> = totals
		.steps()
		.into_iter()
		.map(|s| StepDto {
			stage: s.stage.as_str(),
			reached: s.reached,
			of_previous: s.of_previous.map(ShareDto::from),
			of_leads: s.of_leads.into(),
		})
		.collect();
	let payments: Vec<PaidDto> = s
		.payments
		.into_iter()
		.map(|p| PaidDto {
			currency: p.currency,
			billed: p.billed,
			commission: p.commission,
			count: p.payments,
		})
		.collect();
	let mut body = serde_json::Map::new();
	body.insert("stages".into(), json!(stages));
	body.insert("lost".into(), json!(ShareDto::from(Share::new(totals.lost, totals.leads))));
	body.insert("manual".into(), json!(ShareDto::from(Share::new(totals.manual, totals.leads))));
	body.insert("payments".into(), json!(payments));
	body
}

async fn funnel(State(panel): State<Panel>, q: Result<Query<FunnelQuery>, axum::extract::rejection::QueryRejection>) -> ApiResult<Json<Value>> {
	let Query(q) = q.map_err(|e| ApiError::BadRequest(e.body_text()))?;
	let (from, to) = window(q.from.as_deref(), q.to.as_deref())?;
	let by = match q.by.as_deref() {
		None => FunnelBy::All,
		Some("location") => FunnelBy::Location,
		Some(_) => return Err(ApiError::BadRequest("by is location, or absent".into())),
	};
	let brand = q.brand.as_deref().map(BrandId::parse).transpose()?;
	let slices = panel.funnel_slices(from, to, brand.as_ref(), by).await?;
	let mut body = json!({
		"from": from.to_string(),
		"to": to.to_string(),
		"brand": brand.as_ref().map(BrandId::as_str),
		"min_sample": MIN_SAMPLE,
		"posthog_url": panel.posthog_funnel_url(brand.as_ref(), from, to),
	});
	match by {
		FunnelBy::All => {
			let whole = slices.into_iter().next().ok_or_else(|| ApiError::Internal(eyre::eyre!("the whole funnel came back empty")))?;
			for (k, v) in slice_body(whole) {
				body[k] = v;
			}
		}
		FunnelBy::Location => {
			let locations: Vec<Value> = slices
				.into_iter()
				.map(|s| {
					let mut row = serde_json::Map::new();
					row.insert("brand".into(), json!(s.brand));
					row.insert("location".into(), json!(s.location));
					row.extend(slice_body(s));
					Value::Object(row)
				})
				.collect();
			body["by"] = json!("location");
			body["locations"] = json!(locations);
		}
	}
	Ok(Json(body))
}

// ── places, counts ──────────────────────────────────────────────────────────────────────

async fn places(State(panel): State<Panel>) -> ApiResult<Json<Value>> {
	let places: Vec<Value> = panel
		.places()
		.await?
		.into_iter()
		.map(|p| {
			json!({
				"brand": p.brand_id,
				"location": p.location_id,
				"last_lead_at": ts(p.last_lead_at),
				"has_settings": p.has_settings,
				"withdrawn": p.withdrawn,
			})
		})
		.collect();
	Ok(Json(json!({ "places": places })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CountsQuery {
	brand: Option<String>,
	location: Option<String>,
}

async fn lead_counts(State(panel): State<Panel>, q: Result<Query<CountsQuery>, axum::extract::rejection::QueryRejection>) -> ApiResult<Json<Value>> {
	let Query(q) = q.map_err(|e| ApiError::BadRequest(e.body_text()))?;
	let brand = q.brand.as_deref().map(BrandId::parse).transpose()?;
	let location = q.location.as_deref().map(LocationId::parse).transpose()?;
	let counts = panel.lead_counts(brand.as_ref(), location.as_ref(), Timestamp::now()).await?;
	let total: u64 = counts.stages.iter().map(|(_, n)| n).sum();
	let stages: serde_json::Map<String, Value> = counts.stages.into_iter().map(|(stage, n)| (stage.as_str().to_owned(), json!(n))).collect();
	Ok(Json(json!({ "stages": stages, "overdue": counts.overdue, "total": total })))
}

// ── sources (admin) ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SourceDto {
	key_id: String,
	kind: &'static str,
	brands: Vec<String>,
	created_at: String,
	revoked_at: Option<String>,
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
}

async fn add_source(State(panel): State<Panel>, Extension(caller): Extension<Caller>, b: Result<Json<AddSource>, JsonRejection>) -> ApiResult<Response> {
	let b = body(b)?;
	let kind: SourceKind = b.kind.parse()?;
	// The panel's own events carry no key (`key_id` NULL): a key of kind panel would only let
	// something outside pass its writes off as typed in by hand.
	if !kind.keyed() {
		return Err(ApiError::BadRequest(format!("a key of kind {kind} is not issued: the panel writes without one")));
	}
	let brands = b.brands.iter().map(|b| BrandId::parse(b)).collect::<Result<BTreeSet<_>, _>>()?;
	if !panel_core::ids::is_slug(&b.key_id) {
		return Err(ApiError::BadRequest("key_id is a lowercase slug of 1–64 of [a-z0-9_-]".into()));
	}
	if brands.is_empty() {
		return Err(ApiError::BadRequest("a source writes for at least one brand".into()));
	}
	let Some(added) = panel.add_source(&b.key_id, kind, brands).await? else {
		return Err(ApiError::Conflict(format!("a source {} exists already", b.key_id)));
	};
	tracing::info!(user_id = %caller.user_id, key_id = added.key_id, kind = %kind, "source added from the panel");
	// Shown once: the answer is the only place the secret ever is.
	let mut res = created(json!({ "key_id": added.key_id, "secret": added.secret.as_str() }));
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cursors_round_trip() {
		let c = ("2026-09-30T10:00:00Z".parse().unwrap(), "aquafix".to_owned(), "L-1".to_owned());
		assert_eq!(cursor_decode(&cursor_encode(&c)).unwrap(), c);
		assert!(cursor_decode("nope!").is_err());
		assert!(cursor_decode(&URL_SAFE_NO_PAD.encode("x|y")).is_err());
	}
}

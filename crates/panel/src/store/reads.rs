//! What the operator screens read: leads with the PII of their creation, a lead's events,
//! and the funnel's daily totals from `reporting_funnel_daily`.

use eyre::WrapErr;
use jiff::{Timestamp, civil::Date};
use panel_core::{
	booking::{BookingMatch, BookingState, BookingStatus, DayPart, Provider},
	fact::{LeadChannel, LeadFlow, MessageRef, Messenger},
	funnel::Totals,
	ids::{BrandId, LeadId, LocationId},
	lead::Stage,
};
use serde_json::Value;
use sqlx::{SqliteConnection, types::Json};
use uuid::Uuid;

use super::{day_to_db, from_db, to_db};

/// A lead as the projection holds it, with its creating event's sealed PII.
#[derive(Clone, Debug)]
pub struct LeadRow {
	pub brand_id: String,
	pub lead_id: String,
	pub location_id: Option<String>,
	pub job_id: Option<String>,
	pub stage: Stage,
	pub channel: Option<String>,
	/// Why the landing's antispam doubted it (`rate_limited`, `too_fast`); `None` for an
	/// ordinary lead.
	pub suspect: Option<String>,
	pub manual: bool,
	pub created_at: Option<Timestamp>,
	pub contacted_at: Option<Timestamp>,
	pub quoted_at: Option<Timestamp>,
	pub won_at: Option<Timestamp>,
	pub completed_at: Option<Timestamp>,
	pub paid_at: Option<Timestamp>,
	pub lost_at: Option<Timestamp>,
	pub lost_reason: Option<String>,
	/// `quote` | `estimate` | `fixed`, from its `lead.created`; `None` when that said none.
	pub flow: Option<String>,
	/// The price an estimate or a fixed price showed, integer cents EUR TTC.
	pub quoted_cents: Option<i64>,
	/// `YYYY-MM-DD`: when the pricing model behind `quoted_cents` took effect.
	pub pricing_valid_from: Option<String>,
	/// An estimate's inputs, input id → value id.
	pub estimate_inputs: Option<Value>,
	/// The ref the customer carries into a messenger (`AQ-7K3F`), from its `lead.created`.
	pub message_ref: Option<String>,
	/// When the customer first wrote on a messenger (`lead.messaged`), and on which.
	pub messaged: Option<(Timestamp, Messenger)>,
	/// Its booking: none, asked for, booked, …
	pub booking: BookingState,
	pub last_event_at: Timestamp,
	/// When it sorts in the list: its creation, or its first event when never created.
	pub sort_at: Timestamp,
	/// The `lead.created` that counts (the first journaled), and its sealed PII.
	pub creation: Option<Sealed>,
}

/// An event's id and its sealed PII with the fingerprint of the key that sealed it.
#[derive(Clone, Debug)]
pub struct Sealed {
	pub event_id: Uuid,
	pub pii: Option<(Vec<u8>, Vec<u8>)>,
}

#[derive(sqlx::FromRow)]
struct Row {
	brand_id: String,
	lead_id: String,
	location_id: Option<String>,
	job_id: Option<String>,
	stage: String,
	channel: Option<String>,
	suspect: Option<String>,
	manual: bool,
	created_at: Option<i64>,
	contacted_at: Option<i64>,
	quoted_at: Option<i64>,
	won_at: Option<i64>,
	completed_at: Option<i64>,
	paid_at: Option<i64>,
	lost_at: Option<i64>,
	lost_reason: Option<String>,
	flow: Option<String>,
	quoted_cents: Option<i64>,
	pricing_valid_from: Option<String>,
	estimate_inputs: Option<Json<Value>>,
	booking_status: Option<String>,
	booking_provider: Option<String>,
	booking_start_at: Option<i64>,
	booking_end_at: Option<i64>,
	booking_external_ref: Option<String>,
	booking_match: Option<String>,
	booking_preferred_date: Option<String>,
	booking_preferred_part: Option<String>,
	message_ref: Option<String>,
	messaged_at: Option<i64>,
	messaged_channel: Option<String>,
	last_event_at: i64,
	sort_at: i64,
	creation_id: Option<Uuid>,
	pii_sealed: Option<Vec<u8>>,
	data_key_fp: Option<Vec<u8>>,
}

impl TryFrom<Row> for LeadRow {
	type Error = eyre::Report;

	fn try_from(r: Row) -> eyre::Result<Self> {
		let t = |ts: Option<i64>| ts.map(from_db).transpose();
		let at = || format!("stored booking of lead {}/{}", r.brand_id, r.lead_id);
		let booking = BookingState {
			status: r.booking_status.as_deref().map(BookingStatus::parse).transpose().wrap_err_with(at)?.unwrap_or_default(),
			provider: r.booking_provider.as_deref().map(Provider::parse).transpose().wrap_err_with(at)?,
			start_at: t(r.booking_start_at)?,
			end_at: t(r.booking_end_at)?,
			external_ref: r.booking_external_ref,
			matched: r.booking_match.as_deref().map(BookingMatch::parse).transpose().wrap_err_with(at)?,
			preferred_date: r.booking_preferred_date.as_deref().map(super::day_from_db).transpose()?,
			preferred_part: r.booking_preferred_part.as_deref().map(DayPart::parse).transpose().wrap_err_with(at)?,
		};
		let messaged = match (r.messaged_at, r.messaged_channel.as_deref()) {
			(Some(at), Some(channel)) => Some((
				from_db(at)?,
				Messenger::parse(channel).wrap_err_with(|| format!("stored messenger of lead {}/{}", r.brand_id, r.lead_id))?,
			)),
			_ => None,
		};
		Ok(Self {
			booking,
			messaged,
			message_ref: r.message_ref,
			flow: r.flow,
			quoted_cents: r.quoted_cents,
			pricing_valid_from: r.pricing_valid_from,
			estimate_inputs: r.estimate_inputs.map(|j| j.0),
			stage: r.stage.parse().wrap_err_with(|| format!("stored stage of lead {}/{}", r.brand_id, r.lead_id))?,
			created_at: t(r.created_at)?,
			contacted_at: t(r.contacted_at)?,
			quoted_at: t(r.quoted_at)?,
			won_at: t(r.won_at)?,
			completed_at: t(r.completed_at)?,
			paid_at: t(r.paid_at)?,
			lost_at: t(r.lost_at)?,
			last_event_at: from_db(r.last_event_at)?,
			sort_at: from_db(r.sort_at)?,
			creation: r.creation_id.map(|event_id| Sealed {
				event_id,
				pii: r.pii_sealed.zip(r.data_key_fp),
			}),
			brand_id: r.brand_id,
			lead_id: r.lead_id,
			location_id: r.location_id,
			job_id: r.job_id,
			channel: r.channel,
			suspect: r.suspect,
			manual: r.manual,
			lost_reason: r.lost_reason,
		})
	}
}

// Macros, not consts, so every query stays a literal (`concat!`) that sqlx takes as audited.
macro_rules! lead_select {
	() => {
		"SELECT l.brand_id, l.lead_id, l.location_id, l.job_id, l.stage, l.channel, l.suspect, l.manual, \
		 l.created_at, l.contacted_at, l.quoted_at, l.won_at, l.completed_at, l.paid_at, l.lost_at, l.lost_reason, l.last_event_at, \
		 l.flow, l.quoted_cents, l.pricing_valid_from, l.estimate_inputs, \
		 l.booking_status, l.booking_provider, l.booking_start_at, l.booking_end_at, l.booking_external_ref, l.booking_match, \
		 l.booking_preferred_date, l.booking_preferred_part, l.message_ref, l.messaged_at, l.messaged_channel, \
		 COALESCE(l.created_at, l.last_event_at) AS sort_at, \
		 c.id AS creation_id, c.pii_sealed, c.data_key_fp \
		 FROM leads l \
		 LEFT JOIN events c ON c.id = ( \
		   SELECT e.id FROM events e \
		   WHERE e.brand_id = l.brand_id AND e.lead_id = l.lead_id AND e.type = 'lead.created' AND e.status = 'registered' \
		   ORDER BY e.received_at, e.id LIMIT 1 \
		 ) "
	};
}

/// Which leads to list.
#[derive(Clone, Debug, Default)]
pub struct LeadFilter {
	pub stage: Option<Stage>,
	pub brand: Option<BrandId>,
	pub location: Option<LocationId>,
	/// Only leads created before this and still waiting for their first contact.
	pub waiting_since_before: Option<Timestamp>,
	/// Only leads created at or after this.
	pub created_from: Option<Timestamp>,
	/// Only leads created before this.
	pub created_before: Option<Timestamp>,
	/// Only the leads the antispam doubted (`Some(true)`), or only the others (`Some(false)`).
	pub suspect: Option<bool>,
	/// Only the leads of this flow (`quote`, `estimate`, `fixed`).
	pub flow: Option<LeadFlow>,
	/// Only the leads whose booking stands here (`none`: no booking at all).
	pub booking: Option<BookingStatus>,
	/// Only the leads that came in through this channel.
	pub channel: Option<LeadChannel>,
	/// Only the leads carrying this messenger ref.
	pub message_ref: Option<MessageRef>,
	/// The last lead of the page before: `(sort_at, brand, lead)`.
	pub after: Option<(Timestamp, String, String)>,
	pub limit: i64,
}

/// Newest first, by creation.
pub async fn leads(conn: &mut SqliteConnection, f: &LeadFilter) -> eyre::Result<Vec<LeadRow>> {
	let (after_at, after_brand, after_lead) = match &f.after {
		Some((at, brand, lead)) => (Some(to_db(*at)), Some(brand.as_str()), Some(lead.as_str())),
		None => (None, None, None),
	};
	sqlx::query_as::<_, Row>(concat!(
		lead_select!(),
		"WHERE ($1 IS NULL OR l.stage = $1) AND ($2 IS NULL OR l.brand_id = $2) AND ($3 IS NULL OR l.location_id = $3) \
		 AND ($4 IS NULL OR (l.stage = 'created' AND l.contacted_at IS NULL AND l.created_at < $4)) \
		 AND ($5 IS NULL OR (COALESCE(l.created_at, l.last_event_at), l.brand_id, l.lead_id) < ($5, $6, $7)) \
		 AND ($9 IS NULL OR l.created_at >= $9) AND ($10 IS NULL OR l.created_at < $10) \
		 AND ($11 IS NULL OR (l.suspect IS NOT NULL) = $11) AND ($12 IS NULL OR l.flow = $12) \
		 AND ($13 IS NULL OR COALESCE(l.booking_status, 'none') = $13) \
		 AND ($14 IS NULL OR l.channel = $14) AND ($15 IS NULL OR l.message_ref = $15) \
		 ORDER BY sort_at DESC, l.brand_id DESC, l.lead_id DESC LIMIT $8"
	))
	.bind(f.stage.map(Stage::as_str))
	.bind(f.brand.as_ref().map(BrandId::as_str))
	.bind(f.location.as_ref().map(LocationId::as_str))
	.bind(f.waiting_since_before.map(to_db))
	.bind(after_at)
	.bind(after_brand)
	.bind(after_lead)
	.bind(f.limit)
	.bind(f.created_from.map(to_db))
	.bind(f.created_before.map(to_db))
	.bind(f.suspect)
	.bind(f.flow.map(LeadFlow::as_str))
	.bind(f.booking.map(BookingStatus::as_str))
	.bind(f.channel.map(LeadChannel::as_str))
	.bind(f.message_ref.as_ref().map(MessageRef::as_str))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing leads")?
	.into_iter()
	.map(LeadRow::try_from)
	.collect()
}

pub async fn lead(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId) -> eyre::Result<Option<LeadRow>> {
	sqlx::query_as::<_, Row>(concat!(lead_select!(), "WHERE l.brand_id = $1 AND l.lead_id = $2"))
		.bind(brand.as_str())
		.bind(lead.as_str())
		.fetch_optional(&mut *conn)
		.await
		.wrap_err_with(|| format!("reading lead {brand}/{lead}"))?
		.map(LeadRow::try_from)
		.transpose()
}

/// The lead a messenger ref names, in one query: the brand's newest carrying it (a ref is the
/// landing's, so two leads may share one), by creation, then by id for two created in the same
/// microsecond.
pub async fn lead_by_ref(conn: &mut SqliteConnection, brand: &BrandId, message_ref: &MessageRef) -> eyre::Result<Option<LeadRow>> {
	sqlx::query_as::<_, Row>(concat!(
		lead_select!(),
		"WHERE l.brand_id = $1 AND l.message_ref = $2 ORDER BY l.created_at DESC NULLS LAST, l.lead_id DESC LIMIT 1"
	))
	.bind(brand.as_str())
	.bind(message_ref.as_str())
	.fetch_optional(&mut *conn)
	.await
	.wrap_err_with(|| format!("finding the lead of ref {message_ref} of {brand}"))?
	.map(LeadRow::try_from)
	.transpose()
}

/// The lead's first `lead.messaged` the panel journaled: the one PostHog is told of, and what an
/// operator saying it again is answered with.
pub async fn first_message(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId) -> eyre::Result<Option<Uuid>> {
	sqlx::query_scalar(
		"SELECT id FROM events WHERE brand_id = $1 AND lead_id = $2 AND type = 'lead.messaged' AND status = 'registered' \
		 ORDER BY received_at, id LIMIT 1",
	)
	.bind(brand.as_str())
	.bind(lead.as_str())
	.fetch_optional(&mut *conn)
	.await
	.wrap_err_with(|| format!("finding the first message of lead {brand}/{lead}"))
}

/// One event of a lead, as its card shows it.
#[derive(Clone, Debug)]
pub struct EventRow {
	pub id: Uuid,
	pub r#type: String,
	pub type_version: i32,
	pub occurred_at: Timestamp,
	pub received_at: Timestamp,
	pub source_kind: String,
	pub source_id: String,
	pub job_id: Option<String>,
	pub status: String,
	pub status_reason: Option<String>,
	pub properties: Value,
	pub sealed: Sealed,
}

#[derive(sqlx::FromRow)]
struct EventDbRow {
	id: Uuid,
	r#type: String,
	type_version: i32,
	occurred_at: i64,
	received_at: i64,
	source_kind: String,
	source_id: String,
	job_id: Option<String>,
	status: String,
	status_reason: Option<String>,
	properties: Json<Value>,
	pii_sealed: Option<Vec<u8>>,
	data_key_fp: Option<Vec<u8>>,
}

/// Every event about a lead, whatever the registry made of it, in the order they happened.
pub async fn lead_events(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId) -> eyre::Result<Vec<EventRow>> {
	let rows = sqlx::query_as::<_, EventDbRow>(
		"SELECT id, type, type_version, occurred_at, received_at, source_kind, source_id, job_id, status, status_reason, properties, pii_sealed, data_key_fp \
		 FROM events WHERE brand_id = $1 AND lead_id = $2 ORDER BY occurred_at, id",
	)
	.bind(brand.as_str())
	.bind(lead.as_str())
	.fetch_all(&mut *conn)
	.await
	.wrap_err_with(|| format!("reading the events of lead {brand}/{lead}"))?;
	rows.into_iter()
		.map(|r| {
			Ok(EventRow {
				occurred_at: from_db(r.occurred_at)?,
				received_at: from_db(r.received_at)?,
				sealed: Sealed {
					event_id: r.id,
					pii: r.pii_sealed.zip(r.data_key_fp),
				},
				id: r.id,
				r#type: r.r#type,
				type_version: r.type_version,
				source_kind: r.source_kind,
				source_id: r.source_id,
				job_id: r.job_id,
				status: r.status,
				status_reason: r.status_reason,
				properties: r.properties.0,
			})
		})
		.collect()
}

/// Who journaled the event `id`, and about which lead: `(source_kind, source_id, lead_id)`.
pub async fn event_origin(conn: &mut SqliteConnection, id: Uuid) -> eyre::Result<Option<(String, String, Option<String>)>> {
	sqlx::query_as("SELECT source_kind, source_id, lead_id FROM events WHERE id = $1")
		.bind(id)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("looking up an event's origin")
}

/// Whether `attempt` is a `call.attempted` of this lead.
pub async fn is_call_attempt(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId, attempt: Uuid) -> eyre::Result<bool> {
	sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM calls WHERE event_id = $1 AND brand_id = $2 AND lead_id = $3 AND kind = 'attempted')")
		.bind(attempt)
		.bind(brand.as_str())
		.bind(lead.as_str())
		.fetch_one(&mut *conn)
		.await
		.wrap_err("looking up a call attempt")
}

/// A slice of the personal funnel: one location's, or every location's when both are `None`.
#[derive(Clone, Debug)]
pub struct FunnelRow {
	pub brand_id: Option<String>,
	pub location_id: Option<String>,
	pub totals: Totals,
}

/// The personal funnel's totals over the leads that came in from `from` to `to`, UTC days,
/// both included; for one brand or all.
pub async fn funnel(conn: &mut SqliteConnection, from: Date, to: Date, brand: Option<&BrandId>) -> eyre::Result<Totals> {
	Ok(funnel_rows(conn, from, to, brand, false).await?.pop().map(|r| r.totals).unwrap_or_default())
}

/// [`funnel`], one row per location (a lead without one is under `None`) when `by_location`,
/// else a single row, or none when no lead came in.
pub async fn funnel_rows(conn: &mut SqliteConnection, from: Date, to: Date, brand: Option<&BrandId>, by_location: bool) -> eyre::Result<Vec<FunnelRow>> {
	type Sums = (Option<String>, Option<String>, i64, i64, i64, i64, i64, i64, i64, i64);
	let rows: Vec<Sums> = sqlx::query_as(
		"SELECT CASE WHEN $4 THEN brand_id END, CASE WHEN $4 THEN location_id END, \
		 sum(leads), sum(contacted), sum(quoted), sum(won), \
		 sum(completed), sum(paid), sum(lost_now), sum(manual) \
		 FROM reporting_funnel_daily WHERE day BETWEEN $1 AND $2 AND ($3 IS NULL OR brand_id = $3) \
		 GROUP BY 1, 2 ORDER BY 1, 2 NULLS LAST",
	)
	.bind(day_to_db(from))
	.bind(day_to_db(to))
	.bind(brand.map(BrandId::as_str))
	.bind(by_location)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("summing the funnel")?;
	rows.into_iter()
		.map(|s| {
			Ok(FunnelRow {
				brand_id: s.0,
				location_id: s.1,
				totals: Totals {
					leads: count(s.2)?,
					contacted: count(s.3)?,
					quoted: count(s.4)?,
					won: count(s.5)?,
					completed: count(s.6)?,
					paid: count(s.7)?,
					lost: count(s.8)?,
					manual: count(s.9)?,
				},
			})
		})
		.collect()
}

fn count(v: i64) -> eyre::Result<u64> {
	u64::try_from(v).wrap_err("a negative count")
}

/// What the leads a funnel counts were paid, in one currency: summed as they are, never
/// converted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentSum {
	pub brand_id: Option<String>,
	pub location_id: Option<String>,
	pub currency: String,
	/// Minor units.
	pub billed: i64,
	/// Minor units.
	pub commission: i64,
	pub payments: u64,
}

/// The payments of the leads that came in from `from` to `to` — the funnel's cohort, whenever
/// they were paid — per currency, and per location when `by_location` (keyed as
/// [`funnel_rows`] keys its rows).
pub async fn funnel_payments(conn: &mut SqliteConnection, from: Date, to: Date, brand: Option<&BrandId>, by_location: bool) -> eyre::Result<Vec<PaymentSum>> {
	type Sums = (Option<String>, Option<String>, String, i64, i64, i64);
	let rows: Vec<Sums> = sqlx::query_as(
		"SELECT CASE WHEN $4 THEN l.brand_id END, CASE WHEN $4 THEN l.location_id END, p.currency, \
		 sum(p.billed), sum(p.commission), count(*) \
		 FROM payments p JOIN leads l ON l.brand_id = p.brand_id AND l.lead_id = p.lead_id \
		 WHERE date(l.created_at / 1000000, 'unixepoch') BETWEEN $1 AND $2 AND ($3 IS NULL OR l.brand_id = $3) \
		 GROUP BY 1, 2, 3 ORDER BY 1, 2 NULLS LAST, 3",
	)
	.bind(day_to_db(from))
	.bind(day_to_db(to))
	.bind(brand.map(BrandId::as_str))
	.bind(by_location)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("summing the payments")?;
	rows.into_iter()
		.map(|r| {
			Ok(PaymentSum {
				brand_id: r.0,
				location_id: r.1,
				currency: r.2,
				billed: r.3,
				commission: r.4,
				payments: count(r.5)?,
			})
		})
		.collect()
}

/// A place the panel knows, when its newest lead came in, and the state of its live settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceRow {
	pub brand_id: String,
	pub location_id: String,
	/// `None` while its leads have only been seen before their creation, or it has none.
	pub last_lead_at: Option<Timestamp>,
	/// Live settings with a field set at least.
	pub has_settings: bool,
	pub withdrawn: bool,
}

/// Every place the panel knows, by brand: the locations a lead names, and those registered by
/// hand or by an edit of their settings.
pub async fn places(conn: &mut SqliteConnection) -> eyre::Result<Vec<PlaceRow>> {
	let rows: Vec<(String, String, Option<i64>, bool, bool)> = sqlx::query_as(
		"WITH known AS ( \
		   SELECT brand_id, location_id FROM leads WHERE location_id IS NOT NULL \
		   UNION SELECT brand_id, location_id FROM places) \
		 SELECT k.brand_id, k.location_id, \
		   (SELECT max(l.created_at) FROM leads l WHERE l.brand_id = k.brand_id AND l.location_id = k.location_id), \
		   EXISTS (SELECT 1 FROM place_settings s, json_each(s.settings) WHERE s.brand_id = k.brand_id AND s.location_id = k.location_id), \
		   coalesce((SELECT p.withdrawn FROM places p WHERE p.brand_id = k.brand_id AND p.location_id = k.location_id), 0) \
		 FROM known k ORDER BY k.brand_id, k.location_id",
	)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing the places")?;
	rows.into_iter()
		.map(|(brand_id, location_id, at, has_settings, withdrawn)| {
			Ok(PlaceRow {
				brand_id,
				location_id,
				last_lead_at: at.map(from_db).transpose()?,
				has_settings,
				withdrawn,
			})
		})
		.collect()
}

/// How many leads are at each stage (the stages none is at are absent), and how many of
/// them wait for their first contact since before `waiting_since_before`.
pub async fn stage_counts(conn: &mut SqliteConnection, brand: Option<&BrandId>, location: Option<&LocationId>, waiting_since_before: Timestamp) -> eyre::Result<(Vec<(Stage, u64)>, u64)> {
	let rows: Vec<(String, i64, i64)> = sqlx::query_as(
		"SELECT stage, count(*), count(*) FILTER (WHERE stage = 'created' AND contacted_at IS NULL AND created_at < $3) \
		 FROM leads WHERE ($1 IS NULL OR brand_id = $1) AND ($2 IS NULL OR location_id = $2) GROUP BY stage",
	)
	.bind(brand.map(BrandId::as_str))
	.bind(location.map(LocationId::as_str))
	.bind(to_db(waiting_since_before))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("counting the leads by stage")?;
	let mut overdue = 0;
	let mut stages = Vec::with_capacity(rows.len());
	for (stage, n, late) in rows {
		stages.push((stage.parse().wrap_err_with(|| format!("stored stage {stage}"))?, count(n)?));
		overdue += count(late)?;
	}
	Ok((stages, overdue))
}

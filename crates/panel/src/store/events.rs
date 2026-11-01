//! The journal: `events`, append-only (the table's triggers refuse deletes and any update
//! but of `status` and `status_reason`), deduplicated by the event's id.

use eyre::WrapErr;
use jiff::Timestamp;
use panel_core::{
	event::{Subject, TypeKey},
	ids::{BrandId, EventId, JobId, LeadId, LocationId},
};
use sa_auth::SourceKind;
use serde_json::Value;
use sqlx::{SqliteConnection, types::Json};
use uuid::Uuid;

use super::{from_db, to_db};
use crate::wire::{Checked, Incoming, check};

/// Where an event stands with the registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
	Registered,
	Unregistered,
	Invalid,
}

impl Status {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Registered => "registered",
			Self::Unregistered => "unregistered",
			Self::Invalid => "invalid",
		}
	}

	fn parse(s: &str) -> eyre::Result<Self> {
		match s {
			"registered" => Ok(Self::Registered),
			"unregistered" => Ok(Self::Unregistered),
			"invalid" => Ok(Self::Invalid),
			other => eyre::bail!("stored event status {other:?}"),
		}
	}

	/// The status and its reason for what the registry says.
	pub fn of(checked: &Checked) -> (Self, Option<String>) {
		match checked {
			Checked::Registered(_) => (Self::Registered, None),
			Checked::Unregistered => (Self::Unregistered, None),
			Checked::Invalid(e) => (Self::Invalid, Some(e.0.clone())),
		}
	}
}

/// An event on its way into the journal.
pub struct NewEvent<'a> {
	pub incoming: &'a Incoming,
	/// The key that signed it; `None` for the panel's own events.
	pub key_id: Option<&'a str>,
	pub received_at: Timestamp,
	pub status: Status,
	/// The event's content MAC ([`crate::seal::DataKey::content_mac`]).
	pub content_mac: [u8; 32],
	/// Sealed PII and the fingerprint of the key that sealed it.
	pub pii: Option<(Vec<u8>, [u8; 32])>,
}

/// What became of an insert.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Inserted {
	New,
	/// This event is already in the journal.
	Duplicate,
	/// Another event is in the journal under this id.
	Conflict,
}

/// Inserts an event unless its id is taken.
pub async fn insert(conn: &mut SqliteConnection, e: &NewEvent<'_>) -> eyre::Result<Inserted> {
	let env = &e.incoming.envelope;
	let (pii_sealed, data_key_fp) = e.pii.as_ref().map(|(blob, fp)| (blob.as_slice(), fp.as_slice())).unzip();
	let inserted = sqlx::query(
		"INSERT INTO events (id, schema, type, type_version, occurred_at, received_at, source_kind, source_id, key_id, brand_id, location_id, lead_id, job_id, \
		 properties, pii_sealed, data_key_fp, content_mac, status) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) \
		 ON CONFLICT (id) DO NOTHING",
	)
	.bind(env.id.raw())
	.bind(panel_contracts::SCHEMA)
	.bind(&env.type_key.name)
	.bind(i32::try_from(env.type_key.version).wrap_err("type_version past i32")?)
	.bind(to_db(env.occurred_at))
	.bind(to_db(e.received_at))
	.bind(env.source.kind.as_str())
	.bind(&env.source.id)
	.bind(e.key_id)
	.bind(env.subject.brand_id.as_str())
	.bind(env.subject.location_id.as_ref().map(LocationId::as_str))
	.bind(env.subject.lead_id.as_ref().map(LeadId::as_str))
	.bind(env.subject.job_id.as_ref().map(JobId::as_str))
	.bind(Json(&e.incoming.properties))
	.bind(pii_sealed)
	.bind(data_key_fp)
	.bind(e.content_mac.as_slice())
	.bind(e.status.as_str())
	.execute(&mut *conn)
	.await
	.wrap_err("inserting an event")?
	.rows_affected();
	if inserted == 1 {
		return Ok(Inserted::New);
	}
	let stored: Vec<u8> = sqlx::query_scalar("SELECT content_mac FROM events WHERE id = $1")
		.bind(env.id.raw())
		.fetch_one(&mut *conn)
		.await
		.wrap_err("reading the event an id is taken by")?;
	Ok(if stored == e.content_mac { Inserted::Duplicate } else { Inserted::Conflict })
}

/// An event as the journal holds it, less its PII.
#[derive(Clone, Debug)]
pub struct Stored {
	pub id: EventId,
	pub type_key: TypeKey,
	pub occurred_at: Timestamp,
	pub received_at: Timestamp,
	pub source_kind: SourceKind,
	pub subject: Subject,
	pub properties: Value,
	pub status: Status,
	pub status_reason: Option<String>,
}

impl Stored {
	/// What the registry makes of it now.
	pub fn check(&self) -> Checked {
		check(&self.type_key, self.source_kind, &self.properties, &self.subject)
	}
}

#[derive(sqlx::FromRow)]
struct Row {
	id: Uuid,
	r#type: String,
	type_version: i32,
	occurred_at: i64,
	received_at: i64,
	source_kind: String,
	brand_id: String,
	location_id: Option<String>,
	lead_id: Option<String>,
	job_id: Option<String>,
	properties: Json<Value>,
	status: String,
	status_reason: Option<String>,
}

impl TryFrom<Row> for Stored {
	type Error = eyre::Report;

	fn try_from(r: Row) -> eyre::Result<Self> {
		let at = || format!("stored event {}", r.id);
		let version = u32::try_from(r.type_version).wrap_err_with(at)?;
		Ok(Self {
			id: EventId::from_raw(r.id),
			type_key: TypeKey::parse(&r.r#type, version).wrap_err_with(at)?,
			occurred_at: from_db(r.occurred_at)?,
			received_at: from_db(r.received_at)?,
			source_kind: r.source_kind.parse().wrap_err_with(at)?,
			subject: Subject {
				brand_id: BrandId::parse(&r.brand_id).wrap_err_with(at)?,
				location_id: r.location_id.as_deref().map(LocationId::parse).transpose().wrap_err_with(at)?,
				lead_id: r.lead_id.as_deref().map(LeadId::parse).transpose().wrap_err_with(at)?,
				job_id: r.job_id.as_deref().map(JobId::parse).transpose().wrap_err_with(at)?,
			},
			properties: r.properties.0,
			status: Status::parse(&r.status)?,
			status_reason: r.status_reason,
		})
	}
}

// A macro, not a const, so every query stays a literal (`concat!`) that sqlx takes as audited.
macro_rules! columns {
	() => {
		"id, type, type_version, occurred_at, received_at, source_kind, brand_id, location_id, lead_id, job_id, properties, status, status_reason"
	};
}

/// A lead's registered events.
pub async fn of_lead(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId) -> eyre::Result<Vec<Stored>> {
	sqlx::query_as::<_, Row>(concat!(
		"SELECT ",
		columns!(),
		" FROM events WHERE brand_id = $1 AND lead_id = $2 AND status = 'registered' ORDER BY occurred_at, id"
	))
	.bind(brand.as_str())
	.bind(lead.as_str())
	.fetch_all(&mut *conn)
	.await
	.wrap_err_with(|| format!("reading the events of lead {brand}/{lead}"))?
	.into_iter()
	.map(Stored::try_from)
	.collect()
}

/// The registered events of a provider's booking (`booking_events` names them).
pub async fn of_booking(conn: &mut SqliteConnection, brand: &BrandId, provider: &str, external_ref: &str) -> eyre::Result<Vec<Stored>> {
	sqlx::query_as::<_, Row>(
		"SELECT e.id, e.type, e.type_version, e.occurred_at, e.received_at, e.source_kind, e.brand_id, e.location_id, e.lead_id, e.job_id, \
		 e.properties, e.status, e.status_reason FROM booking_events b JOIN events e ON e.id = b.event_id \
		 WHERE b.brand_id = $1 AND b.provider = $2 AND b.external_ref = $3 AND e.status = 'registered' ORDER BY e.occurred_at, e.id",
	)
	.bind(brand.as_str())
	.bind(provider)
	.bind(external_ref)
	.fetch_all(&mut *conn)
	.await
	.wrap_err_with(|| format!("reading the events of booking {brand}/{provider}/{external_ref}"))?
	.into_iter()
	.map(Stored::try_from)
	.collect()
}

/// A brand's registered experiment events.
pub async fn of_experiments(conn: &mut SqliteConnection, brand: &BrandId) -> eyre::Result<Vec<Stored>> {
	sqlx::query_as::<_, Row>(concat!(
		"SELECT ",
		columns!(),
		" FROM events WHERE brand_id = $1 AND type IN ('experiments.declared', 'experiment.configured') AND status = 'registered' \
		 ORDER BY occurred_at, id"
	))
	.bind(brand.as_str())
	.fetch_all(&mut *conn)
	.await
	.wrap_err_with(|| format!("reading the experiment events of {brand}"))?
	.into_iter()
	.map(Stored::try_from)
	.collect()
}

/// The next page of the whole journal in `(occurred_at, id)` order, after `after`.
pub async fn page(conn: &mut SqliteConnection, after: Option<(Timestamp, EventId)>, limit: i64) -> eyre::Result<Vec<Stored>> {
	let rows = match after {
		None =>
			sqlx::query_as::<_, Row>(concat!("SELECT ", columns!(), " FROM events ORDER BY occurred_at, id LIMIT $1"))
				.bind(limit)
				.fetch_all(&mut *conn)
				.await,
		Some((at, id)) =>
			sqlx::query_as::<_, Row>(concat!(
				"SELECT ",
				columns!(),
				" FROM events WHERE (occurred_at, id) > ($1, $2) ORDER BY occurred_at, id LIMIT $3"
			))
			.bind(to_db(at))
			.bind(id.raw())
			.bind(limit)
			.fetch_all(&mut *conn)
			.await,
	}
	.wrap_err("reading a page of the journal")?;
	rows.into_iter().map(Stored::try_from).collect()
}

/// Records what the registry now says of a stored event. The one update the journal allows.
pub async fn set_status(conn: &mut SqliteConnection, id: EventId, status: Status, reason: Option<&str>) -> eyre::Result<()> {
	sqlx::query("UPDATE events SET status = $2, status_reason = $3 WHERE id = $1")
		.bind(id.raw())
		.bind(status.as_str())
		.bind(reason)
		.execute(&mut *conn)
		.await
		.wrap_err_with(|| format!("updating the status of event {}", id.raw()))?;
	Ok(())
}

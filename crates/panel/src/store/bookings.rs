//! Booking's tables: `booking_events` (one row per registered booking event), `bookings` (a
//! provider's booking as it stands, folded from its events), and `booking_sync` (a pull
//! adapter's cursor and lease).
//!
//! A provider's booking is recomputed whole from its events on every one of them, inside the
//! transaction that journaled it, like a lead ([`super::projections`]); so is every lead it
//! joins or leaves.

use eyre::WrapErr;
use jiff::Timestamp;
use panel_core::{
	booking::{self, BookingItem, BookingMatch, ExternalBooking, ExternalItem, Provider},
	fact::Fact,
	ids::{BrandId, EventId, LeadId},
	lead::Recorded,
};
use sqlx::SqliteConnection;
use uuid::Uuid;

use super::{
	events::{self, Stored},
	from_db,
	projections::registered,
	to_db,
};

/// The id the API names a provider's booking by: derived from its brand, provider and ref.
pub fn booking_id(brand: &BrandId, provider: Provider, external_ref: &str) -> Uuid {
	crate::derived_id(b"sa-panel/booking/v1/", &[brand.as_str().as_bytes(), provider.as_str().as_bytes(), external_ref.as_bytes()])
}

/// A provider's booking a fact is about: `(provider, external_ref)`.
pub fn external_key(fact: &Fact) -> Option<(Provider, &str)> {
	match fact {
		Fact::BookingCreated { provider, external_ref, .. } | Fact::BookingCanceled { provider, external_ref, .. } | Fact::BookingAttached { provider, external_ref } =>
			Some((*provider, external_ref.as_str())),
		_ => None,
	}
}

/// The event's `booking_events` row, if it is a booking event. Idempotent.
pub async fn insert_event_row(conn: &mut SqliteConnection, e: &Recorded) -> eyre::Result<()> {
	let (kind, provider, external_ref, status, start): (&str, Option<Provider>, Option<&str>, Option<&str>, Option<Timestamp>) = match &e.fact {
		Fact::BookingRequested { provider, .. } => ("requested", Some(*provider), None, None, None),
		Fact::BookingCreated {
			provider, external_ref, start_at, ..
		} => ("created", Some(*provider), Some(external_ref), None, Some(*start_at)),
		Fact::BookingCanceled { provider, external_ref, .. } => ("canceled", Some(*provider), Some(external_ref), None, None),
		Fact::BookingSet { start_at, .. } => ("set", Some(Provider::Manual), None, None, Some(*start_at)),
		Fact::BookingStatusChanged(c) => ("status_changed", None, None, Some(c.as_str()), None),
		Fact::BookingCleared => ("cleared", None, None, None, None),
		Fact::BookingAttached { provider, external_ref } => ("attached", Some(*provider), Some(external_ref), None, None),
		Fact::LeadCreated { .. }
		| Fact::LeadMessaged { .. }
		| Fact::LeadContacted { .. }
		| Fact::LeadQuoted { .. }
		| Fact::JobWon
		| Fact::LeadLost { .. }
		| Fact::JobCompleted
		| Fact::PaymentReceived { .. }
		| Fact::CallAttempted
		| Fact::CallLogged { .. }
		| Fact::RetiredCount
		| Fact::ExperimentsDeclared(_)
		| Fact::ExperimentConfigured { .. } => return Ok(()),
	};
	sqlx::query(
		"INSERT INTO booking_events (event_id, brand_id, kind, provider, external_ref, lead_id, status, start_at, occurred_at) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (event_id) DO NOTHING",
	)
	.bind(e.id.raw())
	.bind(e.subject.brand_id.as_str())
	.bind(kind)
	.bind(provider.map(Provider::as_str))
	.bind(external_ref)
	.bind(e.subject.lead_id.as_ref().map(LeadId::as_str))
	.bind(status)
	.bind(start.map(to_db))
	.bind(to_db(e.occurred_at))
	.execute(&mut *conn)
	.await
	.wrap_err("projecting a booking event")?;
	Ok(())
}

// A macro, not a const, so every query stays a literal (`concat!`) that sqlx takes as audited.
macro_rules! booking_columns {
	() => {
		"id, brand_id, provider, external_ref, lead_id, match, status, start_at, end_at, booked_at, created_event_id, last_event_at"
	};
}

/// A provider's booking as `bookings` holds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookingRow {
	pub id: Uuid,
	pub brand_id: BrandId,
	pub provider: Provider,
	pub external_ref: String,
	/// The lead and how it was found; `None` while unmatched.
	pub lead: Option<(LeadId, BookingMatch)>,
	/// `true`: booked; `false`: canceled.
	pub booked: bool,
	pub start_at: Timestamp,
	pub end_at: Option<Timestamp>,
	pub booked_at: Option<Timestamp>,
	/// The latest `booking.created`, whose PII is the attendee's.
	pub created_event_id: Uuid,
	pub last_event_at: Timestamp,
}

type Row = (Uuid, String, String, String, Option<String>, Option<String>, String, i64, Option<i64>, Option<i64>, Uuid, i64);

fn row((id, brand, provider, external_ref, lead, matched, status, start, end, booked_at, created, last): Row) -> eyre::Result<BookingRow> {
	let at = || format!("stored booking {id}");
	let lead = match (lead, matched) {
		(Some(l), Some(m)) => Some((LeadId::parse(&l).wrap_err_with(at)?, BookingMatch::parse(&m).wrap_err_with(at)?)),
		_ => None,
	};
	Ok(BookingRow {
		id,
		brand_id: BrandId::parse(&brand).wrap_err_with(at)?,
		provider: Provider::parse(&provider).wrap_err_with(at)?,
		external_ref,
		lead,
		booked: status == "booked",
		start_at: from_db(start)?,
		end_at: end.map(from_db).transpose()?,
		booked_at: booked_at.map(from_db).transpose()?,
		created_event_id: created,
		last_event_at: from_db(last)?,
	})
}

pub async fn by_key(conn: &mut SqliteConnection, brand: &BrandId, provider: Provider, external_ref: &str) -> eyre::Result<Option<BookingRow>> {
	sqlx::query_as::<_, Row>(concat!(
		"SELECT ",
		booking_columns!(),
		" FROM bookings WHERE brand_id = $1 AND provider = $2 AND external_ref = $3"
	))
	.bind(brand.as_str())
	.bind(provider.as_str())
	.bind(external_ref)
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("reading a booking")?
	.map(row)
	.transpose()
}

pub async fn by_id(conn: &mut SqliteConnection, id: Uuid) -> eyre::Result<Option<BookingRow>> {
	sqlx::query_as::<_, Row>(concat!("SELECT ", booking_columns!(), " FROM bookings WHERE id = $1"))
		.bind(id)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a booking")?
		.map(row)
		.transpose()
}

/// The bookings no lead was found for, the next slot first; one brand or every one.
pub async fn unmatched(conn: &mut SqliteConnection, brand: Option<&BrandId>, limit: i64) -> eyre::Result<Vec<BookingRow>> {
	sqlx::query_as::<_, Row>(concat!(
		"SELECT ",
		booking_columns!(),
		" FROM bookings WHERE lead_id IS NULL AND ($1 IS NULL OR brand_id = $1) ORDER BY start_at, id LIMIT $2"
	))
	.bind(brand.map(BrandId::as_str))
	.bind(limit)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing the bookings without a lead")?
	.into_iter()
	.map(row)
	.collect()
}

/// The registered events of a provider's booking, as facts, in journal order.
async fn events_of(conn: &mut SqliteConnection, brand: &BrandId, provider: Provider, external_ref: &str) -> eyre::Result<Vec<Recorded>> {
	let stored: Vec<Stored> = events::of_booking(conn, brand, provider.as_str(), external_ref).await?;
	Ok(stored.into_iter().filter_map(registered).collect())
}

/// What recomputing a provider's booking changed: the lead it was joined to before and after,
/// and whether it was or is without one.
#[derive(Clone, Debug, Default)]
pub struct Recomputed {
	pub before: Option<LeadId>,
	pub after: Option<LeadId>,
	pub unmatched_before: bool,
	pub unmatched_after: bool,
}

/// Recomputes a provider's booking from its events. Inside a write transaction.
pub async fn recompute(conn: &mut SqliteConnection, brand: &BrandId, provider: Provider, external_ref: &str) -> eyre::Result<Recomputed> {
	let before = by_key(conn, brand, provider, external_ref).await?;
	let mut items: Vec<(Timestamp, EventId, ExternalItem)> = events_of(conn, brand, provider, external_ref)
		.await?
		.into_iter()
		.filter_map(|e| {
			let item = match e.fact {
				Fact::BookingCreated {
					start_at,
					end_at,
					matched,
					booked_at,
					..
				} => ExternalItem::Created {
					start: start_at,
					end: end_at,
					booked_at,
					lead: e.subject.lead_id.map(|l| l.as_str().to_owned()).zip(matched),
				},
				Fact::BookingCanceled { .. } => ExternalItem::Canceled,
				Fact::BookingAttached { .. } => ExternalItem::Attached {
					lead: e.subject.lead_id?.as_str().to_owned(),
				},
				_ => return None,
			};
			Some((e.occurred_at, e.id, item))
		})
		.collect();
	let after = booking::fold_external(&mut items);
	let id = booking_id(brand, provider, external_ref);
	match &after {
		Some(b) => upsert(conn, id, brand, provider, external_ref, b).await?,
		None => {
			sqlx::query("DELETE FROM bookings WHERE id = $1")
				.bind(id)
				.execute(&mut *conn)
				.await
				.wrap_err("dropping a booking with no creation")?;
		}
	}
	let lead_after = after.as_ref().and_then(|b| b.lead.as_ref()).map(|(l, _)| LeadId::parse(l)).transpose()?;
	Ok(Recomputed {
		unmatched_before: before.as_ref().is_some_and(|b| b.lead.is_none()),
		unmatched_after: after.is_some() && lead_after.is_none(),
		before: before.and_then(|b| b.lead.map(|(l, _)| l)),
		after: lead_after,
	})
}

async fn upsert(conn: &mut SqliteConnection, id: Uuid, brand: &BrandId, provider: Provider, external_ref: &str, b: &ExternalBooking) -> eyre::Result<()> {
	let status = if b.canceled { "canceled" } else { "booked" };
	sqlx::query(
		"INSERT INTO bookings (id, brand_id, provider, external_ref, lead_id, match, status, start_at, end_at, booked_at, created_event_id, first_event_id, \
		 last_event_id, last_event_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
		 ON CONFLICT (id) DO UPDATE SET lead_id = EXCLUDED.lead_id, match = EXCLUDED.match, status = EXCLUDED.status, start_at = EXCLUDED.start_at, \
		 end_at = EXCLUDED.end_at, booked_at = EXCLUDED.booked_at, \
		 created_event_id = EXCLUDED.created_event_id, first_event_id = EXCLUDED.first_event_id, last_event_id = EXCLUDED.last_event_id, \
		 last_event_at = EXCLUDED.last_event_at",
	)
	.bind(id)
	.bind(brand.as_str())
	.bind(provider.as_str())
	.bind(external_ref)
	.bind(b.lead.as_ref().map(|(l, _)| l.as_str()))
	.bind(b.lead.as_ref().map(|(_, m)| m.as_str()))
	.bind(status)
	.bind(to_db(b.start))
	.bind(b.end.map(to_db))
	.bind(b.booked_at.map(to_db))
	.bind(b.created_event.raw())
	.bind(b.first_event.raw())
	.bind(b.last_event.raw())
	.bind(to_db(b.last_event_at))
	.execute(&mut *conn)
	.await
	.wrap_err("writing a booking")?;
	Ok(())
}

/// The providers' bookings joined to a lead, each event of each as a [`BookingItem`] its fold
/// reads, with the match the booking has now.
pub async fn items_of_lead(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId) -> eyre::Result<Vec<(Timestamp, EventId, BookingItem)>> {
	let joined: Vec<(String, String, String)> = sqlx::query_as("SELECT provider, external_ref, match FROM bookings WHERE brand_id = $1 AND lead_id = $2")
		.bind(brand.as_str())
		.bind(lead.as_str())
		.fetch_all(&mut *conn)
		.await
		.wrap_err("reading a lead's bookings")?;
	let mut items = Vec::new();
	for (provider, external_ref, matched) in joined {
		let provider = Provider::parse(&provider).wrap_err("a stored booking's provider")?;
		let matched = BookingMatch::parse(&matched).wrap_err("a stored booking's match")?;
		for e in events_of(conn, brand, provider, &external_ref).await? {
			let slot = match e.fact {
				Fact::BookingCreated { start_at, end_at, .. } => Some((start_at, end_at)),
				Fact::BookingCanceled { .. } => None,
				_ => continue,
			};
			items.push((
				e.occurred_at,
				e.id,
				BookingItem::External {
					provider,
					external_ref: external_ref.clone(),
					slot,
					matched,
				},
			));
		}
	}
	Ok(items)
}

/// A lead of the brand created within `[from, to]`, with the sealed PII of its creation: what
/// a booking's attendee is compared with.
#[derive(Clone, Debug)]
pub struct Candidate {
	pub lead_id: LeadId,
	pub creation: Uuid,
	pub pii: Option<(Vec<u8>, Vec<u8>)>,
}

pub async fn candidates(conn: &mut SqliteConnection, brand: &BrandId, from: Timestamp, to: Timestamp) -> eyre::Result<Vec<Candidate>> {
	/// The lead, its creation, the creation's sealed PII and the key's fingerprint.
	type R = (String, Uuid, Option<Vec<u8>>, Option<Vec<u8>>);
	let rows: Vec<R> = sqlx::query_as(
		"SELECT l.lead_id, c.id, c.pii_sealed, c.data_key_fp FROM leads l \
		 JOIN events c ON c.id = ( \
		   SELECT e.id FROM events e \
		   WHERE e.brand_id = l.brand_id AND e.lead_id = l.lead_id AND e.type = 'lead.created' AND e.status = 'registered' \
		   ORDER BY e.received_at, e.id LIMIT 1 \
		 ) \
		 WHERE l.brand_id = $1 AND l.created_at >= $2 AND l.created_at <= $3 ORDER BY l.created_at",
	)
	.bind(brand.as_str())
	.bind(to_db(from))
	.bind(to_db(to))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("finding the leads a booking may be")?;
	rows.into_iter()
		.map(|(lead, creation, pii, fp)| {
			Ok(Candidate {
				lead_id: LeadId::parse(&lead).wrap_err("a stored lead id")?,
				creation,
				pii: pii.zip(fp),
			})
		})
		.collect()
}

/// Whether the brand has a lead by this id.
pub async fn lead_exists(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId) -> eyre::Result<bool> {
	let n: i64 = sqlx::query_scalar("SELECT count(*) FROM leads WHERE brand_id = $1 AND lead_id = $2")
		.bind(brand.as_str())
		.bind(lead.as_str())
		.fetch_one(&mut *conn)
		.await
		.wrap_err("looking a lead up")?;
	Ok(n > 0)
}

/// Every provider's booking the journal names, for a rebuild.
pub async fn all_keys(conn: &mut SqliteConnection) -> eyre::Result<Vec<(BrandId, Provider, String)>> {
	let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT DISTINCT brand_id, provider, external_ref FROM booking_events WHERE external_ref IS NOT NULL ORDER BY 1, 2, 3")
		.fetch_all(&mut *conn)
		.await
		.wrap_err("listing the providers' bookings")?;
	rows.into_iter()
		.map(|(b, p, r)| Ok((BrandId::parse(&b).wrap_err("a stored brand")?, Provider::parse(&p).wrap_err("a stored provider")?, r)))
		.collect()
}

// ── the Telegram rule `booked` ─────────────────────────────────────────────────────────

/// A booking event the rule `booked` has not told of yet.
#[derive(Clone, Debug)]
pub struct Told {
	pub event_id: Uuid,
	pub brand_id: String,
	/// created | set | canceled | status_changed (canceled only).
	pub kind: String,
	pub provider: Option<String>,
	/// The lead it is about now: a provider's booking's current lead, else the event's.
	pub lead_id: Option<String>,
	pub start_at: Option<Timestamp>,
	/// A `created` of a booking created before: a move.
	pub moved: bool,
	/// The operator who did it, for an operator's event.
	pub by: Option<Uuid>,
}

/// Bookings made, moved and canceled, journaled at or after `since`, not yet told of.
pub async fn to_tell(conn: &mut SqliteConnection, since: Timestamp, limit: i64) -> eyre::Result<Vec<Told>> {
	type R = (Uuid, String, String, Option<String>, Option<String>, Option<i64>, bool, Option<String>);
	let rows: Vec<R> = sqlx::query_as(
		"SELECT be.event_id, be.brand_id, be.kind, be.provider, COALESCE(bk.lead_id, be.lead_id), COALESCE(be.start_at, bk.start_at), \
		 be.kind = 'created' AND EXISTS (SELECT 1 FROM booking_events p JOIN events pe ON pe.id = p.event_id \
		   WHERE p.brand_id = be.brand_id AND p.provider = be.provider AND p.external_ref = be.external_ref AND p.kind = 'created' \
		   AND (p.occurred_at, pe.received_at, pe.id) < (be.occurred_at, e.received_at, e.id)), \
		 CASE WHEN e.source_kind = 'panel' THEN e.source_id END \
		 FROM booking_events be JOIN events e ON e.id = be.event_id \
		 LEFT JOIN bookings bk ON bk.brand_id = be.brand_id AND bk.provider = be.provider AND bk.external_ref = be.external_ref \
		 WHERE e.received_at >= $1 AND e.status = 'registered' \
		 AND (be.kind IN ('created', 'set', 'canceled') OR (be.kind = 'status_changed' AND be.status = 'canceled')) \
		 AND NOT EXISTS (SELECT 1 FROM telegram_fanout t WHERE t.rule = 'booked' AND t.event_id = be.event_id) \
		 ORDER BY e.received_at, e.id LIMIT $2",
	)
	.bind(to_db(since))
	.bind(limit)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("finding bookings to tell of")?;
	rows.into_iter()
		.map(|(event_id, brand_id, kind, provider, lead_id, start, moved, by)| {
			Ok(Told {
				event_id,
				brand_id,
				kind,
				provider,
				lead_id,
				start_at: start.map(from_db).transpose()?,
				moved,
				by: by.and_then(|id| Uuid::parse_str(&id).ok()),
			})
		})
		.collect()
}

// ── a pull adapter's sync ───────────────────────────────────────────────────────────────

/// Takes a brand's sync for `holder` until `until`, if no one holds it and it is due: last
/// finished at or before `due_before`, last tried at or before `retry_before`. The cursor to
/// pull from, when taken (the inner `None`: a full pull).
#[expect(clippy::too_many_arguments, reason = "the lease's key and its times, each named at the call site")]
pub async fn sync_lease(
	conn: &mut SqliteConnection,
	provider: Provider,
	brand: &BrandId,
	holder: Uuid,
	now: Timestamp,
	until: Timestamp,
	due_before: Timestamp,
	retry_before: Timestamp,
) -> eyre::Result<Option<Option<String>>> {
	sqlx::query("INSERT INTO booking_sync (provider, brand_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
		.bind(provider.as_str())
		.bind(brand.as_str())
		.execute(&mut *conn)
		.await
		.wrap_err("registering a brand's sync")?;
	let taken: Option<(Option<String>,)> = sqlx::query_as(
		"UPDATE booking_sync SET holder = $3, leased_until = $5, attempted_at = $4 \
		 WHERE provider = $1 AND brand_id = $2 \
		 AND (holder IS NULL OR holder = $3 OR leased_until IS NULL OR leased_until <= $4) \
		 AND (synced_at IS NULL OR synced_at <= $6) AND (attempted_at IS NULL OR attempted_at <= $7) \
		 RETURNING cursor",
	)
	.bind(provider.as_str())
	.bind(brand.as_str())
	.bind(holder)
	.bind(to_db(now))
	.bind(to_db(until))
	.bind(to_db(due_before))
	.bind(to_db(retry_before))
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("leasing a brand's sync")?;
	Ok(taken.map(|(c,)| c))
}

/// Lets a brand's sync go. `done`: the pull finished at `now`, and `cursor` is where the next
/// one starts.
pub async fn sync_release(conn: &mut SqliteConnection, provider: Provider, brand: &BrandId, holder: Uuid, now: Timestamp, done: Option<Option<&str>>) -> eyre::Result<()> {
	let (finished, cursor) = match done {
		Some(cursor) => (true, cursor),
		None => (false, None),
	};
	sqlx::query(
		"UPDATE booking_sync SET holder = NULL, leased_until = NULL, synced_at = CASE WHEN $4 THEN $5 ELSE synced_at END, \
		 cursor = CASE WHEN $4 THEN $6 ELSE cursor END WHERE provider = $1 AND brand_id = $2 AND holder = $3",
	)
	.bind(provider.as_str())
	.bind(brand.as_str())
	.bind(holder)
	.bind(finished)
	.bind(to_db(now))
	.bind(cursor)
	.execute(&mut *conn)
	.await
	.wrap_err("releasing a brand's sync")?;
	Ok(())
}

/// A brand's sync: its cursor, and when a pull last finished.
pub type SyncState = (Option<String>, Option<Timestamp>);

/// Where a brand's sync stands.
pub async fn sync_state(conn: &mut SqliteConnection, provider: Provider, brand: &BrandId) -> eyre::Result<Option<SyncState>> {
	let row: Option<(Option<String>, Option<i64>)> = sqlx::query_as("SELECT cursor, synced_at FROM booking_sync WHERE provider = $1 AND brand_id = $2")
		.bind(provider.as_str())
		.bind(brand.as_str())
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a brand's sync")?;
	row.map(|(c, at)| Ok((c, at.map(from_db).transpose()?))).transpose()
}

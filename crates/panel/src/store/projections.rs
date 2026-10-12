//! The projections — `leads`, `calls`, `payments`, the brands' [`super::experiments`], and
//! the bookings of [`super::bookings`] — derived from registered events only.
//!
//! A lead is never patched: every event about it recomputes its row from all of its
//! events ([`panel_core::lead::fold`]), inside the transaction that journaled the event. That
//! transaction holds the database's one write lock ([`super::begin_write`]), so two events of
//! one lead arriving at once cannot each compute without the other. The rebuild calls the
//! very same function, which is why it lands on the same state.

use eyre::WrapErr;
use panel_core::{
	booking::{self, BookingItem, BookingState},
	fact::Fact,
	ids::{BrandId, EventId, JobId, LeadId, LocationId},
	lead::{self, LeadState, Recorded},
};
use sqlx::{SqliteConnection, types::Json};

use super::{
	bookings,
	events::{self, Stored},
	to_db,
};
use crate::wire::Checked;

/// What projecting an event changed that a screen reads beyond the event itself.
#[derive(Clone, Debug, Default)]
pub struct Applied {
	/// Every lead recomputed: the event's, and those a provider's booking left or joined.
	pub leads: Vec<LeadId>,
	/// The bookings without a lead changed: one came, went, or was joined to a lead.
	pub unmatched: bool,
	/// The event's own lead as it is now, when the event names one that has a row.
	pub lead: Option<LeadState>,
}

/// Projects one registered event: its own row, its brand's experiments, the provider's
/// booking it is about, and every lead that changes with them.
pub async fn apply(conn: &mut SqliteConnection, event: &Recorded) -> eyre::Result<Applied> {
	insert_row(conn, event).await?;
	let brand = &event.subject.brand_id;
	if is_experiment(&event.fact) {
		super::experiments::recompute(conn, brand).await?;
	}
	let mut applied = Applied::default();
	if let Some((provider, external_ref)) = bookings::external_key(&event.fact) {
		let r = bookings::recompute(conn, brand, provider, external_ref).await?;
		applied.unmatched = r.unmatched_before || r.unmatched_after;
		for lead in [r.before, r.after].into_iter().flatten() {
			if !applied.leads.contains(&lead) {
				applied.leads.push(lead);
			}
		}
	}
	if let Some(lead) = &event.subject.lead_id
		&& !applied.leads.contains(lead)
	{
		applied.leads.push(lead.clone());
	}
	for lead in &applied.leads {
		let state = recompute_lead(conn, brand, lead).await?;
		if event.subject.lead_id.as_ref() == Some(lead) {
			applied.lead = state;
		}
	}
	Ok(applied)
}

/// Whether a fact is about the brand's experiments, which are folded per brand.
pub fn is_experiment(fact: &Fact) -> bool {
	matches!(fact, Fact::ExperimentsDeclared(_) | Fact::ExperimentConfigured { .. })
}

/// The event's own row, if its type has a table: a call, a payment. Idempotent.
pub async fn insert_row(conn: &mut SqliteConnection, e: &Recorded) -> eyre::Result<()> {
	let lead = e.subject.lead_id.as_ref().map(LeadId::as_str);
	let location = e.subject.location_id.as_ref().map(LocationId::as_str);
	let manual = e.source_kind.is_manual();
	let at = to_db(e.occurred_at);
	match &e.fact {
		Fact::CallAttempted | Fact::CallLogged { .. } => {
			let (kind, outcome, attempt_id) = match &e.fact {
				Fact::CallLogged { outcome, attempt_id } => ("logged", Some(outcome.as_str()), attempt_id.as_deref()),
				_ => ("attempted", None, None),
			};
			sqlx::query(
				"INSERT INTO calls (event_id, brand_id, lead_id, location_id, kind, outcome, attempt_id, occurred_at, manual) \
				 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (event_id) DO NOTHING",
			)
			.bind(e.id.raw())
			.bind(e.subject.brand_id.as_str())
			.bind(lead)
			.bind(location)
			.bind(kind)
			.bind(outcome)
			.bind(attempt_id)
			.bind(at)
			.bind(manual)
			.execute(&mut *conn)
			.await
			.wrap_err("projecting a call")?;
		}
		Fact::PaymentReceived { billed, commission } => {
			sqlx::query(
				"INSERT INTO payments (event_id, brand_id, lead_id, location_id, job_id, billed, commission, currency, occurred_at, manual) \
				 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (event_id) DO NOTHING",
			)
			.bind(e.id.raw())
			.bind(e.subject.brand_id.as_str())
			.bind(lead)
			.bind(location)
			.bind(e.subject.job_id.as_ref().map(JobId::as_str))
			.bind(billed.minor)
			.bind(commission)
			.bind(billed.currency.as_str())
			.bind(at)
			.bind(manual)
			.execute(&mut *conn)
			.await
			.wrap_err("projecting a payment")?;
		}
		Fact::BookingRequested { .. }
		| Fact::BookingCreated { .. }
		| Fact::BookingCanceled { .. }
		| Fact::BookingSet { .. }
		| Fact::BookingStatusChanged(_)
		| Fact::BookingCleared
		| Fact::BookingAttached { .. } => bookings::insert_event_row(conn, e).await?,
		Fact::LeadCreated { .. }
		| Fact::LeadMessaged { .. }
		| Fact::LeadContacted { .. }
		| Fact::LeadQuoted { .. }
		| Fact::JobWon
		| Fact::LeadLost { .. }
		| Fact::JobCompleted
		| Fact::RetiredCount
		| Fact::ExperimentsDeclared(_)
		| Fact::ExperimentConfigured { .. } => {}
	}
	Ok(())
}

/// Recomputes a lead's row from all its registered events, and answers it. Must run inside a
/// write transaction ([`super::begin_write`]), so no other write lands between the read and the
/// row.
pub async fn recompute_lead(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId) -> eyre::Result<Option<LeadState>> {
	let recorded: Vec<Recorded> = events::of_lead(conn, brand, lead).await?.into_iter().filter_map(registered).collect();
	match lead::fold(&recorded) {
		Some(state) => {
			let booking = booking_of(conn, brand, lead, &recorded).await?;
			upsert_lead(conn, &state, &booking).await?;
			Ok(Some(state))
		}
		None => {
			sqlx::query("DELETE FROM leads WHERE brand_id = $1 AND lead_id = $2")
				.bind(brand.as_str())
				.bind(lead.as_str())
				.execute(&mut *conn)
				.await
				.wrap_err("dropping a lead with no registered events")?;
			Ok(None)
		}
	}
}

/// A stored event as a fact, if the registry still takes it. One marked registered that no
/// longer passes is left out, loudly: the rebuild will mark it.
pub fn registered(e: Stored) -> Option<Recorded> {
	match e.check() {
		Checked::Registered(fact) => Some(Recorded {
			id: e.id,
			occurred_at: e.occurred_at,
			received_at: e.received_at,
			source_kind: e.source_kind,
			subject: e.subject,
			fact,
		}),
		other => {
			tracing::warn!(event = %e.id.raw(), r#type = %e.type_key, checked = ?other, "an event stored as registered no longer passes the registry; run rebuild-projections");
			None
		}
	}
}

/// A lead's booking: its own booking facts, and the events of the providers' bookings joined
/// to it now — not those a provider's event named it in once, which an operator may have
/// joined to another lead since.
async fn booking_of(conn: &mut SqliteConnection, brand: &BrandId, lead: &LeadId, own: &[Recorded]) -> eyre::Result<BookingState> {
	let mut items: Vec<(jiff::Timestamp, EventId, BookingItem)> = own
		.iter()
		.filter_map(|e| {
			let item = match &e.fact {
				Fact::BookingRequested {
					provider,
					preferred_date,
					preferred_part,
				} => BookingItem::Requested {
					provider: *provider,
					date: *preferred_date,
					part: *preferred_part,
				},
				Fact::BookingSet { start_at, end_at } => BookingItem::Set { start: *start_at, end: *end_at },
				Fact::BookingStatusChanged(c) => BookingItem::Closed(*c),
				Fact::BookingCleared => BookingItem::Cleared,
				_ => return None,
			};
			Some((e.occurred_at, e.id, item))
		})
		.collect();
	items.extend(bookings::items_of_lead(conn, brand, lead).await?);
	Ok(booking::fold(&mut items))
}

async fn upsert_lead(conn: &mut SqliteConnection, s: &LeadState, b: &BookingState) -> eyre::Result<()> {
	let t = |ts: Option<jiff::Timestamp>| ts.map(to_db);
	sqlx::query(
		"INSERT INTO leads (brand_id, lead_id, location_id, job_id, stage, channel, manual, \
		 created_at, contacted_at, quoted_at, won_at, completed_at, paid_at, lost_at, lost_reason, last_event_id, last_event_at, suspect, \
		 flow, quoted_cents, pricing_valid_from, estimate_inputs, booking_status, booking_provider, booking_start_at, booking_end_at, \
		 booking_external_ref, booking_match, booking_preferred_date, booking_preferred_part, message_ref, messaged_at, messaged_channel, locale) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, \
		 $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33, $34) \
		 ON CONFLICT (brand_id, lead_id) DO UPDATE SET \
		 location_id = EXCLUDED.location_id, job_id = EXCLUDED.job_id, stage = EXCLUDED.stage, channel = EXCLUDED.channel, manual = EXCLUDED.manual, \
		 created_at = EXCLUDED.created_at, contacted_at = EXCLUDED.contacted_at, quoted_at = EXCLUDED.quoted_at, won_at = EXCLUDED.won_at, \
		 completed_at = EXCLUDED.completed_at, paid_at = EXCLUDED.paid_at, lost_at = EXCLUDED.lost_at, lost_reason = EXCLUDED.lost_reason, \
		 last_event_id = EXCLUDED.last_event_id, last_event_at = EXCLUDED.last_event_at, suspect = EXCLUDED.suspect, \
		 flow = EXCLUDED.flow, quoted_cents = EXCLUDED.quoted_cents, pricing_valid_from = EXCLUDED.pricing_valid_from, \
		 estimate_inputs = EXCLUDED.estimate_inputs, booking_status = EXCLUDED.booking_status, booking_provider = EXCLUDED.booking_provider, \
		 booking_start_at = EXCLUDED.booking_start_at, booking_end_at = EXCLUDED.booking_end_at, booking_external_ref = EXCLUDED.booking_external_ref, \
		 booking_match = EXCLUDED.booking_match, booking_preferred_date = EXCLUDED.booking_preferred_date, \
		 booking_preferred_part = EXCLUDED.booking_preferred_part, message_ref = EXCLUDED.message_ref, \
		 messaged_at = EXCLUDED.messaged_at, messaged_channel = EXCLUDED.messaged_channel, locale = EXCLUDED.locale",
	)
	.bind(s.brand_id.as_str())
	.bind(s.lead_id.as_str())
	.bind(s.location_id.as_ref().map(LocationId::as_str))
	.bind(s.job_id.as_ref().map(JobId::as_str))
	.bind(s.stage.as_str())
	.bind(s.channel.map(|c| c.as_str()))
	.bind(s.manual)
	.bind(t(s.times.created))
	.bind(t(s.times.contacted))
	.bind(t(s.times.quoted))
	.bind(t(s.times.won))
	.bind(t(s.times.completed))
	.bind(t(s.times.paid))
	.bind(t(s.times.lost))
	.bind(s.lost_reason.as_deref())
	.bind(s.last_event_id.raw())
	.bind(to_db(s.last_event_at))
	.bind(s.suspect.map(|m| m.as_str()))
	.bind(s.offer.flow.map(|f| f.as_str()))
	.bind(s.offer.price.map(|p| p.cents))
	.bind(s.offer.price.map(|p| super::day_to_db(p.valid_from)))
	.bind((!s.offer.estimate_inputs.is_empty()).then_some(Json(&s.offer.estimate_inputs)))
	.bind((b.status != booking::BookingStatus::None).then(|| b.status.as_str()))
	.bind(b.provider.map(|p| p.as_str()))
	.bind(t(b.start_at))
	.bind(t(b.end_at))
	.bind(b.external_ref.as_deref())
	.bind(b.matched.map(|m| m.as_str()))
	.bind(b.preferred_date.map(super::day_to_db))
	.bind(b.preferred_part.map(|p| p.as_str()))
	.bind(s.message_ref.as_ref().map(|r| r.as_str()))
	.bind(t(s.messaged.map(|(at, _)| at)))
	.bind(s.messaged.map(|(_, m)| m.as_str()))
	.bind(s.locale.map(|l| l.as_str()))
	.execute(&mut *conn)
	.await
	.wrap_err_with(|| format!("writing lead {}/{}", s.brand_id, s.lead_id))?;
	Ok(())
}

/// Empties the projections, for a rebuild. Inside the rebuild's write transaction, so ingest
/// waits for it to commit rather than writing into a half-built state, and readers keep
/// seeing the projections as they were until then.
pub async fn clear(conn: &mut SqliteConnection) -> eyre::Result<()> {
	sqlx::raw_sql("DELETE FROM leads; DELETE FROM calls; DELETE FROM payments; DELETE FROM experiments; DELETE FROM bookings; DELETE FROM booking_events")
		.execute(&mut *conn)
		.await
		.wrap_err("clearing the projections")?;
	Ok(())
}

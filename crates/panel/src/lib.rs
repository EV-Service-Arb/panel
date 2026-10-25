//! The panel's engine: a signed batch in, each event checked, journaled and projected.
//!
//! ```text
//! batch ─ signature (source's key) ─┬─ event ─ envelope ─ key may write it? ─ registry ─┐
//!                                   ├─ event …                                          │
//!                                   └─ …        one transaction per event: journal ─────┴─ projections
//! ```
//!
//! [`Panel`] is the facade the server talks to; [`wire`] turns protojson into the domain of
//! `panel_core`; [`store`] is SQLite; [`seal`] encrypts what must not sit in the clear.
//! [`operator`] is what a signed-in user does and reads, as events through the same journal;
//! [`session`] is signing in through concierge and the sessions that follow; [`telegram`] the
//! bot's notifications and buttons; [`capture`] what PostHog is told of the leads' lives, sent
//! from an outbox; [`place`] the places' live settings the sites read and the panel edits;
//! [`pricing`] the brands' price lists, the same; [`experiment`] the brands' experiments as
//! configuration; [`booking`] the leads' bookings, the providers' seam and the matching of
//! their bookings to leads; [`live`] the bus that tells the server's sockets what changed,
//! published here after each commit.

pub mod access;
pub mod booking;
pub mod capture;
pub mod experiment;
pub mod invoices;
pub mod live;
pub mod operator;
pub mod place;
pub mod posthog;
pub mod pricing;
pub mod seal;
pub mod session;
pub mod store;
pub mod telegram;
#[cfg(feature = "testing")]
pub mod testing;
pub mod wire;

use std::{collections::BTreeSet, sync::Arc};

use eyre::WrapErr;
use jiff::Timestamp;
use panel_core::{
	Invalid,
	event::{Envelope, KeyGrant, SourceKind},
	fact::{Fact, MessageRef},
	ids::{BrandId, LeadId, LocationId},
	lead::Recorded,
	signature::{self, SignatureError},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{
	seal::{DataKey, pii_aad, source_secret_aad},
	store::{
		Store,
		events::{self, Inserted, NewEvent, Status},
		projections,
	},
	wire::{Checked, Incoming},
};

/// The [`IngestError::Unauthorized`] of a delivery outside the replay window: the only one a
/// caller is told apart. It is checked before the key, and says only that the caller's clock
/// and ours disagree.
pub const STALE: &str = "stale or malformed timestamp";

/// Why a `lead.messaged` naming its lead by a ref no lead of the brand carries is deferred: the
/// landing posts its `lead.created` in the background as the customer taps, so the message may
/// come first. Like a `booking.requested` before its lead, the batch is answered 409 with
/// `Retry-After`, nothing of that event journaled, and the bot sends the same event again.
pub const UNKNOWN_REF: &str = "unknown_ref: no lead of the brand carries properties.message_ref";

/// Why a `lead.messaged` naming a lead the panel has not seen is deferred, as [`UNKNOWN_REF`] is.
pub const UNKNOWN_LEAD: &str = "unknown_lead: subject.lead_id names no lead yet: send again after its lead.created";

/// Why a whole batch was refused. Per-event verdicts are [`Outcome`]s instead.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
	/// No such key, a revoked one, a bad signature, or a delivery outside the window. The
	/// caller is told only that it was refused; the log says which.
	#[error("unauthorized: {0}")]
	Unauthorized(&'static str),
	/// The body as a whole is not a batch; or, for a lookup, the brand or the ref asked is not
	/// one.
	#[error("{0}")]
	BadRequest(Invalid),
	/// A good signature, by a key that may not ask this: not a bot's, or not for that brand.
	#[error("forbidden: {0}")]
	Forbidden(&'static str),
	#[error(transparent)]
	Internal(#[from] eyre::Report),
}

/// The verdict on one event of a batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
	/// Journaled. With a type the registry does not know, it is not projected (yet).
	Accepted {
		unregistered: bool,
	},
	/// Already journaled.
	Duplicate,
	Rejected(Invalid),
	/// Not journaled yet, and not refused: it names what has not arrived (a
	/// `booking.requested` before its lead's `lead.created` — a source's outbox keeps no
	/// order). The batch is answered with a status the source retries.
	Deferred(Invalid),
}

/// One event's verdict, with where it was in the batch and the id it claimed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventVerdict {
	pub index: usize,
	pub id: String,
	pub outcome: Outcome,
}

/// A signed request, as it arrived.
#[derive(Clone, Copy, Debug)]
pub struct SignedBatch<'a> {
	pub key_id: &'a str,
	pub timestamp: &'a str,
	pub signature: &'a str,
	pub body: &'a [u8],
}

/// A new source's credentials; the secret is shown once and never again.
pub struct NewSource {
	pub key_id: String,
	pub secret: Zeroizing<String>,
}

impl std::fmt::Debug for NewSource {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("NewSource").field("key_id", &self.key_id).field("secret", &"<redacted>").finish()
	}
}

/// A lead as a bot sees it through the lookup by ref: where it stands and what the customer
/// asked for, never who they are — no name, no phone.
#[derive(Clone, Debug, PartialEq)]
pub struct RefLead {
	pub lead_id: String,
	/// `None` for a lead seen before its `lead.created`.
	pub channel: Option<String>,
	pub stage: &'static str,
	pub created_at: Option<Timestamp>,
	pub message_ref: String,
	/// What the customer needs, as the landing put it: their words, so a bot shows it back only
	/// to the person who carries the ref.
	pub need: Option<String>,
	pub locality: Option<String>,
	pub quoted_cents: Option<i64>,
	pub flow: Option<String>,
	/// When the customer first wrote on a messenger, and on which: a bot need not say it twice.
	pub messaged_at: Option<Timestamp>,
	pub messaged_channel: Option<&'static str>,
}

/// What a rebuild did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rebuilt {
	pub events: u64,
	pub registered: u64,
	pub unregistered: u64,
	pub invalid: u64,
	pub leads: u64,
}

/// An id derived from its parts, the same for the same parts: what makes a retry of something
/// the panel writes itself land on the journal's id instead of a second event.
///
/// Shaped as a UUIDv7 (version and variant bits), which is what the journal keys by, but its
/// time field is hash, not time: the journal orders by `occurred_at`/`received_at`, the id
/// only breaks ties.
pub fn derived_id(namespace: &[u8], parts: &[&[u8]]) -> uuid::Uuid {
	let mut h = Sha256::new();
	h.update(namespace);
	for (i, part) in parts.iter().enumerate() {
		if i > 0 {
			h.update([0]);
		}
		h.update(part);
	}
	let digest = h.finalize();
	let mut bytes = [0u8; 16];
	bytes.copy_from_slice(&digest[..16]);
	uuid::Builder::from_bytes(bytes)
		.with_version(uuid::Version::SortRand)
		.with_variant(uuid::Variant::RFC4122)
		.into_uuid()
}

/// The sources' keys changed: no brand, no single subject a screen would refetch by.
fn sources_changed() -> live::Change {
	live::Change {
		topic: live::Topic::Sources,
		brand: None,
		id: None,
		user: None,
		at: Timestamp::now(),
	}
}

/// Queues what PostHog is told of a new lead event: nothing for what is about no lead, nor for
/// a `lead.created` that is not the one that counts (a source's second one).
async fn queue_capture(conn: &mut sqlx::SqliteConnection, recorded: &Recorded, lead: Option<&panel_core::lead::LeadState>, now: Timestamp) -> eyre::Result<()> {
	let (Some(capture), Some(lead)) = (panel_core::analytics::capture_of(recorded), lead) else {
		return Ok(());
	};
	if matches!(recorded.fact, Fact::LeadCreated { .. }) && lead.creation != Some(recorded.id) {
		return Ok(());
	}
	// A conversation is told once: its first message, as the panel journaled them.
	if matches!(recorded.fact, Fact::LeadMessaged { .. }) && store::reads::first_message(conn, &lead.brand_id, &lead.lead_id).await? != Some(recorded.id.raw()) {
		return Ok(());
	}
	let properties = serde_json::to_value(&capture.properties).wrap_err("PostHog properties")?;
	store::posthog::enqueue(
		conn,
		&store::posthog::Queued {
			event_id: recorded.id.raw(),
			event: capture.event.to_owned(),
			distinct_id: panel_core::analytics::distinct_id(lead.brand_id.as_str(), lead.lead_id.as_str(), lead.analytics_id.as_ref()),
			properties,
			occurred_at: recorded.occurred_at,
			queued_at: now,
			tries: 0,
		},
	)
	.await
}

/// The engine.
#[derive(Clone, Debug)]
pub struct Panel {
	store: Store,
	key: Arc<DataKey>,
	rotations: Arc<session::Rotations>,
	live: live::Bus,
	posthog: Option<posthog::PosthogProject>,
	/// Whether journaled lead events are queued for PostHog ([`capture`]).
	capture: bool,
}

impl Panel {
	pub fn new(store: Store, key: DataKey) -> Self {
		Self {
			store,
			key: Arc::new(key),
			rotations: Arc::default(),
			live: live::Bus::default(),
			posthog: None,
			capture: false,
		}
	}

	/// This panel linking the funnel and each experiment to `project`.
	pub fn with_posthog_project(mut self, project: Option<posthog::PosthogProject>) -> Self {
		self.posthog = project;
		self
	}

	/// The funnel of `[from, to]` in PostHog, from the visit; `None` without a project.
	pub fn posthog_funnel_url(&self, brand: Option<&BrandId>, from: jiff::civil::Date, to: jiff::civil::Date) -> Option<String> {
		self.posthog.as_ref().map(|p| p.funnel_url(brand, from, to))
	}

	/// This panel on `bus` instead of its own: a bus of another capacity, or one shared.
	pub fn with_live(mut self, bus: live::Bus) -> Self {
		self.live = bus;
		self
	}

	pub fn store(&self) -> &Store {
		&self.store
	}

	/// What changed, after each commit: see [`live`].
	pub fn bus(&self) -> &live::Bus {
		&self.live
	}

	/// Registers a source that may write events of `kind` for `brands`, with a fresh
	/// secret. `None` when the key id is taken.
	pub async fn add_source(&self, key_id: &str, kind: SourceKind, brands: BTreeSet<BrandId>) -> eyre::Result<Option<NewSource>> {
		eyre::ensure!(panel_core::ids::is_slug(key_id), "a key id is a lowercase slug of 1–64 of [a-z0-9_-]");
		eyre::ensure!(!brands.is_empty(), "a source writes for at least one brand");
		// The booking adapters are the panel's own: what a provider says arrives through its
		// webhook's signature or the panel's pull, never a key.
		eyre::ensure!(kind != SourceKind::Booking, "a key of kind booking is not issued: the panel's booking adapters write without one");
		let secret = Zeroizing::new(DataKey::generate_hex()?);
		let sealed = self.key.seal(&source_secret_aad(key_id), secret.as_bytes())?;
		let grant = KeyGrant {
			key_id: key_id.to_owned(),
			kind,
			brands,
		};
		let added = self.store.insert_source(&grant, &sealed, &self.key.fingerprint()).await?;
		if added {
			self.live.changed(sources_changed());
		}
		Ok(added.then(|| NewSource { key_id: key_id.to_owned(), secret }))
	}

	/// Revokes a source's key; `false` when there was no such key, or it was revoked already.
	pub async fn revoke_source(&self, key_id: &str) -> eyre::Result<bool> {
		let revoked = self.store.revoke_source(key_id).await?;
		if revoked {
			self.live.changed(sources_changed());
		}
		Ok(revoked)
	}

	/// Checks a batch's signature, then judges, journals and projects each of its events.
	pub async fn ingest(&self, batch: SignedBatch<'_>, now: Timestamp) -> Result<Vec<EventVerdict>, IngestError> {
		let grant = self.authenticate(&batch, now).await?;
		let raw = wire::batch(batch.body).map_err(IngestError::BadRequest)?;
		let mut verdicts = Vec::with_capacity(raw.len());
		for (index, event) in raw.into_iter().enumerate() {
			let id = wire::claimed_id(&event);
			let outcome = self.one(&grant, event, now).await?;
			verdicts.push(EventVerdict { index, id, outcome });
		}
		let count = |f: fn(&Outcome) -> bool| verdicts.iter().filter(|v| f(&v.outcome)).count();
		tracing::info!(
			key_id = grant.key_id,
			accepted = count(|o| matches!(o, Outcome::Accepted { .. })),
			duplicate = count(|o| matches!(o, Outcome::Duplicate)),
			rejected = count(|o| matches!(o, Outcome::Rejected(_))),
			"ingested a batch"
		);
		Ok(verdicts)
	}

	/// The brand's newest lead carrying `message_ref`, for a bot's key of that brand (`GET
	/// /api/ingest/v1/leads/by-ref/{brand}/{ref}`): signed as a batch is, `request.body` being
	/// what the server signs in place of one — `GET <path>`.
	/// `Ok(None)` when no lead of the brand carries it.
	pub async fn lead_by_ref(&self, request: SignedBatch<'_>, brand: &str, message_ref: &str, now: Timestamp) -> Result<Option<RefLead>, IngestError> {
		let grant = self.authenticate(&request, now).await?;
		let brand = BrandId::parse(brand).map_err(IngestError::BadRequest)?;
		let message_ref = MessageRef::parse(message_ref).map_err(|_| IngestError::BadRequest(Invalid::new("the ref is not like \"AQ-7K3F\"")))?;
		// What the customer asked for is read back only to what holds a conversation with them.
		if grant.kind != SourceKind::Bot {
			tracing::warn!(key_id = grant.key_id, kind = %grant.kind, "lookup by ref: not a bot's key");
			return Err(IngestError::Forbidden("only a bot's key looks a lead up by its ref"));
		}
		if !grant.brands.contains(&brand) {
			tracing::warn!(key_id = grant.key_id, %brand, "lookup by ref: a brand the key may not write for");
			return Err(IngestError::Forbidden("this key is not for that brand"));
		}
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		let Some(row) = store::reads::lead_by_ref(&mut conn, &brand, &message_ref).await? else {
			return Ok(None);
		};
		drop(conn);
		let pii = match &row.creation {
			Some(sealed) => self.open_pii(sealed)?,
			None => None,
		};
		let text = |key: &str| pii.as_ref().and_then(|p| p.get(key)).and_then(Value::as_str).map(str::to_owned);
		Ok(Some(RefLead {
			need: text("need"),
			locality: text("locality"),
			lead_id: row.lead_id,
			channel: row.channel,
			stage: row.stage.as_str(),
			created_at: row.created_at,
			message_ref: message_ref.as_str().to_owned(),
			quoted_cents: row.quoted_cents,
			flow: row.flow,
			messaged_at: row.messaged.map(|(at, _)| at),
			messaged_channel: row.messaged.map(|(_, m)| m.as_str()),
		}))
	}

	async fn authenticate(&self, batch: &SignedBatch<'_>, now: Timestamp) -> Result<KeyGrant, IngestError> {
		let refuse = |e: SignatureError| {
			IngestError::Unauthorized(match e {
				SignatureError::MalformedTimestamp | SignatureError::Stale => STALE,
				SignatureError::Mismatch => "bad signature",
			})
		};
		// Free checks first, before the database: the window, and whether the key id could
		// be one at all. A key id that could not is not logged — it is the caller's string.
		signature::check_window(batch.timestamp, now).map_err(refuse)?;
		if !panel_core::ids::is_slug(batch.key_id) {
			return Err(IngestError::Unauthorized("unknown key"));
		}
		let Some(source) = self.store.active_source(batch.key_id).await? else {
			// The MAC is computed anyway, against a key nobody holds, so an unknown key takes
			// as long to refuse as a bad signature and key ids cannot be told apart by timing.
			let _refused = signature::verify(&[0u8; 32], batch.timestamp, batch.signature, batch.body, now);
			tracing::warn!(key_id = batch.key_id, "ingest: unknown or revoked key");
			return Err(IngestError::Unauthorized("unknown key"));
		};
		if source.data_key_fp != self.key.fingerprint() {
			return Err(IngestError::Internal(eyre::eyre!("source {} was sealed under another PANEL_DATA_KEY", batch.key_id)));
		}
		let secret = self
			.key
			.open(&source_secret_aad(batch.key_id), &source.secret_sealed)
			.wrap_err_with(|| format!("opening the secret of source {}", batch.key_id))?;
		signature::verify(&secret, batch.timestamp, batch.signature, batch.body, now).map_err(|e| {
			tracing::warn!(key_id = batch.key_id, error = %e, "ingest: signature refused");
			refuse(e)
		})?;
		Ok(source.grant)
	}

	async fn one(&self, grant: &KeyGrant, raw: Value, now: Timestamp) -> eyre::Result<Outcome> {
		let incoming = match wire::decode(raw, now) {
			Ok(i) => i,
			Err(e) => return Ok(Outcome::Rejected(e)),
		};
		if let Err(e) = grant.permits(&incoming.envelope) {
			// Warned, not just answered: a key asking for more than it was given is either a
			// misconfigured source or a stolen key trying its luck.
			let env = &incoming.envelope;
			tracing::warn!(key_id = grant.key_id, brand = %env.subject.brand_id, kind = %env.source.kind, r#type = %env.type_key, reason = %e, "ingest: key not permitted");
			return Ok(Outcome::Rejected(e));
		}
		let checked = wire::check(&incoming.envelope.type_key, incoming.envelope.source.kind, &incoming.properties, &incoming.envelope.subject);
		let (status, _) = Status::of(&checked);
		let fact = match checked {
			Checked::Registered(fact) => Some(fact),
			Checked::Unregistered => None,
			Checked::Invalid(e) => return Ok(Outcome::Rejected(e)),
		};
		if let Some(Fact::BookingRequested { preferred_date, .. }) = &fact {
			// The wish is judged against the day it arrives, here and not in the registry: a
			// rebuild a year on must not refuse what was fine then.
			let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
			if preferred_date.is_some_and(|d| !panel_core::booking::preferred_date_in_window(d, today)) {
				return Ok(Outcome::Rejected(Invalid::new("properties.preferred_date is not within 2 days back and 366 ahead")));
			}
			let env = &incoming.envelope;
			if let Some(lead) = &env.subject.lead_id {
				let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
				if !store::bookings::lead_exists(&mut conn, &env.subject.brand_id, lead).await? {
					return Ok(Outcome::Deferred(Invalid::new("subject.lead_id names no lead yet: send again after its lead.created")));
				}
			}
		}
		self.journal(&incoming, Some(&grant.key_id), status, fact, now).await
	}

	/// Journals an event the panel writes itself — an operator's action, an admin's setting —
	/// decoded and judged exactly as a source's, with no signing key. `Err` inside: the
	/// registry refuses it, and why.
	async fn write_own(&self, raw: Value, now: Timestamp) -> eyre::Result<Result<(Outcome, Envelope), Invalid>> {
		let incoming = match wire::decode(raw, now) {
			Ok(i) => i,
			Err(e) => return Ok(Err(e)),
		};
		let env = &incoming.envelope;
		let checked = wire::check(&env.type_key, env.source.kind, &incoming.properties, &env.subject);
		let (status, _) = Status::of(&checked);
		let fact = match checked {
			Checked::Registered(fact) => fact,
			Checked::Invalid(e) => return Ok(Err(e)),
			Checked::Unregistered => eyre::bail!("the panel wrote an unregistered type {}", env.type_key),
		};
		let outcome = self.journal(&incoming, None, status, Some(fact), now).await?;
		Ok(Ok((outcome, incoming.envelope)))
	}

	/// Journals one event and, when registered, projects it — in one transaction, so a
	/// projection never runs ahead of the journal nor behind it.
	async fn journal(&self, incoming: &Incoming, key_id: Option<&str>, status: Status, fact: Option<Fact>, now: Timestamp) -> eyre::Result<Outcome> {
		let pii = match &incoming.pii {
			Some(pii) => {
				let plain = Zeroizing::new(serde_json::to_vec(pii).wrap_err("serializing PII")?);
				Some((self.key.seal(&pii_aad(incoming.envelope.id.raw()), &plain)?, self.key.fingerprint()))
			}
			None => None,
		};
		// The write lock up front: a rebuild in progress is waited out, and so is another
		// event of the same lead (see `store`).
		let mut tx = self.store.begin_write().await?;
		// A message naming its lead by ref alone is journaled under the lead the ref names now,
		// found under the write lock so no newer lead slips in between: the journal keeps that
		// lead id, so a rebuild — when newer leads may carry the same ref — does not look again.
		// The content MAC is of the event as sent, so a resend is still a duplicate. The lead's
		// location goes with it when the bot named none, so the event and its PostHog capture sit
		// where the lead does.
		let named;
		let incoming = match &fact {
			Some(Fact::LeadMessaged { message_ref: Some(r), .. }) if incoming.envelope.subject.lead_id.is_none() => {
				let Some(lead) = store::reads::lead_by_ref(&mut tx, &incoming.envelope.subject.brand_id, r).await? else {
					return Ok(Outcome::Deferred(Invalid::new(UNKNOWN_REF)));
				};
				let mut resolved = incoming.clone();
				let subject = &mut resolved.envelope.subject;
				subject.lead_id = Some(LeadId::parse(&lead.lead_id).wrap_err("a stored lead id")?);
				if subject.location_id.is_none() {
					subject.location_id = lead.location_id.as_deref().map(LocationId::parse).transpose().wrap_err("a stored location id")?;
				}
				named = resolved;
				&named
			}
			// A message names a lead that is there: a bot's leadId the panel has no lead for is
			// waited for like a booking.requested before its lead, never a phantom lead.
			Some(Fact::LeadMessaged { .. }) => {
				let env = &incoming.envelope;
				if let Some(lead) = &env.subject.lead_id
					&& !store::bookings::lead_exists(&mut tx, &env.subject.brand_id, lead).await?
				{
					return Ok(Outcome::Deferred(Invalid::new(UNKNOWN_LEAD)));
				}
				incoming
			}
			_ => incoming,
		};
		let inserted = events::insert(
			&mut tx,
			&NewEvent {
				incoming,
				key_id,
				received_at: now,
				status,
				content_mac: self.key.content_mac(&incoming.canonical),
				pii,
			},
		)
		.await?;
		match inserted {
			Inserted::New => {}
			// Nothing was written; dropping the transaction rolls back nothing.
			Inserted::Duplicate => return Ok(Outcome::Duplicate),
			Inserted::Conflict => return Ok(Outcome::Rejected(Invalid::new("id already names a different event"))),
		}
		let mut changes = Vec::new();
		if let Some(fact) = fact {
			let env = &incoming.envelope;
			let recorded = Recorded {
				id: env.id,
				occurred_at: env.occurred_at,
				received_at: now,
				source_kind: env.source.kind,
				subject: env.subject.clone(),
				fact,
			};
			let applied = projections::apply(&mut tx, &recorded).await?;
			// Here, in the journal's transaction, and nowhere else: the rebuild projects without
			// passing here, so it never tells PostHog anything twice.
			if self.capture {
				queue_capture(&mut tx, &recorded, applied.lead.as_ref(), now).await?;
			}
			changes = live::Change::of_applied(&recorded, &applied, now);
		}
		tx.commit().await.wrap_err("committing an event")?;
		// Every event that reaches the projections passes here, whoever wrote it: this one
		// publication is what keeps ingest, the operator's actions, the buttons and the booking
		// adapters from forgetting to. An unregistered event changes no read, and says nothing.
		for change in changes {
			self.live.changed(change);
		}
		Ok(Outcome::Accepted {
			unregistered: status == Status::Unregistered,
		})
	}

	/// An event's PII, opened. For whoever may see it; the caller checks.
	pub async fn pii(&self, id: uuid::Uuid) -> eyre::Result<Option<Value>> {
		/// `pii_sealed`, `data_key_fp`.
		type Sealed = (Option<Vec<u8>>, Option<Vec<u8>>);
		let row: Option<Sealed> = sqlx::query_as("SELECT pii_sealed, data_key_fp FROM events WHERE id = $1")
			.bind(id)
			.fetch_optional(self.store.pool())
			.await
			.wrap_err("reading an event's PII")?;
		let Some((Some(blob), Some(fp))) = row else { return Ok(None) };
		eyre::ensure!(fp == self.key.fingerprint(), "the PII of event {id} was sealed under another PANEL_DATA_KEY");
		let plain = self.key.open(&pii_aad(id), &blob).wrap_err_with(|| format!("opening the PII of event {id}"))?;
		Ok(Some(serde_json::from_slice(&plain).wrap_err("stored PII is not JSON")?))
	}

	/// Rebuilds the projections from the journal, in one transaction: every event is judged
	/// against the registry as it is now (its status updated to match), and the registered
	/// ones projected exactly as on arrival.
	pub async fn rebuild_projections(&self) -> eyre::Result<Rebuilt> {
		const PAGE: i64 = 1000;
		// One write transaction from the first delete to the commit: ingest waits for it, and
		// readers see the old projections until then.
		let mut tx = self.store.begin_write().await?;
		projections::clear(&mut tx).await?;
		let mut done = Rebuilt::default();
		let mut leads: BTreeSet<(BrandId, LeadId)> = BTreeSet::new();
		let mut external = BTreeSet::new();
		let mut experiment_brands: BTreeSet<BrandId> = BTreeSet::new();
		let mut after = None;
		loop {
			let page = events::page(&mut tx, after, PAGE).await?;
			let Some(last) = page.last() else { break };
			after = Some((last.occurred_at, last.id));
			for stored in page {
				done.events += 1;
				let checked = stored.check();
				let (status, reason) = Status::of(&checked);
				if (status, reason.as_deref()) != (stored.status, stored.status_reason.as_deref()) {
					events::set_status(&mut tx, stored.id, status, reason.as_deref()).await?;
				}
				let fact = match checked {
					Checked::Registered(fact) => fact,
					Checked::Unregistered => {
						done.unregistered += 1;
						continue;
					}
					Checked::Invalid(_) => {
						done.invalid += 1;
						continue;
					}
				};
				done.registered += 1;
				let recorded = Recorded {
					id: stored.id,
					occurred_at: stored.occurred_at,
					received_at: stored.received_at,
					source_kind: stored.source_kind,
					subject: stored.subject,
					fact,
				};
				projections::insert_row(&mut tx, &recorded).await?;
				if let Some((provider, external_ref)) = store::bookings::external_key(&recorded.fact) {
					external.insert((recorded.subject.brand_id.clone(), provider, external_ref.to_owned()));
				}
				if projections::is_experiment(&recorded.fact) {
					experiment_brands.insert(recorded.subject.brand_id.clone());
				}
				if let Some(lead) = recorded.subject.lead_id {
					leads.insert((recorded.subject.brand_id, lead));
				}
			}
		}
		// The providers' bookings before the leads: a lead's booking reads the ones joined to it.
		for (brand, provider, external_ref) in &external {
			if let Some(lead) = store::bookings::recompute(&mut tx, brand, *provider, external_ref).await?.after {
				leads.insert((brand.clone(), lead));
			}
		}
		for (brand, lead) in &leads {
			projections::recompute_lead(&mut tx, brand, lead).await?;
		}
		done.leads = leads.len() as u64;
		for brand in &experiment_brands {
			store::experiments::recompute(&mut tx, brand).await?;
		}
		tx.commit().await.wrap_err("committing the rebuild")?;
		self.live.publish(live::Signal::Resync);
		Ok(done)
	}
}

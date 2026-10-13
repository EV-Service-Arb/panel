//! What an operator does in the panel (spec §5.4, §10a), as events.
//!
//! Every action becomes a `sa.funnel.v1` event of `source.kind = panel`, `source.id` the
//! user's concierge id, and goes through the same decode, registry and journal as a signed
//! batch — only with no signing key (`key_id` NULL). So the projections change the one way
//! they ever change, and a rebuild lands on the same state.
//!
//! And what the operator screens read: leads, a lead's card, the funnel.

use eyre::WrapErr;
use jiff::{Timestamp, civil::Date};
use panel_contracts::SCHEMA;
use panel_core::{
	Invalid,
	fact::{LeadChannel, LeadFlow, MessageRef, Messenger, bounded},
	funnel::{self, Totals},
	ids::{BrandId, EventId, JobId, LeadId, LocationId},
	lead::Stage,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{
	Panel,
	seal::pii_aad,
	store::reads::{self, EventRow, LeadFilter, LeadRow, PaymentSum, PlaceRow, Sealed},
};

/// Why an operator's action was not recorded.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
	/// No such lead, or no such call attempt of it.
	#[error("not found")]
	NotFound,
	/// The lead is not where the action needs it: completing a job never won.
	#[error("{0}")]
	Conflict(&'static str),
	/// What was entered does not pass the registry; the reason names the field.
	#[error("{0}")]
	Invalid(Invalid),
	#[error(transparent)]
	Internal(#[from] eyre::Report),
}

/// Who acts: the concierge user id of the signed-in operator or admin.
#[derive(Clone, Copy, Debug)]
pub struct Actor(pub Uuid);

/// What an action did: recorded now, or found recorded already under the same idempotency
/// key (a retry), in which case `value` is what the first attempt answered.
#[derive(Clone, Debug)]
pub struct Done<T> {
	pub value: T,
	pub replayed: bool,
}

/// The id of the event an action with the client's idempotency key makes: the same for the
/// same user, action and key, so a retry lands on the journal's id and is found there.
///
/// Shaped as a UUIDv7 (version and variant bits), which is what the journal keys by, but
/// its time field is hash, not time: the journal orders by `occurred_at`/`received_at`, the
/// id only breaks ties.
pub fn idempotent_id(by: Actor, action: &str, key: &str) -> Uuid {
	crate::derived_id(b"sa-panel/idempotency/v1/", &[by.0.as_bytes(), action.as_bytes(), key.as_bytes()])
}

/// A lead taken by hand, bypassing the form (§10a "+ Call"): over the phone, or from a customer
/// who wrote on a messenger without going through the landing.
#[derive(Clone, Debug)]
pub struct NewLead {
	pub brand: BrandId,
	pub location: LocationId,
	/// What the customer needs, in their words: PII.
	pub need: String,
	pub phone: Option<String>,
	/// How it came in: one of [`LeadChannel::MANUAL`].
	pub channel: LeadChannel,
}

/// A move of a lead through the funnel (stages 6–9).
#[derive(Clone, Debug)]
pub enum StageMove {
	Contacted {
		channel: Option<String>,
	},
	Quoted {
		amount: Option<i64>,
		currency: Option<String>,
	},
	/// A job is won; without an id, one is made.
	Won {
		job_id: Option<JobId>,
	},
	Lost {
		reason: String,
		note: Option<String>,
	},
	/// The lead's won job is done.
	Completed,
}

/// How an outgoing call ended.
#[derive(Clone, Debug)]
pub struct CallOutcome {
	pub attempt: Uuid,
	pub outcome: String,
}

/// A payment, entered by hand (owner, 2026-09-30).
#[derive(Clone, Debug)]
pub struct Payment {
	/// Minor units of `currency`.
	pub billed: i64,
	pub commission: i64,
	pub currency: String,
}

/// Whether the caller may see PII: the answer is the caller's to give, from their permissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pii {
	Reveal,
	Withhold,
}

/// A lead as the screens show it: the projection, and the PII of its creation when revealed.
#[derive(Clone, Debug)]
pub struct LeadView {
	pub row: LeadRow,
	pub pii: Option<Value>,
	pub waiting: Option<funnel::Waiting>,
}

/// One event on a lead's card.
#[derive(Clone, Debug)]
pub struct EventView {
	pub row: EventRow,
	pub pii: Option<Value>,
}

/// A page of leads; `next` continues after its last.
#[derive(Clone, Debug)]
pub struct LeadPage {
	pub leads: Vec<LeadView>,
	pub next: Option<(Timestamp, String, String)>,
}

/// Which leads to list, as the operator asks.
#[derive(Clone, Debug, Default)]
pub struct LeadQuery {
	pub stage: Option<Stage>,
	pub brand: Option<BrandId>,
	pub location: Option<LocationId>,
	/// Only those waiting for their first contact past [`funnel::CONTACT_SLA`].
	pub overdue: bool,
	/// Only those created at or after this.
	pub created_from: Option<Timestamp>,
	/// Only those created before this.
	pub created_before: Option<Timestamp>,
	pub suspect: SuspectFilter,
	/// Only those that came through this flow.
	pub flow: Option<LeadFlow>,
	/// Only those whose booking stands here.
	pub booking: Option<panel_core::booking::BookingStatus>,
	/// Only those that came in through this channel.
	pub channel: Option<LeadChannel>,
	/// Only those carrying this messenger ref: what the customer quotes in their message.
	pub message_ref: Option<MessageRef>,
	pub after: Option<(Timestamp, String, String)>,
	pub limit: u32,
}

/// Which leads to list by the antispam's doubt ([`panel_core::fact::LeadSuspect`]).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SuspectFilter {
	/// Every lead, doubted or not.
	#[default]
	All,
	/// Only those the antispam doubted.
	Only,
	/// Only those it did not.
	Exclude,
}

/// How the funnel is cut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunnelBy {
	/// One slice for everything.
	All,
	/// A slice per brand's location; leads without one make a slice of their own.
	Location,
}

/// One slice of the funnel: its totals and what its leads were paid, by currency.
#[derive(Clone, Debug)]
pub struct FunnelSlice {
	/// `None` for [`FunnelBy::All`].
	pub brand: Option<String>,
	/// `None` for [`FunnelBy::All`], and for the leads that name no location.
	pub location: Option<String>,
	pub totals: Totals,
	/// Per currency, ordered by it; never converted into one another.
	pub payments: Vec<PaymentSum>,
}

/// How many leads are at each stage, for the list's segments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeadCounts {
	/// Every stage, in funnel order, zero included.
	pub stages: Vec<(Stage, u64)>,
	/// Of those at `created`, the ones waiting for their first contact past [`funnel::CONTACT_SLA`].
	pub overdue: u64,
}

/// A page is at most this many leads.
pub const MAX_PAGE: u32 = 200;

impl<T> Done<T> {
	pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> Done<U> {
		Done {
			value: f(self.value),
			replayed: self.replayed,
		}
	}
}

fn opaque_new(prefix: &str, now: Timestamp) -> String {
	// A letter prefix keeps the id from ever being all digits, which `is_opaque` refuses as
	// looking like a phone number.
	format!("{prefix}-{}", new_uuid(now).simple())
}

/// A fresh UUIDv7 at `now`: a new event's id.
pub(crate) fn new_event_id(now: Timestamp) -> Uuid {
	new_uuid(now)
}

fn new_uuid(now: Timestamp) -> Uuid {
	let nanos = u32::try_from(now.subsec_nanosecond().rem_euclid(1_000_000_000)).unwrap_or(0);
	let secs = u64::try_from(now.as_second()).unwrap_or(0);
	Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, nanos))
}

impl Panel {
	/// Records a lead taken by hand: `lead.created{channel}` (phone_inbound, whatsapp or
	/// telegram), what it needs and the number in its PII. Its id is made here.
	pub async fn create_lead(&self, by: Actor, lead: NewLead, now: Timestamp) -> Result<(LeadId, EventId), ActionError> {
		Ok(self.create_lead_once(by, lead, now, None).await?.value)
	}

	/// [`Self::create_lead`], at most once per idempotency `key` of the user.
	pub async fn create_lead_once(&self, by: Actor, lead: NewLead, now: Timestamp, key: Option<&str>) -> Result<Done<(LeadId, EventId)>, ActionError> {
		let id = key.map(|k| idempotent_id(by, "lead.created", k));
		if let Some(done) = self.replayed(by, id).await? {
			return Ok(done);
		}
		let need = bounded("need", Some(lead.need))
			.map_err(ActionError::Invalid)?
			.ok_or_else(|| ActionError::Invalid(Invalid::new("need is required")))?;
		let phone = bounded("phone", lead.phone).map_err(ActionError::Invalid)?;
		if !LeadChannel::MANUAL.contains(&lead.channel) {
			return Err(ActionError::Invalid(Invalid::new("channel is one of phone_inbound, whatsapp, telegram")));
		}
		let lead_id = match key {
			Some(k) => format!("p-{}", idempotent_id(by, "lead.created/lead_id", k).simple()),
			None => opaque_new("p", now),
		};
		let lead_id = LeadId::parse(&lead_id).map_err(|e| eyre::eyre!("a made lead id: {e}"))?;
		let mut pii = Map::new();
		pii.insert("need".into(), json!(need));
		if let Some(phone) = phone {
			pii.insert("phone".into(), json!(phone));
		}
		let subject = json!({"brandId": lead.brand.as_str(), "locationId": lead.location.as_str(), "leadId": lead_id.as_str()});
		let done = self
			.act(
				by,
				id,
				"lead.created",
				subject,
				json!({"channel": lead.channel.as_str(), "enteredBy": by.0.to_string()}),
				Some(Value::Object(pii)),
				now,
			)
			.await?;
		Ok(done)
	}

	/// Moves a lead: `lead.contacted`, `lead.quoted`, `job.won`, `lead.lost` or
	/// `job.completed`.
	pub async fn move_lead(&self, by: Actor, brand: &BrandId, lead: &LeadId, to: StageMove, now: Timestamp) -> Result<EventId, ActionError> {
		Ok(self.move_lead_once(by, brand, lead, to, now, None).await?.value)
	}

	/// [`Self::move_lead`], at most once per idempotency `key` of the user and lead.
	pub async fn move_lead_once(&self, by: Actor, brand: &BrandId, lead: &LeadId, to: StageMove, now: Timestamp, key: Option<&str>) -> Result<Done<EventId>, ActionError> {
		let id = key.map(|k| idempotent_id(by, &format!("stage/{brand}/{lead}"), k));
		if let Some(done) = self.replayed(by, id).await? {
			return Ok(done.map(|(_, e)| e));
		}
		let current = self.lead_row(brand, lead).await?.ok_or(ActionError::NotFound)?;
		let mut subject = subject_of(&current);
		let (r#type, properties) = match to {
			StageMove::Contacted { channel } => ("lead.contacted", optional(&[("channel", channel.map(Value::from))])),
			StageMove::Quoted { amount, currency } => ("lead.quoted", optional(&[("amount", amount.map(Value::from)), ("currency", currency.map(Value::from))])),
			StageMove::Won { job_id } => {
				let job = match job_id {
					Some(job) => job,
					None => JobId::parse(&opaque_new("j", now)).map_err(|e| eyre::eyre!("a made job id: {e}"))?,
				};
				subject["jobId"] = json!(job.as_str());
				("job.won", json!({}))
			}
			StageMove::Lost { reason, note } => ("lead.lost", optional(&[("reason", Some(Value::from(reason))), ("note", note.map(Value::from))])),
			StageMove::Completed => {
				if current.job_id.is_none() {
					return Err(ActionError::Conflict("the lead has no won job to complete"));
				}
				("job.completed", json!({}))
			}
		};
		Ok(self.act(by, id, r#type, subject, properties, None, now).await?.map(|(_, e)| e))
	}

	/// The customer wrote on a messenger (`lead.messaged`), as the operator saw it: a WhatsApp
	/// message read by hand. At most once per lead: when it has a message already, that one is
	/// the answer (`replayed`); and at most once per idempotency `key` of the user and lead.
	pub async fn mark_messaged_once(&self, by: Actor, brand: &BrandId, lead: &LeadId, channel: Messenger, now: Timestamp, key: Option<&str>) -> Result<Done<EventId>, ActionError> {
		let id = key.map(|k| idempotent_id(by, &format!("messaged/{brand}/{lead}"), k));
		if let Some(done) = self.replayed(by, id).await? {
			return Ok(done.map(|(_, e)| e));
		}
		let current = self.lead_row(brand, lead).await?.ok_or(ActionError::NotFound)?;
		// Said once is enough: the lead keeps its first message, so a second click (or a bot that
		// said it first) is answered with that one, and nothing is journaled.
		if current.messaged.is_some() {
			let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
			if let Some(first) = reads::first_message(&mut conn, brand, lead).await? {
				return Ok(Done {
					value: EventId::from_raw(first),
					replayed: true,
				});
			}
		}
		let properties = json!({"channel": channel.as_str()});
		Ok(self.act(by, id, "lead.messaged", subject_of(&current), properties, None, now).await?.map(|(_, e)| e))
	}

	/// An outgoing call started from the panel (`call.attempted`); its event id is the attempt
	/// id the outcome names.
	pub async fn attempt_call(&self, by: Actor, brand: &BrandId, lead: &LeadId, now: Timestamp) -> Result<EventId, ActionError> {
		let current = self.lead_row(brand, lead).await?.ok_or(ActionError::NotFound)?;
		Ok(self.act(by, None, "call.attempted", subject_of(&current), json!({}), None, now).await?.value.1)
	}

	/// How an attempted call of this lead ended (`call.logged`).
	pub async fn log_call(&self, by: Actor, brand: &BrandId, lead: &LeadId, call: CallOutcome, now: Timestamp) -> Result<EventId, ActionError> {
		let current = self.lead_row(brand, lead).await?.ok_or(ActionError::NotFound)?;
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		if !reads::is_call_attempt(&mut conn, brand, lead, call.attempt).await? {
			return Err(ActionError::NotFound);
		}
		drop(conn);
		let properties = json!({"outcome": call.outcome, "attemptId": call.attempt.to_string()});
		Ok(self.act(by, None, "call.logged", subject_of(&current), properties, None, now).await?.value.1)
	}

	/// A call attempted and not answered, in one go (the Telegram button "no answer"):
	/// `call.attempted`, then `call.logged{no_answer}` naming it — each at most once per
	/// idempotency `key` of the user and lead, so a retry after a failure between the two
	/// finishes the pair instead of starting another.
	pub async fn no_answer_once(&self, by: Actor, brand: &BrandId, lead: &LeadId, now: Timestamp, key: &str) -> Result<Done<EventId>, ActionError> {
		let logged = idempotent_id(by, &format!("no_answer/logged/{brand}/{lead}"), key);
		if let Some(done) = self.replayed(by, Some(logged)).await? {
			return Ok(done.map(|(_, e)| e));
		}
		let current = self.lead_row(brand, lead).await?.ok_or(ActionError::NotFound)?;
		let attempt = idempotent_id(by, &format!("no_answer/attempt/{brand}/{lead}"), key);
		let attempt = self.act(by, Some(attempt), "call.attempted", subject_of(&current), json!({}), None, now).await?.value.1;
		let properties = json!({"outcome": "no_answer", "attemptId": attempt.raw().to_string()});
		Ok(self.act(by, Some(logged), "call.logged", subject_of(&current), properties, None, now).await?.map(|(_, e)| e))
	}

	/// A payment for a lead (`payment.received`).
	pub async fn record_payment(&self, by: Actor, brand: &BrandId, lead: &LeadId, p: Payment, now: Timestamp) -> Result<EventId, ActionError> {
		Ok(self.record_payment_once(by, brand, lead, p, now, None).await?.value)
	}

	/// [`Self::record_payment`], at most once per idempotency `key` of the user and lead.
	pub async fn record_payment_once(&self, by: Actor, brand: &BrandId, lead: &LeadId, p: Payment, now: Timestamp, key: Option<&str>) -> Result<Done<EventId>, ActionError> {
		let id = key.map(|k| idempotent_id(by, &format!("payment/{brand}/{lead}"), k));
		if let Some(done) = self.replayed(by, id).await? {
			return Ok(done.map(|(_, e)| e));
		}
		let current = self.lead_row(brand, lead).await?.ok_or(ActionError::NotFound)?;
		let properties = json!({"billed": p.billed, "commission": p.commission, "currency": p.currency});
		Ok(self.act(by, id, "payment.received", subject_of(&current), properties, None, now).await?.map(|(_, e)| e))
	}

	/// The event an idempotent action made before, if it did: `(lead, event)`. An id taken by
	/// anyone else than this user acting in the panel cannot be a retry of theirs.
	pub(crate) async fn replayed(&self, by: Actor, id: Option<Uuid>) -> Result<Option<Done<(LeadId, EventId)>>, ActionError> {
		let Some(id) = id else { return Ok(None) };
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		let Some((kind, source, lead)) = reads::event_origin(&mut conn, id).await? else {
			return Ok(None);
		};
		if kind != "panel" || source != by.0.to_string() {
			return Err(ActionError::Conflict("this idempotency key is taken"));
		}
		let lead = LeadId::parse(lead.as_deref().unwrap_or_default()).map_err(|e| eyre::eyre!("a journaled lead id: {e}"))?;
		Ok(Some(Done {
			value: (lead, EventId::from_raw(id)),
			replayed: true,
		}))
	}

	/// Builds the event and hands it to the journal exactly as ingest would.
	/// `id`: the event's, when the action is idempotent; else a fresh one. Answers the lead
	/// and the event.
	#[expect(clippy::too_many_arguments, reason = "an event's fields, each named at the call site")]
	pub(crate) async fn act(
		&self,
		by: Actor,
		id: Option<Uuid>,
		r#type: &str,
		subject: Value,
		properties: Value,
		pii: Option<Value>,
		now: Timestamp,
	) -> Result<Done<(LeadId, EventId)>, ActionError> {
		let mut raw = json!({
			"id": id.unwrap_or_else(|| new_uuid(now)).to_string(),
			"schema": SCHEMA,
			"type": r#type,
			"typeVersion": 1,
			"occurredAt": now.to_string(),
			"source": {"kind": "panel", "id": by.0.to_string()},
			"subject": subject,
			"properties": properties,
		});
		if let Some(pii) = pii {
			raw["pii"] = pii;
		}
		let (outcome, env) = self.write_own(raw, now).await?.map_err(ActionError::Invalid)?;
		match outcome {
			crate::Outcome::Accepted { .. } => {
				tracing::info!(user_id = %by.0, r#type, brand = %env.subject.brand_id, "operator action recorded");
				let lead = env.subject.lead_id.clone().ok_or_else(|| eyre::eyre!("an operator action without a lead"))?;
				Ok(Done {
					value: (lead, env.id),
					replayed: false,
				})
			}
			// The same key twice at once: the other request journaled it first (with its own
			// `occurredAt`, hence other content). What it recorded is the answer.
			outcome @ (crate::Outcome::Duplicate | crate::Outcome::Rejected(_) | crate::Outcome::Deferred(_)) => match self.replayed(by, id).await? {
				Some(done) => Ok(done),
				None => match outcome {
					crate::Outcome::Rejected(e) | crate::Outcome::Deferred(e) => Err(ActionError::Invalid(e)),
					_ => Err(ActionError::Internal(eyre::eyre!("a fresh event id was taken"))),
				},
			},
		}
	}

	pub(crate) async fn lead_row(&self, brand: &BrandId, lead: &LeadId) -> eyre::Result<Option<LeadRow>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		reads::lead(&mut conn, brand, lead).await
	}

	/// A page of leads, newest first.
	pub async fn leads(&self, q: &LeadQuery, pii: Pii, now: Timestamp) -> eyre::Result<LeadPage> {
		let limit = q.limit.clamp(1, MAX_PAGE);
		let filter = LeadFilter {
			stage: q.stage,
			brand: q.brand.clone(),
			location: q.location.clone(),
			waiting_since_before: q.overdue.then(|| now - funnel::CONTACT_SLA),
			created_from: q.created_from,
			created_before: q.created_before,
			suspect: match q.suspect {
				SuspectFilter::All => None,
				SuspectFilter::Only => Some(true),
				SuspectFilter::Exclude => Some(false),
			},
			flow: q.flow,
			booking: q.booking,
			channel: q.channel,
			message_ref: q.message_ref.clone(),
			after: q.after.clone(),
			limit: i64::from(limit) + 1,
		};
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		let mut rows = reads::leads(&mut conn, &filter).await?;
		drop(conn);
		let more = rows.len() > limit as usize;
		rows.truncate(limit as usize);
		let next = more.then(|| rows.last().map(|r| (r.sort_at, r.brand_id.clone(), r.lead_id.clone()))).flatten();
		let leads = rows.into_iter().map(|row| self.lead_view(row, pii, now)).collect::<eyre::Result<_>>()?;
		Ok(LeadPage { leads, next })
	}

	/// A lead and every event about it; `None` when there is no such lead.
	pub async fn lead_card(&self, brand: &BrandId, lead: &LeadId, pii: Pii, now: Timestamp) -> eyre::Result<Option<(LeadView, Vec<EventView>)>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		let Some(row) = reads::lead(&mut conn, brand, lead).await? else { return Ok(None) };
		let events = reads::lead_events(&mut conn, brand, lead).await?;
		drop(conn);
		let events = events
			.into_iter()
			.map(|row| {
				let pii = match pii {
					Pii::Reveal => self.open_pii(&row.sealed)?,
					Pii::Withhold => None,
				};
				Ok(EventView { row, pii })
			})
			.collect::<eyre::Result<_>>()?;
		Ok(Some((self.lead_view(row, pii, now)?, events)))
	}

	/// The personal funnel over the leads that came in from `from` to `to` (UTC days, both
	/// included).
	pub async fn funnel(&self, from: Date, to: Date, brand: Option<&BrandId>) -> eyre::Result<Totals> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		reads::funnel(&mut conn, from, to, brand).await
	}

	/// [`Self::funnel`] cut `by`, each slice with the payments of its leads (whenever they were
	/// paid). With [`FunnelBy::All`], a single slice, zeros included.
	pub async fn funnel_slices(&self, from: Date, to: Date, brand: Option<&BrandId>, by: FunnelBy) -> eyre::Result<Vec<FunnelSlice>> {
		let by_location = by == FunnelBy::Location;
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		let rows = reads::funnel_rows(&mut conn, from, to, brand, by_location).await?;
		let payments = reads::funnel_payments(&mut conn, from, to, brand, by_location).await?;
		drop(conn);
		let mut slices: Vec<FunnelSlice> = rows
			.into_iter()
			.map(|r| FunnelSlice {
				brand: r.brand_id,
				location: r.location_id,
				totals: r.totals,
				payments: Vec::new(),
			})
			.collect();
		if slices.is_empty() && !by_location {
			slices.push(FunnelSlice {
				brand: None,
				location: None,
				totals: Totals::default(),
				payments: Vec::new(),
			});
		}
		for p in payments {
			// Both are over the same leads: a payment's slice is always there.
			let slice = slices
				.iter_mut()
				.find(|s| s.brand == p.brand_id && s.location == p.location_id)
				.ok_or_else(|| eyre::eyre!("payments for {:?}/{:?} outside the funnel", p.brand_id, p.location_id))?;
			slice.payments.push(p);
		}
		Ok(slices)
	}

	/// Every place the panel knows, by brand: see [`reads::places`].
	pub async fn places(&self) -> eyre::Result<Vec<PlaceRow>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		reads::places(&mut conn).await
	}

	/// How many leads are at each stage, for one brand, location, both or neither.
	pub async fn lead_counts(&self, brand: Option<&BrandId>, location: Option<&LocationId>, now: Timestamp) -> eyre::Result<LeadCounts> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection")?;
		let (counted, overdue) = reads::stage_counts(&mut conn, brand, location, now - funnel::CONTACT_SLA).await?;
		let stages = Stage::ALL
			.into_iter()
			.map(|stage| (stage, counted.iter().find(|(s, _)| *s == stage).map_or(0, |(_, n)| *n)))
			.collect();
		Ok(LeadCounts { stages, overdue })
	}

	fn lead_view(&self, row: LeadRow, pii: Pii, now: Timestamp) -> eyre::Result<LeadView> {
		let revealed = match (pii, &row.creation) {
			(Pii::Reveal, Some(sealed)) => self.open_pii(sealed)?,
			_ => None,
		};
		Ok(LeadView {
			waiting: funnel::waiting(row.stage, row.created_at, row.contacted_at, now),
			pii: revealed,
			row,
		})
	}

	pub(crate) fn open_pii(&self, sealed: &Sealed) -> eyre::Result<Option<Value>> {
		let Some((blob, fp)) = &sealed.pii else { return Ok(None) };
		let id = sealed.event_id;
		eyre::ensure!(fp.as_slice() == self.key.fingerprint(), "the PII of event {id} was sealed under another PANEL_DATA_KEY");
		let plain = self.key.open(&pii_aad(id), blob).wrap_err_with(|| format!("opening the PII of event {id}"))?;
		Ok(Some(serde_json::from_slice(&plain).wrap_err("stored PII is not JSON")?))
	}
}

/// The subject of an action on a lead: its brand and id, and its location and job as the
/// projection has them.
pub(crate) fn subject_of(lead: &LeadRow) -> Value {
	let mut subject = json!({"brandId": lead.brand_id, "leadId": lead.lead_id});
	if let Some(location) = &lead.location_id {
		subject["locationId"] = json!(location);
	}
	if let Some(job) = &lead.job_id {
		subject["jobId"] = json!(job);
	}
	subject
}

/// An object of the fields that are set.
fn optional(fields: &[(&str, Option<Value>)]) -> Value {
	Value::Object(fields.iter().filter_map(|(k, v)| v.clone().map(|v| ((*k).to_owned(), v))).collect())
}

//! A lead's place in the funnel (spec §2, stages 5–10), folded from its facts.
//!
//! The fold sorts its input by `(occurred_at, id)` first, so it gives one answer however
//! the events arrived: a source may deliver late, out of order, or twice. That is what lets
//! the projection be recomputed from the journal at any time and agree with itself.

use jiff::Timestamp;

use crate::{
	event::{SourceKind, Subject},
	fact::{AnalyticsId, Fact, LeadChannel, LeadLocale, LeadOffer, LeadSuspect, MessageRef, Messenger},
	ids::{BrandId, EventId, JobId, LeadId, LocationId},
};

/// The personal stages, in funnel order. `Lost` is off to the side: any stage can end
/// there, and any later progress takes the lead out of it again.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Stage {
	Created,
	Contacted,
	Quoted,
	Won,
	Completed,
	Paid,
	Lost,
}

impl Stage {
	pub const ALL: [Self; 7] = [Self::Created, Self::Contacted, Self::Quoted, Self::Won, Self::Completed, Self::Paid, Self::Lost];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Created => "created",
			Self::Contacted => "contacted",
			Self::Quoted => "quoted",
			Self::Won => "won",
			Self::Completed => "completed",
			Self::Paid => "paid",
			Self::Lost => "lost",
		}
	}

	/// The stage a fact moves a lead to; `None` for facts that are about the lead without
	/// moving it (calls).
	pub fn of(fact: &Fact) -> Option<Self> {
		match fact {
			Fact::LeadCreated { .. } => Some(Self::Created),
			Fact::LeadContacted { .. } => Some(Self::Contacted),
			Fact::LeadQuoted { .. } => Some(Self::Quoted),
			Fact::JobWon => Some(Self::Won),
			Fact::JobCompleted => Some(Self::Completed),
			Fact::PaymentReceived { .. } => Some(Self::Paid),
			Fact::LeadLost { .. } => Some(Self::Lost),
			// A booking is beside the stages, not one of them: a lead booked and never
			// contacted is still waiting for its call.
			// Writing on a messenger is the customer's move, not the operator's contact.
			Fact::CallAttempted
			| Fact::CallLogged { .. }
			| Fact::LeadMessaged { .. }
			| Fact::RetiredCount
			| Fact::ExperimentsDeclared(_)
			| Fact::ExperimentConfigured { .. }
			| Fact::BookingRequested { .. }
			| Fact::BookingCreated { .. }
			| Fact::BookingCanceled { .. }
			| Fact::BookingSet { .. }
			| Fact::BookingStatusChanged(_)
			| Fact::BookingCleared
			| Fact::BookingAttached { .. } => None,
		}
	}
}

impl std::str::FromStr for Stage {
	type Err = crate::Invalid;

	fn from_str(s: &str) -> Result<Self, crate::Invalid> {
		Self::ALL
			.into_iter()
			.find(|stage| stage.as_str() == s)
			.ok_or_else(|| crate::Invalid::new("stage is not one of created, contacted, quoted, won, completed, paid, lost"))
	}
}

/// A registered event about a lead, as the journal holds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recorded {
	pub id: EventId,
	pub occurred_at: Timestamp,
	/// When the panel journaled it: which of several `lead.created` counts.
	pub received_at: Timestamp,
	pub source_kind: SourceKind,
	pub subject: Subject,
	pub fact: Fact,
}

/// When a lead first reached each stage.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StageTimes {
	pub created: Option<Timestamp>,
	pub contacted: Option<Timestamp>,
	pub quoted: Option<Timestamp>,
	pub won: Option<Timestamp>,
	pub completed: Option<Timestamp>,
	pub paid: Option<Timestamp>,
	pub lost: Option<Timestamp>,
}

impl StageTimes {
	fn reach(&mut self, stage: Stage, at: Timestamp) {
		let slot = match stage {
			Stage::Created => &mut self.created,
			Stage::Contacted => &mut self.contacted,
			Stage::Quoted => &mut self.quoted,
			Stage::Won => &mut self.won,
			Stage::Completed => &mut self.completed,
			Stage::Paid => &mut self.paid,
			Stage::Lost => &mut self.lost,
		};
		slot.get_or_insert(at);
	}
}

/// A lead as the funnel sees it now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeadState {
	pub brand_id: BrandId,
	pub lead_id: LeadId,
	/// The latest location any of its events named.
	pub location_id: Option<LocationId>,
	/// The latest job any of its events named.
	pub job_id: Option<JobId>,
	pub stage: Stage,
	pub times: StageTimes,
	/// Set while the lead is lost: the reason of the loss that put it there.
	pub lost_reason: Option<String>,
	/// How it came in, from its `lead.created`; `None` when that was never registered.
	pub channel: Option<LeadChannel>,
	/// Why the landing's antispam doubted it, from its `lead.created`; `None` for an ordinary
	/// lead, and when that was never registered.
	pub suspect: Option<LeadSuspect>,
	/// Its flow and the price it was shown, from its `lead.created`; empty when that said
	/// nothing of them, or was never registered.
	pub offer: LeadOffer,
	/// The landing's analytics id from its `lead.created`: who the lead is in PostHog.
	pub analytics_id: Option<AnalyticsId>,
	/// The ref the customer carries into a messenger, from its `lead.created`.
	pub message_ref: Option<MessageRef>,
	/// The landing's locale from its `lead.created`; `None` reads as French.
	pub locale: Option<LeadLocale>,
	/// When the customer first wrote on a messenger (`lead.messaged`), and on which.
	pub messaged: Option<(Timestamp, Messenger)>,
	/// Its `lead.created` was typed in by a person (spec §10a).
	pub manual: bool,
	pub last_event_id: EventId,
	pub last_event_at: Timestamp,
	/// The `lead.created` that counts (the first journaled); `None` while there is none.
	pub creation: Option<EventId>,
}

/// Folds a lead's facts into its state; `None` for no facts. Every event must be about the
/// same brand and lead — the caller selects them that way.
///
/// The stage follows the latest progress: it only moves forward through the funnel (a
/// `lead.contacted` after `job.won` is a call about the job, not a step back), a
/// `lead.lost` moves it to `Lost` from anywhere, and progress after a loss reopens it.
///
/// A lead is created once: the `lead.created` the panel journaled first — by `received_at`,
/// not `occurred_at` — is the one that counts, and any later one is left out altogether. A
/// source could otherwise back-date a second creation and take over a lead someone else
/// made: its channel, whether it was typed in by hand, when it came in.
pub fn fold(events: &[Recorded]) -> Option<LeadState> {
	let creation = events
		.iter()
		.filter(|e| matches!(e.fact, Fact::LeadCreated { .. }))
		.min_by_key(|e| (e.received_at, e.id.raw()))
		.map(|e| e.id);
	let mut ordered: Vec<&Recorded> = events.iter().filter(|e| !matches!(e.fact, Fact::LeadCreated { .. }) || Some(e.id) == creation).collect();
	ordered.sort_by_key(|e| (e.occurred_at, e.id.raw()));
	let first = *ordered.first()?;
	let lead_id = first.subject.lead_id.clone()?;

	let mut state = LeadState {
		brand_id: first.subject.brand_id.clone(),
		lead_id,
		location_id: None,
		job_id: None,
		stage: Stage::Created,
		times: StageTimes::default(),
		lost_reason: None,
		channel: None,
		suspect: None,
		offer: LeadOffer::default(),
		analytics_id: None,
		message_ref: None,
		locale: None,
		messaged: None,
		manual: false,
		last_event_id: first.id,
		last_event_at: first.occurred_at,
		creation,
	};
	for e in ordered {
		debug_assert_eq!(e.subject.brand_id, state.brand_id, "fold is per lead");
		if let Some(location) = &e.subject.location_id {
			state.location_id = Some(location.clone());
		}
		if let Some(job) = &e.subject.job_id {
			state.job_id = Some(job.clone());
		}
		if let Fact::LeadCreated {
			channel,
			suspect,
			offer,
			analytics_id,
			message_ref,
			locale,
			..
		} = &e.fact
		{
			state.channel = Some(*channel);
			state.suspect = *suspect;
			state.offer = offer.clone();
			state.analytics_id = analytics_id.clone();
			state.message_ref = message_ref.clone();
			state.locale = *locale;
			state.manual = e.source_kind.is_manual();
		}
		// The first message: the events are in time order, so a later one leaves it.
		if let Fact::LeadMessaged { channel, .. } = &e.fact {
			state.messaged.get_or_insert((e.occurred_at, *channel));
		}
		if let Some(reached) = Stage::of(&e.fact) {
			state.times.reach(reached, e.occurred_at);
			state.stage = match (state.stage, reached) {
				(_, Stage::Lost) => Stage::Lost,
				(Stage::Lost, progress) => progress,
				(current, progress) => current.max(progress),
			};
			state.lost_reason = match &e.fact {
				Fact::LeadLost { reason, .. } => Some(reason.clone()),
				_ if state.stage == Stage::Lost => state.lost_reason,
				_ => None,
			};
		}
		state.last_event_id = e.id;
		state.last_event_at = e.occurred_at;
	}
	Some(state)
}

#[cfg(test)]
mod tests {
	use jiff::SignedDuration;
	use uuid::Uuid;

	use super::*;
	use crate::fact::CallOutcome;

	fn at(minutes: i64) -> Timestamp {
		"2026-09-30T10:00:00Z".parse::<Timestamp>().unwrap() + SignedDuration::from_mins(minutes)
	}

	fn ev(minutes: i64, kind: SourceKind, fact: Fact) -> Recorded {
		Recorded {
			id: EventId::from_raw(Uuid::now_v7()),
			occurred_at: at(minutes),
			received_at: at(minutes),
			source_kind: kind,
			subject: Subject {
				brand_id: BrandId::parse("aquafix").unwrap(),
				location_id: Some(LocationId::parse("paris-11").unwrap()),
				lead_id: Some(LeadId::parse("L-1").unwrap()),
				job_id: matches!(fact, Fact::JobWon | Fact::JobCompleted).then(|| JobId::parse("J-1").unwrap()),
			},
			fact,
		}
	}

	fn created() -> Fact {
		Fact::LeadCreated {
			channel: LeadChannel::Form,
			entered_by: None,
			suspect: None,
			offer: LeadOffer::default(),
			analytics_id: None,
			message_ref: None,
			locale: None,
		}
	}

	#[test]
	fn the_happy_path_to_paid() {
		let events = [
			ev(0, SourceKind::Site, created()),
			ev(1, SourceKind::Panel, Fact::CallAttempted),
			ev(
				2,
				SourceKind::Panel,
				Fact::CallLogged {
					outcome: CallOutcome::Answered,
					attempt_id: None,
				},
			),
			ev(3, SourceKind::Panel, Fact::LeadContacted { channel: None }),
			ev(10, SourceKind::Panel, Fact::LeadQuoted { quote: None }),
			ev(20, SourceKind::Panel, Fact::JobWon),
			ev(30, SourceKind::Panel, Fact::LeadContacted { channel: None }),
			ev(60, SourceKind::Panel, Fact::JobCompleted),
			ev(90, SourceKind::Panel, Fact::payment(12_000, 1_800, "EUR").unwrap()),
		];
		let s = fold(&events).unwrap();
		assert_eq!(s.stage, Stage::Paid);
		assert_eq!(s.times.created, Some(at(0)));
		assert_eq!(s.times.contacted, Some(at(3)), "the first contact counts, not the call about the job");
		assert_eq!(s.times.paid, Some(at(90)));
		assert_eq!(s.times.lost, None);
		assert_eq!(s.job_id.unwrap().as_str(), "J-1");
		assert!(!s.manual);
		assert_eq!(s.channel, Some(LeadChannel::Form));
		assert_eq!(s.last_event_at, at(90));
	}

	#[test]
	fn arrival_order_does_not_matter() {
		let events = vec![
			ev(
				0,
				SourceKind::Panel,
				Fact::LeadCreated {
					channel: LeadChannel::PhoneInbound,
					entered_by: Some("u1".into()),
					suspect: None,
					offer: LeadOffer::default(),
					analytics_id: None,
					message_ref: None,
					locale: None,
				},
			),
			ev(5, SourceKind::Panel, Fact::LeadContacted { channel: None }),
			ev(9, SourceKind::Panel, Fact::lost("no_answer", None).unwrap()),
		];
		let forward = fold(&events).unwrap();
		let mut reversed = events.clone();
		reversed.reverse();
		assert_eq!(fold(&reversed).unwrap(), forward);
		assert_eq!(forward.stage, Stage::Lost);
		assert_eq!(forward.lost_reason.as_deref(), Some("no_answer"));
		assert!(forward.manual, "typed in from the panel");
	}

	#[test]
	fn progress_after_a_loss_reopens_it() {
		let s = fold(&[
			ev(0, SourceKind::Site, created()),
			ev(5, SourceKind::Panel, Fact::lost("no_answer", None).unwrap()),
			ev(60, SourceKind::Panel, Fact::LeadContacted { channel: None }),
		])
		.unwrap();
		assert_eq!(s.stage, Stage::Contacted);
		assert_eq!(s.lost_reason, None);
		assert_eq!(s.times.lost, Some(at(5)), "that it was lost once stays on record");
	}

	#[test]
	fn the_first_creation_journaled_is_the_one() {
		let by_hand = ev(
			10,
			SourceKind::Panel,
			Fact::LeadCreated {
				channel: LeadChannel::PhoneInbound,
				entered_by: Some("op-1".into()),
				suspect: None,
				offer: LeadOffer::default(),
				analytics_id: None,
				message_ref: None,
				locale: None,
			},
		);
		let mut back_dated = ev(0, SourceKind::Site, created());
		back_dated.received_at = at(20);
		let s = fold(&[by_hand.clone(), back_dated.clone()]).unwrap();
		assert!(s.manual);
		assert_eq!(s.channel, Some(LeadChannel::PhoneInbound));
		assert_eq!(s.times.created, Some(at(10)), "the back-dated one does not move it");
		assert_eq!(s.last_event_id, by_hand.id);
		assert_eq!(fold(&[back_dated, by_hand]).unwrap(), s, "whatever the order they are read in");
	}

	#[test]
	fn the_suspect_mark_is_the_counting_creations() {
		let doubted = ev(
			0,
			SourceKind::Site,
			Fact::LeadCreated {
				channel: LeadChannel::Form,
				entered_by: None,
				suspect: Some(LeadSuspect::TooFast),
				offer: LeadOffer::default(),
				analytics_id: None,
				message_ref: None,
				locale: None,
			},
		);
		let s = fold(&[doubted.clone(), ev(5, SourceKind::Panel, Fact::LeadContacted { channel: None })]).unwrap();
		assert_eq!(s.suspect, Some(LeadSuspect::TooFast), "progress does not clear it");
		let mut again = ev(1, SourceKind::Site, created());
		again.received_at = at(30);
		assert_eq!(fold(&[again, doubted]).unwrap().suspect, Some(LeadSuspect::TooFast), "a later clean creation does not either");
	}

	#[test]
	fn the_offer_is_the_counting_creations() {
		let estimate = LeadOffer::parse(Some("estimate"), Some(12_900), Some("2026-10-01"), [("zone".to_owned(), "a".to_owned())]).unwrap();
		let priced = ev(
			0,
			SourceKind::Site,
			Fact::LeadCreated {
				channel: LeadChannel::Form,
				entered_by: None,
				suspect: None,
				offer: estimate.clone(),
				analytics_id: None,
				message_ref: None,
				locale: None,
			},
		);
		let mut again = ev(1, SourceKind::Site, created());
		again.received_at = at(30);
		let s = fold(&[again, priced, ev(5, SourceKind::Panel, Fact::LeadContacted { channel: None })]).unwrap();
		assert_eq!(s.offer, estimate, "a later creation saying nothing does not clear it, nor does progress");
	}

	#[test]
	fn the_analytics_id_is_the_counting_creations() {
		let with_id = |minutes, received, id: &str| {
			let mut e = ev(
				minutes,
				SourceKind::Site,
				Fact::LeadCreated {
					channel: LeadChannel::Form,
					entered_by: None,
					suspect: None,
					offer: LeadOffer::default(),
					analytics_id: Some(AnalyticsId::parse(id).unwrap()),
					message_ref: None,
					locale: None,
				},
			);
			e.received_at = at(received);
			e
		};
		let s = fold(&[with_id(1, 30, "later"), with_id(0, 0, "first"), ev(5, SourceKind::Panel, Fact::JobWon)]).unwrap();
		assert_eq!(s.analytics_id.unwrap().as_str(), "first", "a later creation cannot take the lead over in PostHog");
	}

	#[test]
	fn the_first_message_is_kept_beside_the_stages() {
		let messaged = |channel| Fact::LeadMessaged {
			channel,
			message_ref: Some(MessageRef::parse("AQ-7K3F").unwrap()),
		};
		let created = ev(
			0,
			SourceKind::Site,
			Fact::LeadCreated {
				channel: LeadChannel::Whatsapp,
				entered_by: None,
				suspect: None,
				offer: LeadOffer::default(),
				analytics_id: None,
				message_ref: Some(MessageRef::parse("AQ-7K3F").unwrap()),
				locale: None,
			},
		);
		let events = [
			ev(9, SourceKind::Panel, messaged(Messenger::Whatsapp)),
			created,
			ev(4, SourceKind::Bot, messaged(Messenger::Telegram)),
		];
		let s = fold(&events).unwrap();
		assert_eq!(s.stage, Stage::Created, "a message is not a contact");
		assert_eq!(s.channel, Some(LeadChannel::Whatsapp));
		assert_eq!(s.message_ref.unwrap().as_str(), "AQ-7K3F");
		assert_eq!(s.messaged, Some((at(4), Messenger::Telegram)), "the first, whatever the order they arrived in");
	}

	#[test]
	fn a_lead_seen_before_its_creation() {
		let s = fold(&[ev(5, SourceKind::Panel, Fact::CallAttempted)]).unwrap();
		assert_eq!(s.stage, Stage::Created);
		assert_eq!(s.times.created, None);
		assert_eq!(s.channel, None);
		assert_eq!(s.suspect, None);
		assert!(fold(&[]).is_none());
	}
}

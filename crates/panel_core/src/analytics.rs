//! What the panel tells PostHog of a lead's life after the form (`sa_lead_created`,
//! `sa_lead_contacted`, …), so the funnel from a visit to a payment is one funnel there.
//!
//! Never PII: a brand, a location, the closed vocabularies (channel, flow, outcome, a loss's
//! reason slug) and amounts. Not a messenger ref either: PostHog has the lead's person already,
//! and the ref is what a customer reads out to an operator. Never the customer's words, a note, a name or a phone. The person
//! is the landing's analytics id from the lead's creation, so the visit and the lead are one
//! person in PostHog; a lead without one (typed in, or from a landing before the field) is
//! `sa-lead:<brand>:<lead>`.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::{Value, json};

use crate::{
	booking::BookingMatch,
	fact::{AnalyticsId, Fact},
	lead::Recorded,
};

/// One event for PostHog: its name and properties. Its `uuid` is the journal's event id, so a
/// retry is deduplicated there.
#[derive(Clone, Debug, PartialEq)]
pub struct Capture {
	pub event: &'static str,
	pub properties: BTreeMap<&'static str, Value>,
}

/// What PostHog is told of a journaled event, if anything: `sa_` and its type, dots made
/// underscores — but `booking.status_changed`, which only ever closes a slot, is
/// `sa_booking_closed`. A call attempt is not told (its outcome is), nor what is about no lead: a provider's booking
/// no lead was matched to stays out, and the contact it may be matched by is never read here.
pub fn capture_of(e: &Recorded) -> Option<Capture> {
	let mut p: BTreeMap<&'static str, Value> = BTreeMap::new();
	let event = match &e.fact {
		Fact::LeadCreated { channel, suspect, offer, .. } => {
			p.insert("channel", json!(channel.as_str()));
			if let Some(flow) = offer.flow {
				p.insert("flow", json!(flow.as_str()));
			}
			if let Some(price) = offer.price {
				p.insert("quoted_cents", json!(price.cents));
			}
			if let Some(s) = suspect {
				p.insert("suspect", json!(s.as_str()));
			}
			"sa_lead_created"
		}
		Fact::LeadMessaged { channel, .. } => {
			p.insert("channel", json!(channel.as_str()));
			"sa_lead_messaged"
		}
		Fact::LeadContacted { channel } => {
			if let Some(c) = channel {
				p.insert("channel", json!(c.as_str()));
			}
			"sa_lead_contacted"
		}
		Fact::LeadQuoted { quote } => {
			if let Some(q) = quote {
				p.insert("amount_cents", json!(q.minor));
				p.insert("currency", json!(q.currency.as_str()));
			}
			"sa_lead_quoted"
		}
		Fact::JobWon => "sa_job_won",
		// The reason is a slug the operator picks; the note is their words, and stays.
		Fact::LeadLost { reason, .. } => {
			p.insert("reason", json!(reason));
			"sa_lead_lost"
		}
		Fact::JobCompleted => "sa_job_completed",
		Fact::PaymentReceived { billed, commission } => {
			p.insert("billed_cents", json!(billed.minor));
			p.insert("commission_cents", json!(commission));
			p.insert("currency", json!(billed.currency.as_str()));
			"sa_payment_received"
		}
		Fact::CallLogged { outcome, .. } => {
			p.insert("outcome", json!(outcome.as_str()));
			"sa_call_logged"
		}
		// The wish as the site's closed vocabulary: a day and a part of it, never the visitor's
		// words.
		Fact::BookingRequested {
			provider,
			preferred_date,
			preferred_part,
		} => {
			p.insert("provider", json!(provider.as_str()));
			if let Some(d) = preferred_date {
				p.insert("preferred_date", json!(d.to_string()));
			}
			if let Some(part) = preferred_part {
				p.insert("preferred_part", json!(part.as_str()));
			}
			"sa_booking_requested"
		}
		// The provider's id and revision stay home: a ref opens the calendar entry, and its
		// attendee. The slot is told as how far ahead it was booked, not when it is.
		Fact::BookingCreated {
			provider,
			start_at,
			matched,
			booked_at,
			..
		} => {
			p.insert("provider", json!(provider.as_str()));
			if let Some(m) = matched {
				p.insert("match", json!(m.as_str()));
			}
			p.insert("lead_time_hours", json!(lead_time_hours(*start_at, booked_at.unwrap_or(e.occurred_at))));
			"sa_booking_created"
		}
		Fact::BookingCanceled { provider, .. } => {
			p.insert("provider", json!(provider.as_str()));
			"sa_booking_canceled"
		}
		Fact::BookingSet { start_at, .. } => {
			p.insert("lead_time_hours", json!(lead_time_hours(*start_at, e.occurred_at)));
			"sa_booking_set"
		}
		Fact::BookingStatusChanged(closed) => {
			p.insert("closed", json!(closed.as_str()));
			"sa_booking_closed"
		}
		Fact::BookingCleared => "sa_booking_cleared",
		Fact::BookingAttached { provider, .. } => {
			p.insert("provider", json!(provider.as_str()));
			p.insert("match", json!(BookingMatch::Manual.as_str()));
			"sa_booking_attached"
		}
		Fact::CallAttempted | Fact::RetiredCount | Fact::ExperimentsDeclared(_) | Fact::ExperimentConfigured { .. } => return None,
	};
	e.subject.lead_id.as_ref()?;
	p.insert("brand_id", json!(e.subject.brand_id.as_str()));
	if let Some(l) = &e.subject.location_id {
		p.insert("location_id", json!(l.as_str()));
	}
	p.insert("manual", json!(e.source_kind.is_manual()));
	Some(Capture { event, properties: p })
}

/// Whole hours from `from` to the slot's `start`; negative when it was entered after the fact.
/// The slot's weekday and part of the day would need the place's time zone, which a fact does
/// not carry.
fn lead_time_hours(start: Timestamp, from: Timestamp) -> i64 {
	start.duration_since(from).as_hours()
}

/// Who the lead is in PostHog: the landing's analytics id, else one made of the lead.
pub fn distinct_id(brand: &str, lead: &str, analytics_id: Option<&AnalyticsId>) -> String {
	match analytics_id {
		Some(id) => id.as_str().to_owned(),
		None => format!("sa-lead:{brand}:{lead}"),
	}
}

#[cfg(test)]
mod tests {
	use uuid::Uuid;

	use super::*;
	use crate::{
		booking::{Closed, DayPart, Provider},
		event::{SourceKind, Subject},
		fact::{CallOutcome, LeadChannel, LeadOffer, MessageRef, Messenger},
		ids::{BrandId, EventId, LeadId, LocationId},
	};

	fn rec(fact: Fact) -> Recorded {
		Recorded {
			id: EventId::from_raw(Uuid::now_v7()),
			occurred_at: Timestamp::UNIX_EPOCH,
			received_at: Timestamp::UNIX_EPOCH,
			source_kind: SourceKind::Panel,
			subject: Subject {
				brand_id: BrandId::parse("aquafix").unwrap(),
				location_id: Some(LocationId::parse("paris-11").unwrap()),
				lead_id: Some(LeadId::parse("L-1").unwrap()),
				job_id: None,
			},
			fact,
		}
	}

	#[test]
	fn names_and_no_pii() {
		let lost = capture_of(&rec(Fact::lost("too_expensive", Some("called him a liar, +33 6 00".into())).unwrap())).unwrap();
		assert_eq!(lost.event, "sa_lead_lost");
		assert_eq!(lost.properties["reason"], "too_expensive");
		assert!(!format!("{:?}", lost.properties).contains("+33"), "the note stays home");
		let paid = capture_of(&rec(Fact::payment(12_000, 1_800, "EUR").unwrap())).unwrap();
		assert_eq!((paid.event, paid.properties["billed_cents"].clone()), ("sa_payment_received", json!(12_000)));
		let created = capture_of(&rec(Fact::LeadCreated {
			channel: LeadChannel::Form,
			entered_by: Some("op-1".into()),
			suspect: None,
			offer: LeadOffer::parse(Some("fixed"), Some(9_900), Some("2026-10-01"), []).unwrap(),
			analytics_id: None,
			message_ref: Some(MessageRef::parse("AQ-7K3F").unwrap()),
			locale: None,
		}))
		.unwrap();
		assert_eq!(created.event, "sa_lead_created");
		assert_eq!(
			created.properties.keys().copied().collect::<Vec<_>>(),
			["brand_id", "channel", "flow", "location_id", "manual", "quoted_cents"]
		);
		let call = capture_of(&rec(Fact::CallLogged {
			outcome: CallOutcome::NoAnswer,
			attempt_id: None,
		}))
		.unwrap();
		assert_eq!((call.event, call.properties["outcome"].clone()), ("sa_call_logged", json!("no_answer")));
		assert_eq!(capture_of(&rec(Fact::CallAttempted)), None);
		assert_eq!(capture_of(&rec(Fact::RetiredCount)), None);
		let messaged = capture_of(&rec(Fact::LeadMessaged {
			channel: Messenger::Whatsapp,
			message_ref: Some(MessageRef::parse("AQ-7K3F").unwrap()),
		}))
		.unwrap();
		assert_eq!(messaged.event, "sa_lead_messaged");
		assert_eq!(messaged.properties.keys().copied().collect::<Vec<_>>(), ["brand_id", "channel", "location_id", "manual"]);
		assert_eq!(messaged.properties["channel"], "whatsapp");
	}

	fn created(matched: Option<BookingMatch>, booked_at: Option<Timestamp>) -> Fact {
		Fact::BookingCreated {
			provider: Provider::GoogleCalendar,
			external_ref: "evt-ref-secret".into(),
			start_at: "2026-10-08T09:00:00Z".parse().unwrap(),
			end_at: None,
			matched,
			version: "etag-rev-7".into(),
			booked_at,
		}
	}

	#[test]
	fn a_booking_no_lead_was_matched_to_is_not_told() {
		let mut lone = rec(created(None, None));
		lone.subject.lead_id = None;
		assert_eq!(capture_of(&lone), None);
		let mut canceled = rec(Fact::BookingCanceled {
			provider: Provider::CalCom,
			external_ref: "evt-ref-secret".into(),
			version: "etag-rev-7".into(),
		});
		canceled.subject.lead_id = None;
		assert_eq!(capture_of(&canceled), None);
	}

	#[test]
	fn a_booking_without_its_ref_or_slot() {
		let mut booked = rec(created(Some(BookingMatch::Ref), Some("2026-10-06T09:00:00Z".parse().unwrap())));
		booked.occurred_at = "2026-10-07T09:00:00Z".parse().unwrap();
		let c = capture_of(&booked).unwrap();
		assert_eq!(c.event, "sa_booking_created");
		assert_eq!(
			c.properties.keys().copied().collect::<Vec<_>>(),
			["brand_id", "lead_time_hours", "location_id", "manual", "match", "provider"]
		);
		assert_eq!((c.properties["provider"].clone(), c.properties["match"].clone()), (json!("google_calendar"), json!("ref")));
		assert_eq!(c.properties["lead_time_hours"], 48, "from when it was booked, not when the panel heard");
		let dump = format!("{:?}", c.properties);
		assert!(!dump.contains("evt-ref-secret") && !dump.contains("etag-rev-7") && !dump.contains("2026-10-08"), "{dump}");

		let mut heard = rec(created(Some(BookingMatch::Contact), None));
		heard.occurred_at = "2026-10-07T21:00:00Z".parse().unwrap();
		assert_eq!(capture_of(&heard).unwrap().properties["lead_time_hours"], 12, "no booked_at: from the event");

		let canceled = capture_of(&rec(Fact::BookingCanceled {
			provider: Provider::CalCom,
			external_ref: "evt-ref-secret".into(),
			version: "etag-rev-7".into(),
		}))
		.unwrap();
		assert_eq!((canceled.event, canceled.properties["provider"].clone()), ("sa_booking_canceled", json!("cal_com")));
		assert!(!format!("{:?}", canceled.properties).contains("evt-ref"));

		let attached = capture_of(&rec(Fact::BookingAttached {
			provider: Provider::GoogleCalendar,
			external_ref: "evt-ref-secret".into(),
		}))
		.unwrap();
		assert_eq!((attached.event, attached.properties["match"].clone()), ("sa_booking_attached", json!("manual")));
		assert!(!format!("{:?}", attached.properties).contains("evt-ref"));
	}

	#[test]
	fn the_operators_side_of_a_booking() {
		let requested = capture_of(&rec(Fact::BookingRequested {
			provider: Provider::Manual,
			preferred_date: Some("2026-10-09".parse().unwrap()),
			preferred_part: Some(DayPart::Evening),
		}))
		.unwrap();
		assert_eq!(requested.event, "sa_booking_requested");
		assert_eq!(
			(requested.properties["preferred_date"].clone(), requested.properties["preferred_part"].clone()),
			(json!("2026-10-09"), json!("evening"))
		);
		let mut set = rec(Fact::BookingSet {
			start_at: "2026-10-02T10:30:00Z".parse().unwrap(),
			end_at: None,
		});
		set.occurred_at = "2026-10-01T10:00:00Z".parse().unwrap();
		let set = capture_of(&set).unwrap();
		assert_eq!((set.event, set.properties["lead_time_hours"].clone()), ("sa_booking_set", json!(24)));
		let closed = capture_of(&rec(Fact::BookingStatusChanged(Closed::NoShow))).unwrap();
		assert_eq!((closed.event, closed.properties["closed"].clone()), ("sa_booking_closed", json!("no_show")));
		assert_eq!(capture_of(&rec(Fact::BookingCleared)).unwrap().event, "sa_booking_cleared");
	}

	#[test]
	fn the_person() {
		assert_eq!(distinct_id("aquafix", "L-1", None), "sa-lead:aquafix:L-1");
		let id = AnalyticsId::parse("0192f1c2-7d1e-7b3a-9c4d-1a2b3c4d5e6f").unwrap();
		assert_eq!(distinct_id("aquafix", "L-1", Some(&id)), id.as_str());
	}
}

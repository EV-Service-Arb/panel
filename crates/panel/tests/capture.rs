//! What PostHog is owed: queued in the journal's transaction for new lead events only — never
//! by a rebuild, a duplicate or a second creation — under the landing's analytics id, and sent,
//! retried and given up as `panel::capture` says. A booking is told under its lead, and a
//! provider's booking no lead was matched to is not told at all.

use std::sync::Mutex;

use jiff::{SignedDuration, Timestamp};
use panel::{
	Panel,
	capture::{Captured, Capturer, Delivered, GIVE_UP_AFTER, SendError},
	operator::{Actor, StageMove},
	testing::{TestDb, event, panel, sign},
};
use panel_core::{
	booking::{BookingEvent, Change, Contact, Provider},
	event::SourceKind,
	ids::{BrandId, LeadId},
};
use serde_json::{Value, json};

/// Answers each send with the next of `answers` (then Ok), and remembers every batch.
#[derive(Default)]
struct Fake {
	answers: Mutex<Vec<Result<(), SendError>>>,
	sent: Mutex<Vec<Vec<Captured>>>,
}

impl Capturer for Fake {
	async fn send(&self, batch: &[Captured]) -> Result<(), SendError> {
		self.sent.lock().unwrap().push(batch.to_vec());
		let mut answers = self.answers.lock().unwrap();
		if answers.is_empty() { Ok(()) } else { answers.remove(0) }
	}
}

async fn site(panel: &Panel) -> String {
	let brands = [BrandId::parse("aquafix").unwrap()].into();
	panel.add_source("aquafix-site", SourceKind::Site, brands).await.unwrap().unwrap().secret.to_string()
}

fn created(lead: &str, at: Timestamp, analytics_id: Option<&str>) -> Value {
	let mut props = json!({"channel": "form"});
	if let Some(id) = analytics_id {
		props["analyticsId"] = json!(id);
	}
	event("lead.created", at, "site", json!({"brandId": "aquafix", "locationId": "paris-11", "leadId": lead}), props)
}

async fn outbox(panel: &Panel) -> Vec<(String, String, Value)> {
	sqlx::query_as::<_, (String, String, sqlx::types::Json<Value>)>("SELECT event, distinct_id, properties FROM posthog_outbox ORDER BY occurred_at, event_id")
		.fetch_all(panel.store().pool())
		.await
		.unwrap()
		.into_iter()
		.map(|(e, d, p)| (e, d, p.0))
		.collect()
}

#[tokio::test]
async fn only_a_fresh_commit_queues_and_never_the_rebuild() {
	let db = TestDb::create().await;
	let panel = panel(&db).await.with_capture(true);
	let secret = site(&panel).await;
	let now = Timestamp::now();
	let first = created("L-1", now - SignedDuration::from_mins(3), Some("0192f1c2-7d1e-7b3a-9c4d-1a2b3c4d5e6f"));
	let got = panel
		.ingest(sign("aquafix-site", &secret, &[first.clone(), created("L-2", now, None)], now).batch(), now)
		.await
		.unwrap();
	assert!(got.iter().all(|v| v.outcome == panel::Outcome::Accepted { unregistered: false }), "{got:?}");
	// Sent again: a duplicate. A second creation of L-1 is journaled, and does not count.
	panel
		.ingest(sign("aquafix-site", &secret, &[first, created("L-1", now, Some("other"))], now).batch(), now)
		.await
		.unwrap();

	let brand = BrandId::parse("aquafix").unwrap();
	let by = Actor(uuid::Uuid::now_v7());
	let l1 = LeadId::parse("L-1").unwrap();
	// A minute on: the outbox is read in occurred_at order, and ties fall to the ids' random bits.
	let later = now + SignedDuration::from_mins(1);
	panel.move_lead(by, &brand, &l1, StageMove::Contacted { channel: Some("phone".into()) }, later).await.unwrap();
	panel.attempt_call(by, &brand, &l1, later).await.unwrap();

	let queued = outbox(&panel).await;
	let names: Vec<(&str, &str)> = queued.iter().map(|(e, d, _)| (e.as_str(), d.as_str())).collect();
	assert_eq!(
		names,
		[
			("sa_lead_created", "0192f1c2-7d1e-7b3a-9c4d-1a2b3c4d5e6f"),
			("sa_lead_created", "sa-lead:aquafix:L-2"),
			("sa_lead_contacted", "0192f1c2-7d1e-7b3a-9c4d-1a2b3c4d5e6f"),
		],
		"no duplicate, no second creation, no call attempt; the lead's later life under its visit's id"
	);
	assert_eq!(queued[2].2, json!({"brand_id": "aquafix", "channel": "phone", "location_id": "paris-11", "manual": true}));

	panel.rebuild_projections().await.unwrap();
	assert_eq!(outbox(&panel).await, queued, "the rebuild tells PostHog nothing");

	let off = Panel::new(db.store().await, panel::seal::DataKey::from_hex(&"0".repeat(64)).unwrap());
	let l2 = LeadId::parse("L-2").unwrap();
	off.move_lead(by, &brand, &l2, StageMove::Contacted { channel: None }, now).await.unwrap();
	assert_eq!(outbox(&panel).await.len(), 3, "capture off: nothing queued");
}

#[tokio::test]
async fn a_booking_is_told_under_its_lead_and_never_without_one() {
	let db = TestDb::create().await;
	let panel = panel(&db).await.with_capture(true);
	let secret = site(&panel).await;
	let now = Timestamp::now();
	let visit = "0192f1c2-7d1e-7b3a-9c4d-1a2b3c4d5e6f";
	panel
		.ingest(
			sign("aquafix-site", &secret, &[created("L-1", now - SignedDuration::from_mins(3), Some(visit))], now).batch(),
			now,
		)
		.await
		.unwrap();

	let brand = BrandId::parse("aquafix").unwrap();
	let later = now + SignedDuration::from_mins(1);
	let booking = |external_ref: &str, lead_ref: Option<&str>| BookingEvent {
		provider: Provider::GoogleCalendar,
		external_ref: external_ref.into(),
		version: "etag-rev-7".into(),
		at: later,
		change: Change::Booked {
			start: later + SignedDuration::from_hours(48),
			end: None,
			booked_at: Some(later),
		},
		lead_ref: lead_ref.map(str::to_owned),
		contact: Contact {
			name: Some("Jean Dupont".into()),
			email: Some("jean@example.com".into()),
			phone: Some("+33700000000".into()),
		},
	};
	// One no lead matches (L-1 left no phone or email), one the booking page carried L-1 back on.
	let got = panel
		.ingest_bookings(&brand, vec![booking("evt-lone", None), booking("evt-ref-secret", Some("L-1"))], later)
		.await
		.unwrap();
	assert_eq!(got.written, 2, "{got:?}");
	let by = Actor(uuid::Uuid::now_v7());
	let l1 = LeadId::parse("L-1").unwrap();
	panel.close_booking_once(by, &brand, &l1, "done", later + SignedDuration::from_mins(1), None).await.unwrap();

	let queued = outbox(&panel).await;
	let names: Vec<(&str, &str)> = queued.iter().map(|(e, d, _)| (e.as_str(), d.as_str())).collect();
	assert_eq!(
		names,
		[("sa_lead_created", visit), ("sa_booking_created", visit), ("sa_booking_closed", visit)],
		"the unmatched booking is journaled, not told"
	);
	let told = queued[1].2.as_object().unwrap();
	assert_eq!(
		told.keys().filter(|k| *k != "location_id").map(String::as_str).collect::<Vec<_>>(),
		["brand_id", "lead_time_hours", "manual", "match", "provider"],
		"no ref, revision, slot or attendee"
	);
	assert_eq!((&told["lead_time_hours"], &told["match"], &told["manual"]), (&json!(48), &json!("ref"), &json!(false)));
	let dump = format!("{queued:?}");
	for home in ["evt-ref-secret", "etag-rev-7", "Jean", "jean@", "+337"] {
		assert!(!dump.contains(home), "{home} reached the outbox: {dump}");
	}
	assert_eq!(queued[2].2["closed"], "done");

	panel.rebuild_projections().await.unwrap();
	assert_eq!(outbox(&panel).await, queued, "the rebuild tells PostHog nothing of bookings either");
}

#[tokio::test]
async fn sent_retried_split_and_given_up() {
	let db = TestDb::create().await;
	let panel = panel(&db).await.with_capture(true);
	let secret = site(&panel).await;
	let now = Timestamp::now();
	let events: Vec<Value> = (0..3).map(|i| created(&format!("L-{i}"), now - SignedDuration::from_mins(i), None)).collect();
	panel.ingest(sign("aquafix-site", &secret, &events, now).batch(), now).await.unwrap();

	let fake = Fake::default();
	fake.answers.lock().unwrap().push(Err(SendError::Retry("503".into())));
	assert_eq!(panel.capture_pass(&fake, now).await.unwrap(), Delivered { sent: 0, retried: 3, dropped: 0 });
	assert_eq!(panel.capture_pass(&fake, now).await.unwrap(), Delivered::default(), "not due again yet");
	let later = now + SignedDuration::from_secs(11);

	// One bad row in the batch: the batch is refused, then each alone; the bad one dropped.
	*fake.answers.lock().unwrap() = vec![Err(SendError::Refused("400".into())), Ok(()), Err(SendError::Refused("400".into())), Ok(())];
	assert_eq!(panel.capture_pass(&fake, later).await.unwrap(), Delivered { sent: 2, retried: 0, dropped: 1 });
	assert_eq!(panel.capture_backlog().await.unwrap(), 0);
	let sent = fake.sent.lock().unwrap().clone();
	assert_eq!(sent.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 1, 1, 1]);
	assert_eq!(sent[0][0].event, "sa_lead_created");
	assert!(sent[0].iter().any(|c| c.distinct_id == "sa-lead:aquafix:L-0" && c.timestamp == events_time(&events, 0)));

	// A week of failures: given up.
	panel.ingest(sign("aquafix-site", &secret, &[created("L-9", now, None)], now).batch(), now).await.unwrap();
	fake.answers.lock().unwrap().push(Err(SendError::Retry("timeout".into())));
	assert_eq!(panel.capture_pass(&fake, now + GIVE_UP_AFTER).await.unwrap(), Delivered { sent: 0, retried: 0, dropped: 1 });
}

/// When event `i` happened, as the journal keeps it (microseconds, cut).
fn events_time(events: &[Value], i: usize) -> Timestamp {
	events[i]["occurredAt"]
		.as_str()
		.unwrap()
		.parse::<Timestamp>()
		.unwrap()
		.round(jiff::TimestampRound::new().smallest(jiff::Unit::Microsecond).mode(jiff::RoundMode::Trunc))
		.unwrap()
}

/// A conversation is told to PostHog once, at its first message, where its lead is: a later
/// message, or one the operator marks, is not told again.
#[tokio::test]
async fn a_conversation_is_told_once() {
	let db = TestDb::create().await;
	let panel = panel(&db).await.with_capture(true);
	let secret = site(&panel).await;
	let bot = panel
		.add_source("aquafix-tg", SourceKind::Bot, [BrandId::parse("aquafix").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let now = Timestamp::now();
	let at = |m| now - SignedDuration::from_mins(m);
	let landing = [panel::testing::messenger_lead(at(10), "aquafix", "L-1", "telegram", "AQ-7K3F")];
	panel.ingest(sign("aquafix-site", &secret, &landing, now).batch(), now).await.unwrap();
	let wrote = |m| {
		event(
			"lead.messaged",
			at(m),
			"bot",
			json!({"brandId": "aquafix"}),
			json!({"channel": "telegram", "messageRef": "AQ-7K3F"}),
		)
	};
	let got = panel.ingest(sign("aquafix-tg", &bot, &[wrote(5), wrote(4)], now).batch(), now).await.unwrap();
	assert!(got.iter().all(|v| v.outcome == panel::Outcome::Accepted { unregistered: false }), "{got:?}");
	let messaged: Vec<(String, Value)> = outbox(&panel).await.into_iter().filter(|(e, ..)| e == "sa_lead_messaged").map(|(e, _, p)| (e, p)).collect();
	assert_eq!(messaged.len(), 1, "{messaged:?}");
	assert_eq!(messaged[0].1["location_id"], "royat", "the lead's place, found with it by its ref");
	assert_eq!(messaged[0].1["channel"], "telegram");
	assert!(!messaged[0].1.to_string().contains("AQ-7K3F"), "never the ref");
}

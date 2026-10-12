//! What an operator does, against a real Postgres: every action is an event in the journal,
//! the projections follow, and a rebuild lands on the same state.

use jiff::{SignedDuration, Timestamp, civil::date};
use panel::{
	operator::{ActionError, Actor, CallOutcome, FunnelBy, LeadQuery, NewLead, Payment, Pii, StageMove},
	testing::{TestDb, event, panel, sign},
};
use panel_core::{
	event::SourceKind,
	ids::{BrandId, JobId, LeadId, LocationId},
	lead::Stage,
};
use serde_json::json;
use uuid::Uuid;

fn now() -> Timestamp {
	"2026-09-30T18:00:00Z".parse().unwrap()
}

fn brand() -> BrandId {
	BrandId::parse("aquafix").unwrap()
}

#[tokio::test]
async fn actions_are_events_and_move_the_projection() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let by = Actor(Uuid::now_v7());
	let t = |mins| now() + SignedDuration::from_mins(mins);

	let (lead, created) = panel
		.create_lead(
			by,
			NewLead {
				brand: brand(),
				location: LocationId::parse("paris-11").unwrap(),
				need: "  a leaking tap ".into(),
				phone: Some("+33 6 00 00 00 00".into()),
				channel: panel_core::fact::LeadChannel::PhoneInbound,
			},
			t(0),
		)
		.await
		.unwrap();
	assert!(lead.as_str().starts_with("p-"));

	let (view, events) = panel.lead_card(&brand(), &lead, Pii::Reveal, t(0)).await.unwrap().unwrap();
	assert_eq!(view.row.stage, Stage::Created);
	assert_eq!(view.row.channel.as_deref(), Some("phone_inbound"));
	assert!(view.row.manual);
	assert_eq!(view.pii.unwrap(), json!({"need": "a leaking tap", "phone": "+33 6 00 00 00 00"}));
	assert_eq!(events.len(), 1);
	assert_eq!(events[0].row.id, created.raw());
	assert_eq!(events[0].row.source_kind, "panel");
	assert_eq!(events[0].row.source_id, by.0.to_string(), "who typed it in");
	assert_eq!(events[0].row.properties["enteredBy"], json!(by.0.to_string()));
	let (withheld, events) = panel.lead_card(&brand(), &lead, Pii::Withhold, t(0)).await.unwrap().unwrap();
	assert!(withheld.pii.is_none() && events[0].pii.is_none(), "the PII check point holds");

	let attempt = panel.attempt_call(by, &brand(), &lead, t(1)).await.unwrap();
	let call = |attempt| CallOutcome {
		attempt,
		outcome: "answered".into(),
	};
	panel.log_call(by, &brand(), &lead, call(attempt.raw()), t(2)).await.unwrap();
	assert!(matches!(panel.log_call(by, &brand(), &lead, call(Uuid::now_v7()), t(2)).await, Err(ActionError::NotFound)));
	let bad = CallOutcome {
		attempt: attempt.raw(),
		outcome: "voicemail".into(),
	};
	assert!(matches!(panel.log_call(by, &brand(), &lead, bad, t(2)).await, Err(ActionError::Invalid(_))));

	panel.move_lead(by, &brand(), &lead, StageMove::Contacted { channel: Some("phone".into()) }, t(3)).await.unwrap();
	assert!(
		matches!(panel.move_lead(by, &brand(), &lead, StageMove::Completed, t(4)).await, Err(ActionError::Conflict(_))),
		"nothing won yet"
	);
	panel
		.move_lead(
			by,
			&brand(),
			&lead,
			StageMove::Quoted {
				amount: Some(12_000),
				currency: Some("EUR".into()),
			},
			t(5),
		)
		.await
		.unwrap();
	panel.move_lead(by, &brand(), &lead, StageMove::Won { job_id: None }, t(6)).await.unwrap();
	panel.move_lead(by, &brand(), &lead, StageMove::Completed, t(7)).await.unwrap();
	let payment = Payment {
		billed: 12_000,
		commission: 1_800,
		currency: "EUR".into(),
	};
	panel.record_payment(by, &brand(), &lead, payment, t(8)).await.unwrap();
	let too_much = Payment {
		billed: 1,
		commission: 2,
		currency: "EUR".into(),
	};
	assert!(matches!(panel.record_payment(by, &brand(), &lead, too_much, t(8)).await, Err(ActionError::Invalid(_))));

	let (view, events) = panel.lead_card(&brand(), &lead, Pii::Reveal, t(9)).await.unwrap().unwrap();
	assert_eq!(view.row.stage, Stage::Paid);
	assert!(view.row.job_id.as_deref().unwrap().starts_with("j-"));
	assert_eq!(events.len(), 8);

	let pool = db.pool().await;
	let keyless: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE key_id IS NULL AND source_kind = 'panel'")
		.fetch_one(&pool)
		.await
		.unwrap();
	assert_eq!(keyless, 8, "the panel's own events carry no signing key");
	let calls: i64 = sqlx::query_scalar("SELECT count(*) FROM calls").fetch_one(&pool).await.unwrap();
	assert_eq!(calls, 2);

	let before = panel.lead_card(&brand(), &lead, Pii::Reveal, t(9)).await.unwrap().unwrap().0.row;
	let rebuilt = panel.rebuild_projections().await.unwrap();
	assert_eq!((rebuilt.registered, rebuilt.leads), (8, 1));
	let after = panel.lead_card(&brand(), &lead, Pii::Reveal, t(9)).await.unwrap().unwrap().0.row;
	assert_eq!(format!("{before:?}"), format!("{after:?}"), "the rebuild lands on the same state");

	let lost = StageMove::Lost {
		reason: "Too expensive".into(),
		note: None,
	};
	assert!(
		matches!(panel.move_lead(by, &brand(), &lead, lost, t(10)).await, Err(ActionError::Invalid(_))),
		"a reason is a slug"
	);
	let nobody = LeadId::parse("L-404").unwrap();
	assert!(matches!(panel.attempt_call(by, &brand(), &nobody, t(10)).await, Err(ActionError::NotFound)));
	let won_elsewhere = StageMove::Won {
		job_id: Some(JobId::parse("J-7").unwrap()),
	};
	assert!(matches!(panel.move_lead(by, &brand(), &nobody, won_elsewhere, t(10)).await, Err(ActionError::NotFound)));
}

#[tokio::test]
async fn lists_filters_pages_and_the_sla() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let site = panel.add_source("aquafix-site", SourceKind::Site, [brand()].into()).await.unwrap().unwrap();
	let at = |mins: i64| now() + SignedDuration::from_mins(mins);
	let lead = |id: &str| json!({"brandId": "aquafix", "locationId": "paris-11", "leadId": id});
	let mut events = Vec::new();
	for (i, id) in ["L-1", "L-2", "L-3"].iter().enumerate() {
		let mut e = event("lead.created", at(i as i64 * 20), "site", lead(id), json!({"channel": "form"}));
		e["pii"] = json!({"name": format!("Customer {id}")});
		events.push(e);
	}
	let got = panel.ingest(sign("aquafix-site", &site.secret, &events, at(40)).batch(), at(40)).await.unwrap();
	assert_eq!(got.len(), 3);
	let by = Actor(Uuid::now_v7());
	panel
		.move_lead(by, &brand(), &LeadId::parse("L-2").unwrap(), StageMove::Contacted { channel: None }, at(41))
		.await
		.unwrap();

	let q = |overdue, limit| LeadQuery {
		overdue,
		limit,
		..LeadQuery::default()
	};
	let all = panel.leads(&q(false, 50), Pii::Reveal, at(45)).await.unwrap();
	let ids: Vec<&str> = all.leads.iter().map(|l| l.row.lead_id.as_str()).collect();
	assert_eq!(ids, ["L-3", "L-2", "L-1"], "newest first");
	assert_eq!(all.leads[0].pii.as_ref().unwrap()["name"], "Customer L-3");
	assert!(all.next.is_none());
	let l1 = &all.leads[2];
	assert!(l1.waiting.unwrap().overdue, "45 minutes without contact");
	assert!(!all.leads[0].waiting.unwrap().overdue, "5 minutes");
	assert!(all.leads[1].waiting.is_none(), "contacted");

	let overdue = panel.leads(&q(true, 50), Pii::Withhold, at(45)).await.unwrap();
	assert_eq!(overdue.leads.iter().map(|l| l.row.lead_id.as_str()).collect::<Vec<_>>(), ["L-1"]);
	assert!(overdue.leads[0].pii.is_none());

	let first = panel.leads(&q(false, 2), Pii::Reveal, at(45)).await.unwrap();
	assert_eq!(first.leads.len(), 2);
	let second = LeadQuery {
		after: first.next.clone(),
		..q(false, 2)
	};
	let second = panel.leads(&second, Pii::Reveal, at(45)).await.unwrap();
	assert_eq!(second.leads.iter().map(|l| l.row.lead_id.as_str()).collect::<Vec<_>>(), ["L-1"]);
	assert!(second.next.is_none());

	let contacted = LeadQuery {
		stage: Some(Stage::Contacted),
		..q(false, 50)
	};
	assert_eq!(panel.leads(&contacted, Pii::Reveal, at(45)).await.unwrap().leads.len(), 1);
	let other_brand = LeadQuery {
		brand: Some(BrandId::parse("vifnet").unwrap()),
		..q(false, 50)
	};
	assert!(panel.leads(&other_brand, Pii::Reveal, at(45)).await.unwrap().leads.is_empty());

	let totals = panel.funnel(date(2026, 9, 30), date(2026, 9, 30), None).await.unwrap();
	assert_eq!((totals.leads, totals.contacted, totals.manual), (3, 1, 0));
	let none = panel.funnel(date(2026, 9, 1), date(2026, 9, 29), Some(&brand())).await.unwrap();
	assert_eq!(none.leads, 0);
}

#[tokio::test]
async fn slices_places_payments_and_counts() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let site = panel.add_source("aquafix-site", SourceKind::Site, [brand()].into()).await.unwrap().unwrap();
	let at = |mins: i64| now() + SignedDuration::from_mins(mins);
	let yesterday = now() - SignedDuration::from_hours(24);
	let subject = |id: &str, location: Option<&str>| {
		let mut s = json!({"brandId": "aquafix", "leadId": id});
		if let Some(l) = location {
			s["locationId"] = json!(l);
		}
		s
	};
	let form = json!({"channel": "form"});
	let events = [
		event("lead.created", yesterday, "site", subject("L-0", Some("paris-11")), form.clone()),
		event("lead.created", at(0), "site", subject("L-1", Some("paris-11")), form.clone()),
		event("lead.created", at(1), "site", subject("L-2", Some("lyon-2")), form.clone()),
		event("lead.created", at(2), "site", subject("L-3", None), form.clone()),
	];
	panel.ingest(sign("aquafix-site", &site.secret, &events, at(3)).batch(), at(3)).await.unwrap();
	let by = Actor(Uuid::now_v7());
	let vifnet = BrandId::parse("vifnet").unwrap();
	let new = NewLead {
		brand: vifnet.clone(),
		location: LocationId::parse("apex").unwrap(),
		need: "a boiler".into(),
		phone: None,
		channel: panel_core::fact::LeadChannel::PhoneInbound,
	};
	panel.create_lead(by, new, at(4)).await.unwrap();
	let lead = |id: &str| LeadId::parse(id).unwrap();
	let pay = |billed, commission, currency: &str| Payment {
		billed,
		commission,
		currency: currency.into(),
	};
	panel.record_payment(by, &brand(), &lead("L-1"), pay(12_000, 1_800, "EUR"), at(5)).await.unwrap();
	panel.record_payment(by, &brand(), &lead("L-1"), pay(3_000, 450, "EUR"), at(6)).await.unwrap();
	panel.record_payment(by, &brand(), &lead("L-2"), pay(5_000, 500, "USD"), at(7)).await.unwrap();
	panel.record_payment(by, &brand(), &lead("L-0"), pay(9_999, 1, "EUR"), at(8)).await.unwrap();
	panel.move_lead(by, &brand(), &lead("L-3"), StageMove::Contacted { channel: None }, at(9)).await.unwrap();

	let today = date(2026, 9, 30);
	let all = panel.funnel_slices(today, today, None, FunnelBy::All).await.unwrap();
	assert_eq!(all.len(), 1);
	assert_eq!((all[0].brand.as_deref(), all[0].location.as_deref()), (None, None));
	assert_eq!((all[0].totals.leads, all[0].totals.paid, all[0].totals.contacted), (4, 2, 1));
	let sums: Vec<(&str, i64, i64, u64)> = all[0].payments.iter().map(|p| (p.currency.as_str(), p.billed, p.commission, p.payments)).collect();
	assert_eq!(sums, [("EUR", 15_000, 2_250, 2), ("USD", 5_000, 500, 1)], "yesterday's lead is outside, whenever it was paid");

	let slices = panel.funnel_slices(today, today, None, FunnelBy::Location).await.unwrap();
	let keys: Vec<(Option<&str>, Option<&str>, u64, usize)> = slices.iter().map(|s| (s.brand.as_deref(), s.location.as_deref(), s.totals.leads, s.payments.len())).collect();
	assert_eq!(
		keys,
		[
			(Some("aquafix"), Some("lyon-2"), 1, 1),
			(Some("aquafix"), Some("paris-11"), 1, 1),
			(Some("aquafix"), None, 1, 0),
			(Some("vifnet"), Some("apex"), 1, 0)
		]
	);
	assert_eq!(slices[1].payments[0].billed, 15_000);
	let two_days = panel.funnel_slices(date(2026, 9, 29), today, Some(&brand()), FunnelBy::Location).await.unwrap();
	let paris = two_days.iter().find(|s| s.location.as_deref() == Some("paris-11")).unwrap();
	assert_eq!((paris.totals.leads, paris.payments[0].billed, paris.payments[0].payments), (2, 24_999, 3));

	let empty = date(2026, 9, 1);
	let none = panel.funnel_slices(empty, empty, None, FunnelBy::All).await.unwrap();
	assert_eq!((none.len(), none[0].totals.leads, none[0].payments.len()), (1, 0, 0), "zeros, not nothing");
	assert!(panel.funnel_slices(empty, empty, None, FunnelBy::Location).await.unwrap().is_empty());
	let only_vifnet = panel.funnel_slices(today, today, Some(&vifnet), FunnelBy::All).await.unwrap();
	assert_eq!((only_vifnet[0].totals.leads, only_vifnet[0].payments.len()), (1, 0));

	let places = panel.places().await.unwrap();
	let got: Vec<(&str, &str, Option<Timestamp>)> = places.iter().map(|p| (p.brand_id.as_str(), p.location_id.as_str(), p.last_lead_at)).collect();
	assert_eq!(
		got,
		[("aquafix", "lyon-2", Some(at(1))), ("aquafix", "paris-11", Some(at(0))), ("vifnet", "apex", Some(at(4)))],
		"no place for a lead that names none"
	);

	let counts = panel.lead_counts(None, None, at(45)).await.unwrap();
	let n = |stage| counts.stages.iter().find(|(s, _)| *s == stage).unwrap().1;
	assert_eq!(counts.stages.len(), Stage::ALL.len(), "every stage, zero included");
	assert_eq!((n(Stage::Created), n(Stage::Contacted), n(Stage::Paid), n(Stage::Lost)), (1, 1, 3, 0));
	assert_eq!(counts.overdue, 1, "vifnet's, 41 minutes without contact");
	let aquafix = panel.lead_counts(Some(&brand()), None, at(45)).await.unwrap();
	assert_eq!((aquafix.stages.iter().map(|(_, n)| n).sum::<u64>(), aquafix.overdue), (4, 0));
	let paris = panel.lead_counts(Some(&brand()), Some(&LocationId::parse("paris-11").unwrap()), at(45)).await.unwrap();
	assert_eq!(paris.stages.iter().map(|(_, n)| n).sum::<u64>(), 2);

	let window = LeadQuery {
		created_from: Some(at(1)),
		created_before: Some(at(4)),
		limit: 50,
		..LeadQuery::default()
	};
	let ids: Vec<String> = panel.leads(&window, Pii::Withhold, at(45)).await.unwrap().leads.into_iter().map(|l| l.row.lead_id).collect();
	assert_eq!(ids, ["L-3", "L-2"], "from included, before excluded");
}

/// A customer who wrote on a messenger without the landing is taken in by hand under that
/// messenger's channel; an operator says a customer wrote; the list is filtered on both.
#[tokio::test]
async fn messenger_leads_by_hand() {
	use panel::testing::messenger_lead;
	use panel_core::fact::{LeadChannel, MessageRef, Messenger};

	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let by = Actor(Uuid::now_v7());
	let t = |mins| now() + SignedDuration::from_mins(mins);
	let new = |channel| NewLead {
		brand: brand(),
		location: LocationId::parse("royat").unwrap(),
		need: "a leak".into(),
		phone: None,
		channel,
	};
	let (wrote, _) = panel.create_lead(by, new(LeadChannel::Whatsapp), t(0)).await.unwrap();
	for not_by_hand in [LeadChannel::Form, LeadChannel::Callback] {
		let refused = panel.create_lead(by, new(not_by_hand), t(0)).await;
		assert!(
			matches!(&refused, Err(ActionError::Invalid(e)) if e.0 == "channel is one of phone_inbound, whatsapp, telegram"),
			"{not_by_hand:?}: {refused:?}"
		);
	}
	let site = panel.add_source("aquafix-site", SourceKind::Site, [brand()].into()).await.unwrap().unwrap();
	let landing = [messenger_lead(t(1), "aquafix", "L-1", "telegram", "AQ-7K3F")];
	panel.ingest(sign("aquafix-site", &site.secret, &landing, t(2)).batch(), t(2)).await.unwrap();

	let l1 = LeadId::parse("L-1").unwrap();
	let first = panel.mark_messaged_once(by, &brand(), &l1, Messenger::Telegram, t(3), Some("k1")).await.unwrap();
	let again = panel.mark_messaged_once(by, &brand(), &l1, Messenger::Telegram, t(4), Some("k1")).await.unwrap();
	assert!(!first.replayed && again.replayed && again.value == first.value, "a retry is the first");
	let twice = panel.mark_messaged_once(by, &brand(), &l1, Messenger::Whatsapp, t(4), None).await.unwrap();
	assert!(twice.replayed && twice.value == first.value, "said once is enough: the first message is the answer");
	let pool = db.pool().await;
	let messages: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE type = 'lead.messaged'").fetch_one(&pool).await.unwrap();
	assert_eq!(messages, 1);
	let nobody = panel.mark_messaged_once(by, &brand(), &LeadId::parse("L-404").unwrap(), Messenger::Whatsapp, t(3), None).await;
	assert!(matches!(nobody, Err(ActionError::NotFound)));

	let listed = |channel, message_ref| {
		let panel = panel.clone();
		async move {
			let q = LeadQuery {
				channel,
				message_ref,
				limit: 50,
				..LeadQuery::default()
			};
			panel.leads(&q, Pii::Withhold, t(5)).await.unwrap().leads
		}
	};
	let whatsapp = listed(Some(LeadChannel::Whatsapp), None).await;
	assert_eq!(whatsapp.iter().map(|l| l.row.lead_id.as_str()).collect::<Vec<_>>(), [wrote.as_str()]);
	assert!(whatsapp[0].row.manual && whatsapp[0].row.messaged.is_none(), "taken by hand, not yet said to have written");
	let by_ref = listed(None, Some(MessageRef::parse("AQ-7K3F").unwrap())).await;
	assert_eq!(by_ref.len(), 1);
	let row = &by_ref[0].row;
	assert_eq!(
		(row.lead_id.as_str(), row.channel.as_deref(), row.message_ref.as_deref()),
		("L-1", Some("telegram"), Some("AQ-7K3F"))
	);
	assert_eq!(row.messaged, Some((t(3), Messenger::Telegram)));
	assert_eq!(row.stage, Stage::Created, "writing is not being contacted");
	assert!(listed(None, Some(MessageRef::parse("AQ-0000").unwrap())).await.is_empty());
	assert!(listed(Some(LeadChannel::Form), None).await.is_empty());
}

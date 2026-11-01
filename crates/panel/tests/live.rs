//! The bus the live sockets listen on: what each write publishes once it has committed, and
//! what publishes nothing. Publishing is synchronous with the write's return, so `try_recv`
//! tells "nothing was published" without waiting.

use jiff::Timestamp;
use panel::{
	live::{Change, Signal, Topic},
	operator::{Actor, NewLead, StageMove},
	place::Expected,
	testing::{TestDb, event, panel, sign},
};
use panel_core::{
	ids::{BrandId, LeadId, LocationId},
	place::{Editor, PlaceSettings},
};
use sa_auth::SourceKind;
use serde_json::json;
use tokio::sync::broadcast::{Receiver, error::TryRecvError};
use uuid::Uuid;

fn aquafix() -> BrandId {
	BrandId::parse("aquafix").unwrap()
}

/// The next change, with its time checked and dropped.
fn next(rx: &mut Receiver<Signal>) -> (Topic, Option<String>, Option<String>) {
	match rx.try_recv() {
		Ok(Signal::Changed(Change { topic, brand, id, user: None, at })) => {
			assert!(at <= Timestamp::now());
			(topic, brand.map(|b| b.as_str().to_owned()), id)
		}
		other => panic!("expected a change, got {other:?}"),
	}
}

fn quiet(rx: &mut Receiver<Signal>) {
	assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)), "nothing more published");
}

fn some(s: &str) -> Option<String> {
	Some(s.to_owned())
}

#[tokio::test]
async fn journaled_events_publish_after_the_commit_and_only_then() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let mut rx = panel.bus().subscribe();
	let secret = panel.add_source("aquafix-site", SourceKind::Site, [aquafix()].into()).await.unwrap().unwrap().secret.to_string();
	assert_eq!(next(&mut rx), (Topic::Sources, None, None));

	let now = Timestamp::now();
	let lead = json!({"brandId": "aquafix", "locationId": "royat", "leadId": "L-1"});
	let created = event("lead.created", now, "site", lead.clone(), json!({"channel": "form"}));
	panel.ingest(sign("aquafix-site", &secret, std::slice::from_ref(&created), now).batch(), now).await.unwrap();
	assert_eq!(next(&mut rx), (Topic::Leads, some("aquafix"), some("L-1")));

	// A resend, an unregistered type, a refused event: no read changed.
	let unregistered = event("review.new", now, "site", lead, json!({}));
	let refused = event("lead.created", now, "site", json!({"brandId": "vifnet", "leadId": "V-1"}), json!({"channel": "form"}));
	panel.ingest(sign("aquafix-site", &secret, &[created, unregistered, refused], now).batch(), now).await.unwrap();
	quiet(&mut rx);

	// What an operator does goes through the same journal.
	let by = Actor(Uuid::now_v7());
	let l1 = LeadId::parse("L-1").unwrap();
	panel.move_lead(by, &aquafix(), &l1, StageMove::Contacted { channel: None }, now).await.unwrap();
	assert_eq!(next(&mut rx), (Topic::Lead, some("aquafix"), some("L-1")));
	let new = NewLead {
		brand: aquafix(),
		location: LocationId::parse("royat").unwrap(),
		need: "a leak".into(),
		phone: None,
		channel: panel_core::fact::LeadChannel::PhoneInbound,
	};
	let (made, _) = panel.create_lead(by, new, now).await.unwrap();
	assert_eq!(next(&mut rx), (Topic::Leads, some("aquafix"), Some(made.as_str().to_owned())));
	quiet(&mut rx);

	panel.rebuild_projections().await.unwrap();
	assert_eq!(rx.try_recv().unwrap(), Signal::Resync, "everything may have changed");

	assert!(panel.revoke_source("aquafix-site").await.unwrap());
	assert_eq!(next(&mut rx), (Topic::Sources, None, None));
	assert!(!panel.revoke_source("aquafix-site").await.unwrap());
	quiet(&mut rx);
}

#[tokio::test]
async fn a_place_publishes_when_it_changes() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let mut rx = panel.bus().subscribe();
	let (brand, slug) = (aquafix(), LocationId::parse("royat").unwrap());
	let now = Timestamp::now();

	panel.register_place(&Editor::Cli, &brand, &slug, now).await.unwrap();
	assert_eq!(next(&mut rx), (Topic::Places, some("aquafix"), some("royat")));
	panel.register_place(&Editor::Cli, &brand, &slug, now).await.unwrap();
	quiet(&mut rx);

	let phone = PlaceSettings::parse(&json!({"phone": "+33423500640"})).unwrap();
	panel.set_place(&Editor::Cli, &brand, &slug, phone.clone(), Expected::Any, now).await.unwrap();
	assert_eq!(next(&mut rx), (Topic::Places, some("aquafix"), some("royat")));
	panel.set_place(&Editor::Cli, &brand, &slug, phone, Expected::Any, now).await.unwrap();
	quiet(&mut rx);

	panel.withdraw_place(&Editor::Cli, &brand, &slug, true, now).await.unwrap();
	assert_eq!(next(&mut rx), (Topic::Places, some("aquafix"), some("royat")));

	// A conflict writes nothing, and says nothing.
	let stale = Expected::At(Some("2020-01-01T00:00:00Z".parse().unwrap()));
	assert!(panel.set_place(&Editor::Cli, &brand, &slug, PlaceSettings::default(), stale, now).await.is_err());
	quiet(&mut rx);
}

//! Booking in the engine, on throwaway SQLite files: the journal's migration to the source
//! kind `booking` keeps every row, kitstart's `booking.requested` fixtures judged by the
//! registry, and `booking.requested` through a signed ingest — deferred while its lead has
//! not arrived, refused for a wish out of the window.

use std::{fs, path::PathBuf};

use jiff::{SignedDuration, Timestamp};
use panel::{
	Outcome,
	operator::Pii,
	testing::{TestDb, event, panel, sign},
	wire::{Checked, check},
};
use panel_core::{
	booking::BookingStatus,
	event::{Subject, TypeKey},
	ids::{BrandId, LeadId},
};
use sa_auth::SourceKind;
use serde_json::{Value, json};
use sqlx::{migrate::Migrator, sqlite::SqlitePoolOptions};

/// The migration that widens the journal's source kinds.
const BOOKING_KIND: i64 = 20261005090000;

const ACCEPTED: Outcome = Outcome::Accepted { unregistered: false };

const LEAD: &str = "lead-42-0a1b2c3d";

fn fixtures(sub: &str) -> Vec<(String, Value)> {
	let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../panel_core/tests/fixtures/booking").join(sub);
	let mut out: Vec<(String, Value)> = fs::read_dir(&dir)
		.unwrap()
		.map(|e| e.unwrap().path())
		.filter(|p| p.extension().is_some_and(|e| e == "json"))
		.map(|p| {
			(
				p.file_name().unwrap().to_string_lossy().into_owned(),
				serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap(),
			)
		})
		.collect();
	out.sort_by(|a, b| a.0.cmp(&b.0));
	out
}

fn subject(lead: &str) -> Subject {
	Subject {
		brand_id: BrandId::parse("vifnet").unwrap(),
		location_id: None,
		lead_id: Some(LeadId::parse(lead).unwrap()),
		job_id: None,
	}
}

#[test]
fn kitstarts_requested_fixtures() {
	let key = TypeKey::parse("booking.requested", 1).unwrap();
	let valid = fixtures("requested/valid");
	assert_eq!(valid.len(), 7, "the vendored set");
	for (name, props) in valid {
		let got = check(&key, SourceKind::Site, &props, &subject(LEAD));
		assert!(matches!(got, Checked::Registered(_)), "{name}: {got:?}");
	}
	let invalid = fixtures("requested/invalid");
	assert_eq!(invalid.len(), 15, "the vendored set");
	for (name, props) in invalid {
		let got = check(&key, SourceKind::Site, &props, &subject(LEAD));
		assert!(matches!(got, Checked::Invalid(_)), "{name}: {got:?}");
	}
	let other_lead = check(&key, SourceKind::Site, &json!({"lead_ref": LEAD, "provider": "manual"}), &subject("lead-43-0a1b2c3d"));
	assert!(matches!(other_lead, Checked::Invalid(_)), "the ref is the subject's lead");
	let from_a_panel = check(&key, SourceKind::Panel, &json!({"lead_ref": LEAD, "provider": "manual"}), &subject(LEAD));
	assert!(matches!(from_a_panel, Checked::Invalid(_)), "a site's alone");
}

#[tokio::test]
async fn a_requested_booking_waits_for_its_lead() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = panel
		.add_source("vifnet-site", SourceKind::Site, [BrandId::parse("vifnet").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let now = Timestamp::now();
	let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
	let subject = json!({"brandId": "vifnet", "locationId": "vifnet", "leadId": LEAD});
	let wish = |date: jiff::civil::Date| {
		event(
			"booking.requested",
			now,
			"site",
			subject.clone(),
			json!({"lead_ref": LEAD, "provider": "manual", "preferred_date": date.to_string(), "preferred_part": "morning"}),
		)
	};
	let requested = wish(today.tomorrow().unwrap());
	let first = panel.ingest(sign("vifnet-site", &secret, std::slice::from_ref(&requested), now).batch(), now).await.unwrap();
	assert!(matches!(first[0].outcome, Outcome::Deferred(_)), "{first:?}: the lead has not arrived");

	let created = event(
		"lead.created",
		now - SignedDuration::from_mins(1),
		"site",
		subject.clone(),
		json!({"channel": "form", "flow": "quote"}),
	);
	let both = panel.ingest(sign("vifnet-site", &secret, &[created, requested], now).batch(), now).await.unwrap();
	assert_eq!(both.iter().map(|v| v.outcome.clone()).collect::<Vec<_>>(), [ACCEPTED, ACCEPTED]);

	let (lead, _) = panel
		.lead_card(&BrandId::parse("vifnet").unwrap(), &LeadId::parse(LEAD).unwrap(), Pii::Withhold, now)
		.await
		.unwrap()
		.unwrap();
	let b = lead.row.booking;
	assert_eq!((b.status, b.provider.map(|p| p.as_str())), (BookingStatus::Requested, Some("manual")));
	assert_eq!((b.preferred_date, b.preferred_part.map(|p| p.as_str())), (today.tomorrow().ok(), Some("morning")));

	let too_far = wish(today.checked_add(SignedDuration::from_hours(24 * 367)).unwrap());
	let refused = panel.ingest(sign("vifnet-site", &secret, &[too_far], now).batch(), now).await.unwrap();
	assert!(matches!(&refused[0].outcome, Outcome::Rejected(e) if e.0.contains("preferred_date")), "{refused:?}");
	let past = wish(today.checked_sub(SignedDuration::from_hours(24 * 3)).unwrap());
	assert!(matches!(
		panel.ingest(sign("vifnet-site", &secret, &[past], now).batch(), now).await.unwrap()[0].outcome,
		Outcome::Rejected(_)
	));
}

/// The journal is copied whole into its wider table: every row, byte for byte, the foreign
/// keys of the projections still resolving, the append-only triggers back — and it goes back
/// down while it holds no `booking` event, and refuses to while it does.
#[tokio::test]
async fn the_journal_keeps_every_row_through_its_new_source_kind() {
	let db = TestDb::create().await;
	let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
	let migrator = Migrator::new(migrations.as_path()).await.unwrap();
	let pool = SqlitePoolOptions::new().max_connections(1).connect_with(panel::store::options(db.path())).await.unwrap();
	migrator.run_to(BOOKING_KIND - 1, &pool).await.unwrap();

	// A journal of the build before, written as it would have: a source, two events (one
	// with sealed-looking PII), and the projections that reference them.
	let id = |n: u8| {
		let mut b = [0u8; 16];
		b[15] = n;
		b.to_vec()
	};
	let fp = vec![7u8; 32];
	sqlx::query("INSERT INTO sources (key_id, kind, brand_ids, secret_sealed, data_key_fp) VALUES ('vifnet-site', 'site', '[\"vifnet\"]', x'00', $1)")
		.bind(&fp)
		.execute(&pool)
		.await
		.unwrap();
	for (n, r#type, pii) in [(1u8, "lead.created", Some(vec![9u8; 40])), (2, "call.attempted", None)] {
		sqlx::query(
			"INSERT INTO events (id, schema, type, type_version, occurred_at, received_at, source_kind, source_id, key_id, brand_id, lead_id, \
			 properties, pii_sealed, data_key_fp, content_mac, status) \
			 VALUES ($1, 'sa.funnel.v1', $2, 1, $3, $3, 'site', 'vifnet-site', 'vifnet-site', 'vifnet', 'L-1', '{\"channel\":\"form\"}', $4, $5, $6, 'registered')",
		)
		.bind(id(n))
		.bind(r#type)
		.bind(1_000_000 * i64::from(n))
		.bind(pii.clone())
		.bind(pii.as_ref().map(|_| fp.clone()))
		.bind(vec![n; 32])
		.execute(&pool)
		.await
		.unwrap();
	}
	sqlx::query("INSERT INTO calls (event_id, brand_id, lead_id, kind, occurred_at, manual) VALUES ($1, 'vifnet', 'L-1', 'attempted', 2000000, 0)")
		.bind(id(2))
		.execute(&pool)
		.await
		.unwrap();
	sqlx::query("INSERT INTO leads (brand_id, lead_id, stage, manual, last_event_id, last_event_at) VALUES ('vifnet', 'L-1', 'created', 0, $1, 2000000)")
		.bind(id(2))
		.execute(&pool)
		.await
		.unwrap();
	type Dump = Vec<(Vec<u8>, String, String, Option<Vec<u8>>, Vec<u8>, i64)>;
	let dump = || async {
		let rows: Dump = sqlx::query_as("SELECT id, type, properties, pii_sealed, content_mac, manual FROM events ORDER BY id")
			.fetch_all(&pool)
			.await
			.unwrap();
		rows
	};
	let before = dump().await;

	migrator.run(&pool).await.unwrap();
	assert_eq!(dump().await, before, "every row, every byte");
	let dangling: Vec<(String,)> = sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check").fetch_all(&pool).await.unwrap();
	assert!(dangling.is_empty(), "{dangling:?}");
	let fks: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(&pool).await.unwrap();
	assert_eq!(fks, 1, "foreign keys are back on");
	assert!(sqlx::query("DELETE FROM events").execute(&pool).await.is_err(), "still append-only");
	assert!(sqlx::query("UPDATE events SET brand_id = 'x'").execute(&pool).await.is_err());
	assert!(sqlx::query("DELETE FROM leads").execute(&pool).await.is_ok(), "a projection is not");
	assert!(sqlx::query("DELETE FROM calls").execute(&pool).await.is_ok());
	let ingest_daily: i64 = sqlx::query_scalar("SELECT sum(events) FROM reporting_ingest_daily").fetch_one(&pool).await.unwrap();
	assert_eq!(ingest_daily, 2, "the view is back");

	// Down, and up again, while no booking event is there.
	migrator.undo(&pool, BOOKING_KIND - 1).await.unwrap();
	assert_eq!(dump().await, before);
	migrator.run(&pool).await.unwrap();
	sqlx::query(
		"INSERT INTO events (id, schema, type, type_version, occurred_at, received_at, source_kind, source_id, brand_id, properties, content_mac, status) \
		 VALUES ($1, 'sa.funnel.v1', 'booking.canceled', 1, 3000000, 3000000, 'booking', 'google_calendar', 'vifnet', '{}', $2, 'registered')",
	)
	.bind(id(3))
	.bind(vec![3u8; 32])
	.execute(&pool)
	.await
	.expect("the new kind is taken");
	assert!(migrator.undo(&pool, BOOKING_KIND - 1).await.is_err(), "a journal with a booking event does not go back");
	assert_eq!(dump().await.len(), 3, "and loses nothing trying");
}

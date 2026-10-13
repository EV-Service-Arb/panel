//! Ingest against a real Postgres: signatures, dedup, the registry, brand scoping,
//! projections and their rebuild. Every query of the store runs here at least once.

use jiff::{SignedDuration, Timestamp};
use panel::{
	IngestError, Outcome, Panel,
	testing::{TestDb, event, messenger_lead, panel, sign, sign_verbatim},
};
use panel_core::{event::SourceKind, ids::BrandId};
use serde_json::{Value, json};

fn now() -> Timestamp {
	"2026-09-30T18:00:00Z".parse().unwrap()
}

fn at(minutes: i64) -> Timestamp {
	"2026-09-30T09:00:00Z".parse::<Timestamp>().unwrap() + SignedDuration::from_mins(minutes)
}

fn brands(names: &[&str]) -> std::collections::BTreeSet<BrandId> {
	names.iter().map(|b| BrandId::parse(b).unwrap()).collect()
}

/// A panel with a site key for aquafix and a panel key for aquafix; their secrets.
async fn setup(db: &TestDb) -> (Panel, String, String) {
	let panel = panel(db).await;
	let site = panel.add_source("aquafix-site", SourceKind::Site, brands(&["aquafix"])).await.unwrap().unwrap();
	let ops = panel.add_source("aquafix-ops", SourceKind::Panel, brands(&["aquafix"])).await.unwrap().unwrap();
	(panel, site.secret.to_string(), ops.secret.to_string())
}

fn lead(id: &str) -> Value {
	json!({"brandId": "aquafix", "locationId": "paris-11", "leadId": id})
}

fn outcomes(v: &[panel::EventVerdict]) -> Vec<Outcome> {
	v.iter().map(|v| v.outcome.clone()).collect()
}

const ACCEPTED: Outcome = Outcome::Accepted { unregistered: false };

async fn count(pool: &sqlx::SqlitePool, sql: &'static str) -> i64 {
	sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn signatures() {
	let db = TestDb::create().await;
	let (panel, secret, ops) = setup(&db).await;
	let events = [event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}))];

	let good = sign("aquafix-site", &secret, &events, now());
	assert_eq!(outcomes(&panel.ingest(good.batch(), now()).await.unwrap()), [ACCEPTED]);

	let wrong_secret = sign("aquafix-site", &"0".repeat(64), &events, now());
	assert!(matches!(panel.ingest(wrong_secret.batch(), now()).await, Err(IngestError::Unauthorized("bad signature"))));

	let stale = sign("aquafix-site", &secret, &events, now() - SignedDuration::from_mins(6));
	assert!(matches!(panel.ingest(stale.batch(), now()).await, Err(IngestError::Unauthorized("stale or malformed timestamp"))));

	let mut tampered = sign("aquafix-site", &secret, &events, now());
	tampered.body.push(b' ');
	assert!(matches!(panel.ingest(tampered.batch(), now()).await, Err(IngestError::Unauthorized("bad signature"))));

	let unknown = sign("nobody", &secret, &events, now());
	assert!(matches!(panel.ingest(unknown.batch(), now()).await, Err(IngestError::Unauthorized("unknown key"))));

	assert!(panel.store().revoke_source("aquafix-site").await.unwrap());
	let revoked = sign("aquafix-site", &secret, &events, now());
	assert!(matches!(panel.ingest(revoked.batch(), now()).await, Err(IngestError::Unauthorized("unknown key"))));
	assert!(!panel.store().revoke_source("aquafix-site").await.unwrap(), "revoked once");

	let not_a_batch = sign("aquafix-ops", &ops, &[], now());
	assert!(matches!(panel.ingest(not_a_batch.batch(), now()).await, Err(IngestError::BadRequest(_))));
}

#[tokio::test]
async fn sources_are_listed_and_unique() {
	let db = TestDb::create().await;
	let (panel, _, _) = setup(&db).await;
	assert!(
		panel.add_source("aquafix-site", SourceKind::Site, brands(&["vifnet"])).await.unwrap().is_none(),
		"the key id is taken"
	);
	let listed = panel.store().sources().await.unwrap();
	assert_eq!(listed.iter().map(|s| s.grant.key_id.as_str()).collect::<Vec<_>>(), ["aquafix-ops", "aquafix-site"]);
	assert!(listed.iter().all(|s| s.revoked_at.is_none()));
	let pool = db.pool().await;
	let secrets: Vec<Vec<u8>> = sqlx::query_scalar("SELECT secret_sealed FROM sources").fetch_all(&pool).await.unwrap();
	assert!(secrets.iter().all(|s| s.len() == 24 + 64 + 16), "sealed: nonce, 64 hex characters, tag");
}

#[tokio::test]
async fn dedup_by_id() {
	let db = TestDb::create().await;
	let (panel, secret, _) = setup(&db).await;
	let e = event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}));

	let first = panel.ingest(sign("aquafix-site", &secret, &[e.clone(), e.clone()], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&first), [ACCEPTED, Outcome::Duplicate], "a repeat inside one batch too");
	let again = panel.ingest(sign("aquafix-site", &secret, std::slice::from_ref(&e), now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&again), [Outcome::Duplicate]);

	let mut changed = e.clone();
	changed["properties"]["channel"] = json!("phone_inbound");
	let conflict = panel.ingest(sign("aquafix-site", &secret, &[changed], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&conflict), [Outcome::Rejected(panel_core::Invalid::new("id already names a different event"))]);

	let pool = db.pool().await;
	assert_eq!(count(&pool, "SELECT count(*) FROM events").await, 1);
	assert_eq!(
		count(&pool, "SELECT count(*) FROM leads WHERE channel = 'form'").await,
		1,
		"the conflicting resend changed nothing"
	);
}

#[tokio::test]
async fn a_callback_request_is_a_lead_channel() {
	let db = TestDb::create().await;
	let (panel, secret, _) = setup(&db).await;
	let events = [
		event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "callback"})),
		event("lead.created", at(0), "site", lead("L-2"), json!({"channel": "smoke_signal"})),
	];
	let got = panel.ingest(sign("aquafix-site", &secret, &events, now()).batch(), now()).await.unwrap();
	assert_eq!(
		outcomes(&got),
		[
			ACCEPTED,
			Outcome::Rejected(panel_core::Invalid::new("properties.channel is not one of form, phone_inbound, callback, whatsapp, telegram"))
		]
	);
	let pool = db.pool().await;
	let channel = || async {
		sqlx::query_scalar::<_, Option<String>>("SELECT channel FROM reporting_leads WHERE lead_id = 'L-1'")
			.fetch_one(&pool)
			.await
			.unwrap()
	};
	assert_eq!(channel().await.as_deref(), Some("callback"));
	panel.rebuild_projections().await.unwrap();
	assert_eq!(channel().await.as_deref(), Some("callback"), "and the rebuild agrees");
}

/// The migration that let `callback` into `leads.channel` rebuilt the table: undone and
/// applied again, the rows and the views over them are still there.
#[tokio::test]
async fn the_callback_migration_keeps_the_leads_both_ways() {
	const CALLBACK: i64 = 20261003120000;
	let db = TestDb::create().await;
	let (panel, secret, _) = setup(&db).await;
	let events = [
		event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"})),
		event("lead.created", at(1), "site", lead("L-2"), json!({"channel": "callback"})),
	];
	panel.ingest(sign("aquafix-site", &secret, &events, now()).batch(), now()).await.unwrap();
	let pool = db.pool().await;
	let rows = || async {
		sqlx::query_as::<_, (String, Option<String>)>("SELECT lead_id, channel FROM reporting_leads ORDER BY lead_id")
			.fetch_all(&pool)
			.await
			.unwrap()
	};
	let funnel = "SELECT sum(leads) FROM reporting_funnel_daily";

	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, CALLBACK - 1).await.unwrap();
	assert_eq!(
		rows().await,
		[("L-1".to_owned(), Some("form".to_owned())), ("L-2".to_owned(), None)],
		"callback has no word before it"
	);
	assert_eq!(count(&pool, funnel).await, 2);
	let refused = sqlx::query("UPDATE leads SET channel = 'callback' WHERE lead_id = 'L-2'").execute(&pool).await;
	assert!(refused.is_err(), "the old CHECK is back");

	migrator.run(&pool).await.unwrap();
	assert_eq!(rows().await, [("L-1".to_owned(), Some("form".to_owned())), ("L-2".to_owned(), None)]);
	assert_eq!(count(&pool, funnel).await, 2);
	panel.rebuild_projections().await.unwrap();
	assert_eq!(rows().await[1], ("L-2".to_owned(), Some("callback".to_owned())), "the journal still has it");
}

/// A landing's antispam marks a lead it doubted but kept: either word is taken and kept on the
/// lead, any other is refused, and an unmarked lead stays unmarked.
#[tokio::test]
async fn a_suspect_lead_is_kept_and_marked() {
	let db = TestDb::create().await;
	let (panel, secret, _) = setup(&db).await;
	let events = [
		event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form", "suspect": "rate_limited"})),
		event("lead.created", at(1), "site", lead("L-2"), json!({"channel": "callback", "suspect": "too_fast"})),
		event("lead.created", at(2), "site", lead("L-3"), json!({"channel": "form", "suspect": "honeypot"})),
		event("lead.created", at(3), "site", lead("L-4"), json!({"channel": "form"})),
	];
	let got = panel.ingest(sign("aquafix-site", &secret, &events, now()).batch(), now()).await.unwrap();
	assert_eq!(
		outcomes(&got),
		[
			ACCEPTED,
			ACCEPTED,
			Outcome::Rejected(panel_core::Invalid::new("properties.suspect is not one of rate_limited, too_fast")),
			ACCEPTED
		]
	);
	let pool = db.pool().await;
	let marks = || async {
		sqlx::query_as::<_, (String, Option<String>)>("SELECT lead_id, suspect FROM reporting_leads ORDER BY lead_id")
			.fetch_all(&pool)
			.await
			.unwrap()
	};
	let want = [
		("L-1".to_owned(), Some("rate_limited".to_owned())),
		("L-2".to_owned(), Some("too_fast".to_owned())),
		("L-4".to_owned(), None),
	];
	assert_eq!(marks().await, want);
	assert_eq!(count(&pool, "SELECT sum(leads) FROM reporting_funnel_daily").await, 3, "a doubted lead is still a lead");
	assert_eq!(count(&pool, "SELECT sum(suspect) FROM reporting_funnel_daily").await, 2, "counted apart too");
	panel.rebuild_projections().await.unwrap();
	assert_eq!(marks().await, want, "and the rebuild agrees");
}

/// The suspect migration adds a column and remakes the views: undone, the column is gone and
/// the leads are not; applied again, the rebuild brings the marks back from the journal.
#[tokio::test]
async fn the_suspect_migration_keeps_the_leads_both_ways() {
	const SUSPECT: i64 = 20261003140000;
	let db = TestDb::create().await;
	let (panel, secret, _) = setup(&db).await;
	let events = [
		event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"})),
		event("lead.created", at(1), "site", lead("L-2"), json!({"channel": "form", "suspect": "too_fast"})),
	];
	panel.ingest(sign("aquafix-site", &secret, &events, now()).batch(), now()).await.unwrap();
	let pool = db.pool().await;
	let funnel = "SELECT sum(leads) FROM reporting_funnel_daily";
	let marks = || async {
		sqlx::query_as::<_, (String, Option<String>)>("SELECT lead_id, suspect FROM reporting_leads ORDER BY lead_id")
			.fetch_all(&pool)
			.await
	};

	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, SUSPECT - 1).await.unwrap();
	assert!(marks().await.is_err(), "no suspect column before it");
	assert!(sqlx::query("SELECT suspect FROM leads").execute(&pool).await.is_err());
	assert_eq!(count(&pool, "SELECT count(*) FROM reporting_leads").await, 2);
	assert_eq!(count(&pool, funnel).await, 2);

	migrator.run(&pool).await.unwrap();
	assert_eq!(marks().await.unwrap(), [("L-1".to_owned(), None), ("L-2".to_owned(), None)]);
	let refused = sqlx::query("UPDATE leads SET suspect = 'honeypot' WHERE lead_id = 'L-2'").execute(&pool).await;
	assert!(refused.is_err(), "the CHECK holds the vocabulary");
	panel.rebuild_projections().await.unwrap();
	assert_eq!(
		marks().await.unwrap(),
		[("L-1".to_owned(), None), ("L-2".to_owned(), Some("too_fast".to_owned()))],
		"the journal still has it"
	);
	assert_eq!(count(&pool, funnel).await, 2);
}

#[tokio::test]
async fn unregistered_types_are_kept_not_projected() {
	let db = TestDb::create().await;
	let (panel, secret, _) = setup(&db).await;
	let mut future = event("lead.created", at(0), "site", lead("L-9"), json!({"channel": "form", "utm": "gbp"}));
	future["typeVersion"] = json!(2);
	let events = [event("review.new", at(0), "site", json!({"brandId": "aquafix"}), json!({"stars": 2})), future];

	let got = panel.ingest(sign("aquafix-site", &secret, &events, now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [Outcome::Accepted { unregistered: true }, Outcome::Accepted { unregistered: true }]);
	let pool = db.pool().await;
	assert_eq!(count(&pool, "SELECT count(*) FROM events WHERE status = 'unregistered'").await, 2);
	assert_eq!(count(&pool, "SELECT count(*) FROM leads").await, 0);
	assert_eq!(count(&pool, "SELECT sum(events) FROM reporting_ingest_daily WHERE status = 'unregistered'").await, 2);
}

#[tokio::test]
async fn a_key_writes_only_its_brands_and_kind() {
	let db = TestDb::create().await;
	let (panel, secret, _) = setup(&db).await;
	let events = [
		event("lead.created", at(0), "site", json!({"brandId": "vifnet", "leadId": "V-1"}), json!({"channel": "form"})),
		event("lead.contacted", at(1), "panel", lead("L-1"), json!({})),
		event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"})),
	];
	let got = panel.ingest(sign("aquafix-site", &secret, &events, now()).batch(), now()).await.unwrap();
	let [foreign, posing, ok] = &got[..] else { panic!("{got:?}") };
	assert_eq!(foreign.outcome, Outcome::Rejected(panel_core::Invalid::new("this key may not write for brand vifnet")));
	assert!(matches!(&posing.outcome, Outcome::Rejected(e) if e.0.contains("source.kind panel")), "{posing:?}");
	assert_eq!(ok.outcome, ACCEPTED);
	assert_eq!((foreign.index, ok.index), (0, 2));
	assert_eq!(foreign.id, events[0]["id"].as_str().unwrap());
	let pool = db.pool().await;
	assert_eq!(count(&pool, "SELECT count(*) FROM events").await, 1, "rejected events are not journaled");
}

#[tokio::test]
async fn invalid_events_of_known_types_are_rejected() {
	let db = TestDb::create().await;
	let (panel, _, ops) = setup(&db).await;
	let events = [
		event("call.logged", at(0), "panel", lead("L-1"), json!({"outcome": "voicemail"})),
		event("job.won", at(0), "panel", lead("L-1"), json!({})),
		event("payment.received", at(0), "panel", lead("L-1"), json!({"billed": 100, "commission": 200, "currency": "EUR"})),
		json!({"id": "not even close"}),
	];
	let got = panel.ingest(sign("aquafix-ops", &ops, &events, now()).batch(), now()).await.unwrap();
	assert!(got.iter().all(|v| matches!(v.outcome, Outcome::Rejected(_))), "{got:?}");
	assert_eq!(got[3].id, "not even close");
	assert_eq!(count(&db.pool().await, "SELECT count(*) FROM events").await, 0);
}

/// One lead through the whole funnel, delivered out of order over two sources and three
/// batches, plus a lost lead.
async fn scenario(panel: &Panel, site: &str, ops: &str) {
	let created = event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}));
	let mut with_pii = event("lead.created", at(5), "panel", lead("L-2"), json!({"channel": "phone_inbound", "enteredBy": "op-1"}));
	with_pii["pii"] = json!({"phone": "+33 6 12 34 56 78", "need": "leak under the sink"});
	let job = json!({"brandId": "aquafix", "locationId": "paris-11", "leadId": "L-1", "jobId": "J-1"});
	let late = [
		event(
			"payment.received",
			at(300),
			"panel",
			job.clone(),
			json!({"billed": "12000", "commission": 1800, "currency": "EUR"}),
		),
		event("job.completed", at(240), "panel", job.clone(), json!({})),
		event("job.won", at(60), "panel", job, json!({})),
		event("lead.quoted", at(30), "panel", lead("L-1"), json!({"amount": 12000, "currency": "EUR"})),
	];
	let early = [
		event("call.attempted", at(2), "panel", lead("L-1"), json!({})),
		event("call.logged", at(3), "panel", lead("L-1"), json!({"outcome": "answered"})),
		event("lead.contacted", at(3), "panel", lead("L-1"), json!({"channel": "phone"})),
		with_pii,
		event("call.logged", at(6), "panel", lead("L-2"), json!({"outcome": "no_answer"})),
		event("lead.lost", at(7), "panel", lead("L-2"), json!({"reason": "no_answer"})),
	];
	for (key, secret, batch) in [("aquafix-ops", ops, &late[..]), ("aquafix-site", site, &[created][..]), ("aquafix-ops", ops, &early[..])] {
		let got = panel.ingest(sign(key, secret, batch, now()).batch(), now()).await.unwrap();
		assert!(got.iter().all(|v| v.outcome == ACCEPTED), "{got:?}");
	}
}

/// The projections as rows of JSON, in a stable order: what "the same state" means.
async fn projections(pool: &sqlx::SqlitePool) -> Vec<Value> {
	sqlx::query_scalar(
		"SELECT json_object('kind', kind, 'row', json(row)) FROM ( \
		   SELECT 'lead' AS kind, json_object('brand_id', brand_id, 'lead_id', lead_id, 'location_id', location_id, 'job_id', job_id, \
		     'stage', stage, 'channel', channel, 'manual', manual, 'created_at', created_at, 'contacted_at', contacted_at, \
		     'quoted_at', quoted_at, 'won_at', won_at, 'completed_at', completed_at, 'paid_at', paid_at, 'lost_at', lost_at, \
		     'lost_reason', lost_reason, 'last_event_id', hex(last_event_id), 'last_event_at', last_event_at) AS row FROM leads \
		   UNION ALL SELECT 'call', json_object('event_id', hex(event_id), 'brand_id', brand_id, 'lead_id', lead_id, 'location_id', location_id, \
		     'kind', kind, 'outcome', outcome, 'attempt_id', attempt_id, 'occurred_at', occurred_at, 'manual', manual) FROM calls \
		   UNION ALL SELECT 'payment', json_object('event_id', hex(event_id), 'brand_id', brand_id, 'lead_id', lead_id, 'location_id', location_id, \
		     'job_id', job_id, 'billed', billed, 'commission', commission, 'currency', currency, 'occurred_at', occurred_at, 'manual', manual) FROM payments \
		 ) ORDER BY kind, row",
	)
	.fetch_all(pool)
	.await
	.unwrap()
}

#[tokio::test]
async fn stages_are_projected_from_the_events() {
	let db = TestDb::create().await;
	let (panel, site, ops) = setup(&db).await;
	scenario(&panel, &site, &ops).await;
	let pool = db.pool().await;

	type LeadRow = (String, String, bool, Option<String>, Option<String>);
	let rows: Vec<LeadRow> = sqlx::query_as("SELECT lead_id, stage, manual, lost_reason, job_id FROM leads ORDER BY lead_id")
		.fetch_all(&pool)
		.await
		.unwrap();
	assert_eq!(
		rows,
		[
			("L-1".into(), "paid".into(), false, None, Some("J-1".into())),
			("L-2".into(), "lost".into(), true, Some("no_answer".into()), None),
		]
	);
	let times: Vec<Option<i64>> =
		sqlx::query_as::<_, (Option<i64>, Option<i64>, Option<i64>, Option<i64>)>("SELECT created_at, contacted_at, won_at, paid_at FROM leads WHERE lead_id = 'L-1'")
			.fetch_one(&pool)
			.await
			.map(|(a, b, c, d)| vec![a, b, c, d])
			.unwrap();
	let db = |m| Some(at(m).as_microsecond());
	assert_eq!(times, [db(0), db(3), db(60), db(300)]);

	assert_eq!(count(&pool, "SELECT count(*) FROM calls").await, 3);
	assert_eq!(count(&pool, "SELECT count(*) FROM calls WHERE kind = 'logged' AND outcome IS NOT NULL").await, 2);
	let (billed, commission): (i64, i64) = sqlx::query_as("SELECT billed, commission FROM payments").fetch_one(&pool).await.unwrap();
	assert_eq!((billed, commission), (12_000, 1_800));

	let funnel: (i64, i64, i64, i64, i64) = sqlx::query_as("SELECT leads, contacted, paid, lost_now, manual FROM reporting_funnel_daily")
		.fetch_one(&pool)
		.await
		.unwrap();
	assert_eq!(funnel, (2, 1, 1, 1, 1));
}

#[tokio::test]
async fn rebuild_lands_on_the_same_state() {
	let db = TestDb::create().await;
	let (panel, site, ops) = setup(&db).await;
	scenario(&panel, &site, &ops).await;
	let pool = db.pool().await;
	let incremental = projections(&pool).await;
	assert_eq!(incremental.len(), 2 + 3 + 1);

	let rebuilt = panel.rebuild_projections().await.unwrap();
	assert_eq!((rebuilt.events, rebuilt.registered, rebuilt.leads), (11, 11, 2));
	assert_eq!(projections(&pool).await, incremental);
}

#[tokio::test]
async fn rebuild_picks_up_a_type_registered_after_it_arrived() {
	let db = TestDb::create().await;
	let (panel, site, _) = setup(&db).await;
	let pool = db.pool().await;
	panel
		.ingest(
			sign("aquafix-site", &site, &[event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}))], now()).batch(),
			now(),
		)
		.await
		.unwrap();
	// As an older panel that did not know lead.created would have left it.
	sqlx::query("UPDATE events SET status = 'unregistered'").execute(&pool).await.unwrap();
	sqlx::query("DELETE FROM leads").execute(&pool).await.unwrap();

	let rebuilt = panel.rebuild_projections().await.unwrap();
	assert_eq!((rebuilt.registered, rebuilt.unregistered, rebuilt.leads), (1, 0, 1));
	assert_eq!(count(&pool, "SELECT count(*) FROM events WHERE status = 'registered'").await, 1);
	assert_eq!(count(&pool, "SELECT count(*) FROM leads").await, 1);
}

#[tokio::test]
async fn the_journal_is_append_only() {
	let db = TestDb::create().await;
	let (panel, site, _) = setup(&db).await;
	panel
		.ingest(
			sign("aquafix-site", &site, &[event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}))], now()).batch(),
			now(),
		)
		.await
		.unwrap();
	let pool = db.pool().await;
	for sql in [
		"UPDATE events SET brand_id = 'vifnet'",
		"UPDATE events SET properties = '{}'",
		"UPDATE events SET id = randomblob(16)",
		"DELETE FROM events",
	] {
		let err = sqlx::query(sql).execute(&pool).await.unwrap_err().to_string();
		assert!(err.contains("append-only"), "{sql}: {err}");
	}
	sqlx::query("UPDATE events SET status = 'invalid', status_reason = 'x'")
		.execute(&pool)
		.await
		.expect("status is the one thing that changes");
}

#[tokio::test]
async fn pii_is_sealed_and_kept_out_of_reporting() {
	let db = TestDb::create().await;
	let (panel, site, ops) = setup(&db).await;
	scenario(&panel, &site, &ops).await;
	let pool = db.pool().await;

	let (id, sealed): (uuid::Uuid, Vec<u8>) = sqlx::query_as("SELECT id, pii_sealed FROM events WHERE pii_sealed IS NOT NULL").fetch_one(&pool).await.unwrap();
	assert!(!sealed.windows(4).any(|w| w == b"+33 "), "no plaintext phone at rest");
	let pii = panel.pii(id).await.unwrap().unwrap();
	assert_eq!(pii["phone"], "+33 6 12 34 56 78");
	assert_eq!(panel.pii(uuid::Uuid::now_v7()).await.unwrap(), None);

	let columns: Vec<String> =
		sqlx::query_scalar("SELECT v.name || '.' || c.name FROM sqlite_schema v JOIN pragma_table_info(v.name) c WHERE v.type = 'view' AND v.name LIKE 'reporting\\_%' ESCAPE '\\'")
			.fetch_all(&pool)
			.await
			.unwrap();
	assert!(columns.len() > 20, "{columns:?}");
	for c in &columns {
		assert!(!["pii", "properties", "secret", "sealed"].iter().any(|bad| c.contains(bad)), "{c} in reporting");
	}
}

#[tokio::test]
async fn a_site_key_writes_no_operator_events() {
	let db = TestDb::create().await;
	let (panel, site, _) = setup(&db).await;
	let events = [
		event("payment.received", at(0), "site", lead("L-1"), json!({"billed": 100, "commission": 10, "currency": "EUR"})),
		event("job.won", at(0), "site", json!({"brandId": "aquafix", "leadId": "L-1", "jobId": "J-1"}), json!({})),
		event("call.logged", at(0), "site", lead("L-1"), json!({"outcome": "answered"})),
	];
	let got = panel.ingest(sign("aquafix-site", &site, &events, now()).batch(), now()).await.unwrap();
	assert_eq!(got[0].outcome, Outcome::Rejected(panel_core::Invalid::new("a site source may not write payment.received")));
	assert!(got.iter().all(|v| matches!(v.outcome, Outcome::Rejected(_))), "{got:?}");
	let pool = db.pool().await;
	assert_eq!(count(&pool, "SELECT count(*) FROM events").await, 0);

	// One that got into the journal anyway — an older panel, a hand-made row — is dropped
	// by the rebuild, which judges through the same rule.
	panel
		.ingest(
			sign("aquafix-site", &site, &[event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}))], now()).batch(),
			now(),
		)
		.await
		.unwrap();
	sqlx::query(
		"INSERT INTO events (id, schema, type, type_version, occurred_at, received_at, source_kind, source_id, brand_id, lead_id, properties, content_mac, status) \
		 VALUES ($1, 'sa.funnel.v1', 'payment.received', 1, $3, $3, 'site', 'x', 'aquafix', 'L-1', '{\"billed\": 100, \"commission\": 10, \"currency\": \"EUR\"}', $2, 'registered')",
	)
	.bind(uuid::Uuid::now_v7())
	.bind(vec![0u8; 32])
	.bind(now().as_microsecond())
	.execute(&pool)
	.await
	.unwrap();
	let rebuilt = panel.rebuild_projections().await.unwrap();
	assert_eq!((rebuilt.registered, rebuilt.invalid), (1, 1));
	assert_eq!(count(&pool, "SELECT count(*) FROM payments").await, 0);
	assert_eq!(
		count(&pool, "SELECT count(*) FROM events WHERE status = 'invalid' AND status_reason LIKE 'a site source%'").await,
		1
	);
}

#[tokio::test]
async fn a_back_dated_creation_does_not_take_over_a_lead() {
	let db = TestDb::create().await;
	let (panel, site, ops) = setup(&db).await;
	let by_hand = event("lead.created", at(10), "panel", lead("L-1"), json!({"channel": "phone_inbound", "enteredBy": "op-1"}));
	panel.ingest(sign("aquafix-ops", &ops, &[by_hand], now()).batch(), now()).await.unwrap();
	let back_dated = event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}));
	let later = now() + SignedDuration::from_mins(1);
	let got = panel.ingest(sign("aquafix-site", &site, &[back_dated], later).batch(), later).await.unwrap();
	assert_eq!(outcomes(&got), [ACCEPTED], "journaled: it is a fact that the site said so");

	let pool = db.pool().await;
	let lead_row = || async {
		sqlx::query_as::<_, (bool, Option<String>, i64)>("SELECT manual, channel, created_at FROM leads WHERE lead_id = 'L-1'")
			.fetch_one(&pool)
			.await
			.unwrap()
	};
	let (manual, channel, created) = lead_row().await;
	assert!(manual);
	assert_eq!(channel.as_deref(), Some("phone_inbound"));
	assert_eq!(created, at(10).as_microsecond());
	panel.rebuild_projections().await.unwrap();
	assert_eq!(lead_row().await, (manual, channel, created), "and the rebuild agrees");
}

#[tokio::test]
async fn what_the_journal_would_refuse_is_rejected_not_a_500() {
	let db = TestDb::create().await;
	let (panel, site, _) = setup(&db).await;
	let mut huge_version = event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}));
	huge_version["typeVersion"] = json!(u32::MAX);
	let events = [
		huge_version,
		event("review.new", at(0), "site", json!({"brandId": "aquafix"}), json!({"text": "nul \u{0} here"})),
		event("review.new", at(0), "site", json!({"brandId": "aquafix"}), json!({"text": "x".repeat(17 * 1024)})),
		event("lead.created", at(0), "site", lead("L-2"), json!({"channel": "form"})),
	];
	let got = panel
		.ingest(sign("aquafix-site", &site, &events, now()).batch(), now())
		.await
		.expect("per event, not for the batch");
	assert!(got[..3].iter().all(|v| matches!(v.outcome, Outcome::Rejected(_))), "{got:?}");
	assert_eq!(got[3].outcome, ACCEPTED, "the rest of the batch goes on");
	assert_eq!(count(&db.pool().await, "SELECT count(*) FROM events").await, 1);
}

#[tokio::test]
async fn a_source_is_named_by_its_key() {
	let db = TestDb::create().await;
	let (panel, site, _) = setup(&db).await;
	let mut e = event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}));
	e["source"]["id"] = json!("vifnet-site");
	let got = panel.ingest(sign_verbatim("aquafix-site", &site, &[e], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [Outcome::Rejected(panel_core::Invalid::new("source.id is not the id of the key that signed it"))]);
	let e = event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}));
	let got = panel.ingest(sign("aquafix-site", &site, &[e], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [ACCEPTED]);
	let source_id: String = sqlx::query_scalar("SELECT source_id FROM events").fetch_one(&db.pool().await).await.unwrap();
	assert_eq!(source_id, "aquafix-site");
}

#[tokio::test]
async fn ingest_waits_for_a_rebuild_in_progress() {
	let db = TestDb::create().await;
	let (panel, site, _) = setup(&db).await;
	let pool = db.pool().await;
	// Stands in for a rebuild: the write lock a rebuild holds, held open.
	let mut conn = pool.acquire().await.unwrap();
	let rebuild = panel::store::begin_write(&mut conn).await.unwrap();

	let events = [event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}))];
	let signed = sign("aquafix-site", &site, &events, now());
	let ingest = {
		let panel = panel.clone();
		tokio::spawn(async move { panel.ingest(signed.batch(), now()).await.map(|v| outcomes(&v)) })
	};
	tokio::time::sleep(std::time::Duration::from_millis(300)).await;
	assert!(!ingest.is_finished(), "ingest went ahead of the rebuild");
	assert_eq!(count(&pool, "SELECT count(*) FROM events").await, 0);
	rebuild.commit().await.unwrap();
	assert_eq!(ingest.await.unwrap().unwrap(), [ACCEPTED]);
}

#[test]
fn a_new_sources_secret_does_not_print() {
	let s = panel::NewSource {
		key_id: "aquafix-site".into(),
		secret: zeroize::Zeroizing::new("0123456789abcdef-xyzzy".into()),
	};
	let shown = format!("{s:?}");
	assert!(!shown.contains("xyzzy") && shown.contains("<redacted>"), "{shown}");
}

/// A panel with [`setup`]'s keys and a bot's key for aquafix; the site's and the bot's secrets.
async fn with_bot(db: &TestDb) -> (Panel, String, String) {
	let (panel, site, _) = setup(db).await;
	let bot = panel.add_source("aquafix-wa", SourceKind::Bot, brands(&["aquafix"])).await.unwrap().unwrap();
	(panel, site, bot.secret.to_string())
}

/// `(lead_id, messaged_channel, messaged_at)` of every lead, by id.
async fn messaged(pool: &sqlx::SqlitePool) -> Vec<(String, Option<String>, Option<i64>)> {
	sqlx::query_as("SELECT lead_id, messaged_channel, messaged_at FROM reporting_leads ORDER BY lead_id")
		.fetch_all(pool)
		.await
		.unwrap()
}

/// A bot starts a lead from a conversation (a messenger's channel only), and tells when a
/// customer wrote — naming the lead, or the ref the landing gave them, which the panel resolves
/// to the brand's newest lead carrying it and journals under that lead.
#[tokio::test]
async fn a_bot_starts_messenger_leads_and_says_when_a_customer_wrote() {
	let db = TestDb::create().await;
	let (panel, site, bot) = with_bot(&db).await;
	let pool = db.pool().await;

	let landing = [
		messenger_lead(at(0), "aquafix", "L-1", "whatsapp", "AQ-7K3F"),
		messenger_lead(at(1), "aquafix", "L-9", "telegram", "AQ-0000"),
	];
	let got = panel.ingest(sign("aquafix-site", &site, &landing, now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [ACCEPTED, ACCEPTED]);
	let refs: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as("SELECT lead_id, channel, message_ref FROM reporting_leads ORDER BY lead_id")
		.fetch_all(&pool)
		.await
		.unwrap();
	assert_eq!(refs[0], ("L-1".to_owned(), Some("whatsapp".to_owned()), Some("AQ-7K3F".to_owned())));

	let by_ref = |minutes, channel: &str, r: &str| event("lead.messaged", at(minutes), "bot", json!({"brandId": "aquafix"}), json!({"channel": channel, "messageRef": r}));
	let first = by_ref(5, "whatsapp", "AQ-7K3F");
	let early = by_ref(6, "telegram", "AQ-ZZZZ");
	let batch = [
		event(
			"lead.created",
			at(2),
			"bot",
			json!({"brandId": "aquafix", "leadId": "wa-01j9zk3f"}),
			json!({"channel": "whatsapp"}),
		),
		event("lead.created", at(2), "bot", json!({"brandId": "aquafix", "leadId": "wa-2"}), json!({"channel": "form"})),
		first.clone(),
		early.clone(),
		event("lead.messaged", at(7), "bot", lead("L-9"), json!({"channel": "telegram"})),
		event("lead.messaged", at(7), "bot", json!({"brandId": "aquafix"}), json!({"channel": "telegram"})),
	];
	let got = panel.ingest(sign("aquafix-wa", &bot, &batch, now()).batch(), now()).await.unwrap();
	assert_eq!(
		outcomes(&got),
		[
			ACCEPTED,
			Outcome::Rejected(panel_core::Invalid::new("a bot source writes lead.created only with channel whatsapp or telegram")),
			ACCEPTED,
			Outcome::Deferred(panel_core::Invalid::new(panel::UNKNOWN_REF)),
			ACCEPTED,
			Outcome::Rejected(panel_core::Invalid::new("subject.lead_id or properties.message_ref is required for lead.messaged")),
		]
	);
	assert!(panel::UNKNOWN_REF.starts_with("unknown_ref"), "what a bot matches on");
	let journaled: (String, Option<String>) = sqlx::query_as("SELECT lead_id, location_id FROM events WHERE id = $1")
		.bind(uuid::Uuid::parse_str(first["id"].as_str().unwrap()).unwrap())
		.fetch_one(&pool)
		.await
		.unwrap();
	assert_eq!(journaled, ("L-1".to_owned(), Some("royat".to_owned())), "journaled under the lead the ref names, at its place");
	let want = [
		("L-1".to_owned(), Some("whatsapp".to_owned()), Some(at(5).as_microsecond())),
		("L-9".to_owned(), Some("telegram".to_owned()), Some(at(7).as_microsecond())),
		("wa-01j9zk3f".to_owned(), None, None),
	];
	assert_eq!(messaged(&pool).await, want);
	let stage: String = sqlx::query_scalar("SELECT stage FROM leads WHERE lead_id = 'L-1'").fetch_one(&pool).await.unwrap();
	assert_eq!(stage, "created", "a message is the customer's, not the operator's contact");
	assert_eq!(count(&pool, "SELECT sum(messaged) FROM reporting_funnel_daily").await, 2);

	// A newer lead with the same ref: the next message is its, the first stays where it was —
	// a resend of it too, and so does the rebuild.
	let newer = [messenger_lead(at(10), "aquafix", "L-2", "telegram", "AQ-7K3F")];
	panel.ingest(sign("aquafix-site", &site, &newer, now()).batch(), now()).await.unwrap();
	let got = panel
		.ingest(sign("aquafix-wa", &bot, &[first, by_ref(11, "telegram", "AQ-7K3F")], now()).batch(), now())
		.await
		.unwrap();
	assert_eq!(outcomes(&got), [Outcome::Duplicate, ACCEPTED]);
	let after = messaged(&pool).await;
	assert_eq!((&after[0], &after[1].1), (&want[0], &Some("telegram".to_owned())), "{after:?}");
	let before = projections(&pool).await;
	let rebuilt_from = messaged(&pool).await;
	panel.rebuild_projections().await.unwrap();
	assert_eq!(projections(&pool).await, before);
	assert_eq!(messaged(&pool).await, rebuilt_from, "the rebuild does not look the ref up again");

	// The message that came before its lead was deferred, not journaled: sent again once the
	// landing's lead.created is in, it is taken under that lead.
	let deferred_id = uuid::Uuid::parse_str(early["id"].as_str().unwrap()).unwrap();
	let n: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE id = $1").bind(deferred_id).fetch_one(&pool).await.unwrap();
	assert_eq!(n, 0, "a deferred event is not journaled");
	let late = [messenger_lead(at(12), "aquafix", "L-3", "telegram", "AQ-ZZZZ")];
	panel.ingest(sign("aquafix-site", &site, &late, now()).batch(), now()).await.unwrap();
	let got = panel.ingest(sign("aquafix-wa", &bot, &[early], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [ACCEPTED]);
	let journaled: String = sqlx::query_scalar("SELECT lead_id FROM events WHERE id = $1").bind(deferred_id).fetch_one(&pool).await.unwrap();
	assert_eq!(journaled, "L-3");
}

/// A landing knows a messenger link was opened, never that a message was sent; a bot writes no
/// operator's events.
#[tokio::test]
async fn who_may_say_a_customer_wrote() {
	let db = TestDb::create().await;
	let (panel, site, bot) = with_bot(&db).await;
	let ops = panel.add_source("aquafix-ops2", SourceKind::Panel, brands(&["aquafix"])).await.unwrap().unwrap();
	panel
		.ingest(
			sign("aquafix-site", &site, &[messenger_lead(at(0), "aquafix", "L-1", "whatsapp", "AQ-7K3F")], now()).batch(),
			now(),
		)
		.await
		.unwrap();
	let messaged = event("lead.messaged", at(1), "site", lead("L-1"), json!({"channel": "whatsapp"}));
	let got = panel.ingest(sign("aquafix-site", &site, &[messaged], now()).batch(), now()).await.unwrap();
	assert!(matches!(&got[0].outcome, Outcome::Rejected(e) if e.0 == "a site source may not write lead.messaged"), "{got:?}");
	let contacted = event("lead.contacted", at(1), "bot", lead("L-1"), json!({"channel": "whatsapp"}));
	let got = panel.ingest(sign("aquafix-wa", &bot, &[contacted], now()).batch(), now()).await.unwrap();
	assert!(matches!(&got[0].outcome, Outcome::Rejected(e) if e.0 == "a bot source may not write lead.contacted"), "{got:?}");
	let by_hand = event(
		"lead.messaged",
		at(2),
		"panel",
		json!({"brandId": "aquafix"}),
		json!({"channel": "telegram", "messageRef": "AQ-7K3F"}),
	);
	let got = panel.ingest(sign("aquafix-ops2", &ops.secret, &[by_hand], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [ACCEPTED], "the panel may name the lead by its ref too");
	// A lead id the panel has no lead for is waited for, not taken as a phantom lead.
	let ahead = event("lead.messaged", at(3), "bot", lead("tg-404"), json!({"channel": "telegram"}));
	let got = panel.ingest(sign("aquafix-wa", &bot, std::slice::from_ref(&ahead), now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [Outcome::Deferred(panel_core::Invalid::new(panel::UNKNOWN_LEAD))]);
	assert_eq!(count(&db.pool().await, "SELECT count(*) FROM leads WHERE lead_id = 'tg-404'").await, 0);
	let started = event("lead.created", at(2), "bot", lead("tg-404"), json!({"channel": "telegram"}));
	let got = panel.ingest(sign("aquafix-wa", &bot, &[started, ahead], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [ACCEPTED, ACCEPTED], "sent again after its lead.created");
	let foreign = event(
		"lead.messaged",
		at(3),
		"bot",
		json!({"brandId": "vifnet"}),
		json!({"channel": "telegram", "messageRef": "VF-7K3F"}),
	);
	let got = panel.ingest(sign("aquafix-wa", &bot, &[foreign], now()).batch(), now()).await.unwrap();
	assert_eq!(outcomes(&got), [Outcome::Rejected(panel_core::Invalid::new("this key may not write for brand vifnet"))]);
}

/// The messenger migration rebuilt `leads`: undone, the words whatsapp and telegram have no
/// place and the new columns are gone, the rows are not; applied again, the rebuild brings the
/// channel, the ref and the message back from the journal.
#[tokio::test]
async fn the_messenger_migration_keeps_the_leads_both_ways() {
	const MESSENGER: i64 = 20261007090100;
	let db = TestDb::create().await;
	let (panel, site, bot) = with_bot(&db).await;
	let events = [
		event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"})),
		messenger_lead(at(1), "aquafix", "L-2", "whatsapp", "AQ-7K3F"),
	];
	panel.ingest(sign("aquafix-site", &site, &events, now()).batch(), now()).await.unwrap();
	let wrote = event(
		"lead.messaged",
		at(2),
		"bot",
		json!({"brandId": "aquafix"}),
		json!({"channel": "whatsapp", "messageRef": "AQ-7K3F"}),
	);
	assert_eq!(outcomes(&panel.ingest(sign("aquafix-wa", &bot, &[wrote], now()).batch(), now()).await.unwrap()), [ACCEPTED]);
	let pool = db.pool().await;
	let rows = || async {
		sqlx::query_as::<_, (String, Option<String>)>("SELECT lead_id, channel FROM reporting_leads ORDER BY lead_id")
			.fetch_all(&pool)
			.await
			.unwrap()
	};
	let funnel = "SELECT sum(leads) FROM reporting_funnel_daily";
	let index: i64 = count(&pool, "SELECT count(*) FROM sqlite_schema WHERE name = 'leads_by_message_ref'").await;
	assert_eq!(index, 1);

	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, MESSENGER - 1).await.unwrap();
	assert_eq!(
		rows().await,
		[("L-1".to_owned(), Some("form".to_owned())), ("L-2".to_owned(), None)],
		"whatsapp has no word before it"
	);
	assert!(sqlx::query("SELECT message_ref FROM leads").execute(&pool).await.is_err());
	assert!(sqlx::query("SELECT messaged_at FROM reporting_leads").execute(&pool).await.is_err());
	assert_eq!(count(&pool, funnel).await, 2);
	assert_eq!(
		count(&pool, "SELECT count(*) FROM sqlite_schema WHERE name = 'leads_by_booking'").await,
		1,
		"the old index is back"
	);
	let refused = sqlx::query("UPDATE leads SET channel = 'whatsapp' WHERE lead_id = 'L-2'").execute(&pool).await;
	assert!(refused.is_err(), "the old CHECK is back");

	migrator.run(&pool).await.unwrap();
	assert_eq!(rows().await, [("L-1".to_owned(), Some("form".to_owned())), ("L-2".to_owned(), None)]);
	assert_eq!(count(&pool, funnel).await, 2);
	let refused = sqlx::query("UPDATE leads SET messaged_channel = 'sms', messaged_at = 1 WHERE lead_id = 'L-2'")
		.execute(&pool)
		.await;
	assert!(refused.is_err(), "the CHECK holds the messengers");
	let refused = sqlx::query("UPDATE leads SET messaged_at = 1 WHERE lead_id = 'L-2'").execute(&pool).await;
	assert!(refused.is_err(), "a time and a messenger together");
	panel.rebuild_projections().await.unwrap();
	let back: (Option<String>, Option<String>, Option<String>, Option<i64>) =
		sqlx::query_as("SELECT channel, message_ref, messaged_channel, messaged_at FROM reporting_leads WHERE lead_id = 'L-2'")
			.fetch_one(&pool)
			.await
			.unwrap();
	assert_eq!(
		back,
		(Some("whatsapp".to_owned()), Some("AQ-7K3F".to_owned()), Some("whatsapp".to_owned()), Some(at(2).as_microsecond())),
		"the journal still has it"
	);
	assert_eq!(count(&pool, "SELECT sum(messaged) FROM reporting_funnel_daily").await, 1);
}

/// The bot kind migration made `sources` and `events` again: every row and reference kept, the
/// guards back; undone while no bot exists, and refused once one does.
#[tokio::test]
async fn the_bot_kind_migration_keeps_the_journal_and_its_keys() {
	const BOT_KIND: i64 = 20261007090000;
	const MESSENGER: i64 = 20261007090100;
	let db = TestDb::create().await;
	let (panel, site, _) = setup(&db).await;
	panel
		.ingest(
			sign("aquafix-site", &site, &[event("lead.created", at(0), "site", lead("L-1"), json!({"channel": "form"}))], now()).batch(),
			now(),
		)
		.await
		.unwrap();
	let pool = db.pool().await;
	let dump = || async {
		sqlx::query_as::<_, (String, String)>(
			"SELECT 'source', key_id || kind || brand_ids || hex(secret_sealed) || created_at FROM sources \
			 UNION ALL SELECT 'event', hex(id) || source_kind || coalesce(key_id, '') || coalesce(lead_id, '') || properties || hex(content_mac) FROM events \
			 ORDER BY 1, 2",
		)
		.fetch_all(&pool)
		.await
		.unwrap()
	};
	let before = dump().await;
	assert_eq!(before.len(), 3, "two keys and an event");

	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, MESSENGER - 1).await.unwrap();
	migrator.undo(&pool, BOT_KIND - 1).await.unwrap();
	assert_eq!(dump().await, before, "every row, every byte, down");
	let refused = sqlx::query("INSERT INTO sources (key_id, kind, brand_ids, secret_sealed, data_key_fp) VALUES ('b', 'bot', '[\"aquafix\"]', x'00', zeroblob(32))")
		.execute(&pool)
		.await;
	assert!(refused.is_err(), "no bot before it");
	migrator.run(&pool).await.unwrap();
	assert_eq!(dump().await, before, "and up");
	let dangling: Vec<(String,)> = sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check").fetch_all(&pool).await.unwrap();
	assert!(dangling.is_empty(), "{dangling:?}");
	let fks: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(&pool).await.unwrap();
	assert_eq!(fks, 1, "foreign keys are back on");
	assert!(sqlx::query("DELETE FROM events").execute(&pool).await.is_err(), "still append-only");
	assert!(sqlx::query("DELETE FROM sources").execute(&pool).await.is_err(), "sources are never deleted");
	assert!(sqlx::query("UPDATE sources SET kind = 'bot'").execute(&pool).await.is_err(), "only ever revoked");
	assert_eq!(count(&pool, "SELECT sum(events) FROM reporting_ingest_daily").await, 1, "the view is back");
	assert_eq!(count(&pool, "SELECT count(*) FROM sqlite_schema WHERE name = 'events_of_experiments'").await, 1);

	panel.add_source("aquafix-tg", SourceKind::Bot, brands(&["aquafix"])).await.unwrap().unwrap();
	migrator.undo(&pool, MESSENGER - 1).await.unwrap();
	assert!(migrator.undo(&pool, BOT_KIND - 1).await.is_err(), "a database with a bot's key does not go back");
	assert_eq!(count(&pool, "SELECT count(*) FROM sources").await, 3, "and loses nothing trying");
}

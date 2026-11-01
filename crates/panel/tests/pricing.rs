//! A brand's pricing against a real SQLite file: saved whole and checked against the brand's
//! locales, optimistic concurrency, removal, the history, what a site is answered, the brands
//! listed, the live topic, and the schema's guards.

use std::collections::BTreeSet;

use jiff::Timestamp;
use panel::{
	live::{Signal, Topic},
	place::Expected,
	pricing::{ChangeKind, PricingError},
	testing::{TestDb, event, panel, sign},
};
use panel_core::{ids::BrandId, place::Editor, pricing::PricingInputs};
use sa_auth::SourceKind;
use serde_json::{Value, json};
use uuid::Uuid;

fn now() -> Timestamp {
	"2026-10-03T12:00:00Z".parse().unwrap()
}

fn brand() -> BrandId {
	BrandId::parse("vifnet").unwrap()
}

fn admin() -> Editor {
	Editor::User {
		id: Uuid::now_v7(),
		label: "admin@evinvest.ltd".into(),
	}
}

/// kitstart's `valid/cleaning.json`: labelled in fr and en.
fn cleaning() -> Value {
	let raw = include_str!("../../panel_core/tests/fixtures/pricing/valid/cleaning.json");
	serde_json::from_str(raw).unwrap()
}

#[tokio::test]
async fn a_model_is_saved_whole_and_checked_against_what_was_read() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let brand = brand();

	let none = panel.pricing(&brand).await.unwrap();
	assert_eq!(
		(none.model.as_ref(), none.updated_at, none.locales.as_slice()),
		(None, None, ["fr".to_owned(), "en".to_owned()].as_slice())
	);
	assert_eq!(panel.live_pricing(&brand).await.unwrap(), None);

	let mut bus = panel.bus().subscribe();
	let saved = panel.set_pricing(&admin(), &brand, &cleaning(), Expected::At(None), now()).await.unwrap();
	assert_eq!(saved.model, Some(cleaning()));
	assert_eq!(saved.updated_at, Some(now()));
	assert_eq!(saved.updated_by.as_deref(), Some("admin@evinvest.ltd"));
	assert_eq!(panel.live_pricing(&brand).await.unwrap(), Some(cleaning()));
	let Ok(Signal::Changed(c)) = bus.try_recv() else { panic!("told after the commit") };
	assert_eq!((c.topic, c.brand.as_ref()), (Topic::Pricing, Some(&brand)));

	// The same model again changes nothing and says nothing.
	let again = panel.set_pricing(&admin(), &brand, &cleaning(), Expected::At(saved.updated_at), now()).await.unwrap();
	assert_eq!(again, saved);
	assert!(bus.try_recv().is_err());

	let mut cheaper = cleaning();
	cheaper["minimumCents"] = json!(3900);
	let stale = panel.set_pricing(&admin(), &brand, &cheaper, Expected::At(None), now()).await.unwrap_err();
	let PricingError::Stale(current) = stale else { panic!("{stale:?}") };
	assert_eq!(current.updated_at, saved.updated_at, "what it is now, for the editor to show");
	let next = panel.set_pricing(&admin(), &brand, &cheaper, Expected::At(saved.updated_at), now()).await.unwrap();
	assert!(next.updated_at > saved.updated_at, "a later token even within the same microsecond");

	let mut half = cheaper.clone();
	half["needs"]["standard"]["inputs"] = json!(["zone", "pets"]);
	let PricingError::Invalid(problems) = panel.set_pricing(&admin(), &brand, &half, Expected::Any, now()).await.unwrap_err() else {
		panic!("refused whole")
	};
	assert_eq!(problems[0].path, "model.needs.standard.inputs[1]");
	assert_eq!(panel.pricing(&brand).await.unwrap(), next, "nothing written");

	let removed = panel.remove_pricing(&admin(), &brand, Expected::At(next.updated_at), now()).await.unwrap();
	assert_eq!(removed.model, None);
	assert!(removed.updated_at > next.updated_at, "a removal is a version too");
	assert_eq!(panel.live_pricing(&brand).await.unwrap(), None);
	assert_eq!(panel.remove_pricing(&admin(), &brand, Expected::Any, now()).await.unwrap(), removed, "nothing to remove");

	let history = panel.pricing_history(&brand).await.unwrap();
	let summary: Vec<(ChangeKind, Option<&str>, Option<usize>)> = history.iter().map(|c| (c.kind, c.valid_from.as_deref(), c.needs)).collect();
	assert_eq!(
		summary,
		[
			(ChangeKind::Remove, None, None),
			(ChangeKind::Set, Some("2026-10-01"), Some(2)),
			(ChangeKind::Set, Some("2026-10-01"), Some(2))
		]
	);
}

#[tokio::test]
async fn every_label_must_be_in_the_brands_locales() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let brand = brand();
	// Labelled in English only.
	let raw = include_str!("../../panel_core/tests/fixtures/pricing/valid/at-the-cap.json");
	let english: Value = serde_json::from_str(raw).unwrap();

	let PricingError::Invalid(problems) = panel.set_pricing(&admin(), &brand, &english, Expected::Any, now()).await.unwrap_err() else {
		panic!("no fr label")
	};
	assert_eq!(problems[0].to_string(), "model.inputs[0].labels: no \"fr\" label");
	let en = panel.set_brand_locales(&admin(), &brand, &["en".to_owned()], now()).await.unwrap();
	assert_eq!(en.locales, ["en"]);
	panel.set_pricing(&admin(), &brand, &english, Expected::Any, now()).await.unwrap();
	assert!(panel.live_pricing(&brand).await.unwrap().is_some());

	// A locale added later: the saved model is no longer one the sites can show.
	panel.set_brand_locales(&admin(), &brand, &["en".to_owned(), "fr".to_owned()], now()).await.unwrap();
	assert_eq!(panel.live_pricing(&brand).await.unwrap(), None);
	assert!(panel.pricing(&brand).await.unwrap().model.is_some(), "kept for the editor to fix");

	// The preview holds a draft to the same rule, and prices it as the site would.
	let answers: PricingInputs = [("big".to_owned(), "max".to_owned()), ("times".to_owned(), "two".to_owned())].into();
	assert!(matches!(panel.preview_price(&brand, &english, "huge", &answers).await, Err(PricingError::Invalid(_))));
	let mut both = english.clone();
	both["inputs"][0]["labels"]["fr"] = json!("Grand");
	both["inputs"][0]["options"][0]["labels"]["fr"] = json!("Max");
	both["inputs"][1]["labels"]["fr"] = json!("Fois");
	both["inputs"][1]["options"][0]["labels"]["fr"] = json!("×2");
	assert_eq!(panel.preview_price(&brand, &both, "huge", &answers).await.unwrap(), Some(100_000_000));
	assert_eq!(panel.preview_price(&brand, &both, "tiny", &answers).await.unwrap(), None, "a need it does not price");
}

#[tokio::test]
async fn every_brand_the_panel_knows_is_listed() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let site = panel
		.add_source("aquafix-site", SourceKind::Site, BTreeSet::from([BrandId::parse("aquafix").unwrap()]))
		.await
		.unwrap()
		.unwrap();
	let lead = event(
		"lead.created",
		now(),
		"site",
		json!({"brandId": "aquafix", "locationId": "royat", "leadId": "L-1"}),
		json!({"channel": "form"}),
	);
	panel.ingest(sign("aquafix-site", &site.secret, &[lead], now()).batch(), now()).await.unwrap();
	panel
		.add_source("review-archive", SourceKind::ReviewArchive, BTreeSet::from([BrandId::parse("ecoclean").unwrap()]))
		.await
		.unwrap();
	panel.set_pricing(&admin(), &brand(), &cleaning(), Expected::Any, now()).await.unwrap();

	let all = panel.all_pricing().await.unwrap();
	let brands: Vec<(&str, bool)> = all.iter().map(|v| (v.brand.as_str(), v.model.is_some())).collect();
	assert_eq!(brands, [("aquafix", false), ("ecoclean", false), ("vifnet", true)]);
}

#[tokio::test]
async fn the_history_is_append_only_and_pricing_never_deleted() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	panel.set_pricing(&admin(), &brand(), &cleaning(), Expected::Any, now()).await.unwrap();
	let pool = db.pool().await;
	for (sql, why) in [
		("UPDATE pricing_changes SET changed_by = 'someone else'", "append-only: UPDATE refused"),
		("DELETE FROM pricing_changes", "append-only: DELETE refused"),
		("DELETE FROM pricing", "DELETE refused"),
		("UPDATE pricing SET model = '[]'", "CHECK constraint failed"),
		("INSERT INTO brand_locales VALUES ('vifnet', '[]', 0, 'cli')", "CHECK constraint failed"),
		("INSERT INTO pricing (brand_id, updated_at, updated_by) VALUES ('Vifnet', 0, 'cli')", "CHECK constraint failed"),
	] {
		let err = sqlx::query(sql).execute(&pool).await.unwrap_err().to_string();
		assert!(err.contains(why), "{sql}: {err}");
	}
}

/// Undone and applied again, the migration leaves a database the panel works on.
#[tokio::test]
async fn the_pricing_migration_goes_both_ways() {
	const PRICING: i64 = 20261004100000;
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let pool = db.pool().await;
	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, PRICING - 1).await.unwrap();
	let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'pricing%' OR name = 'brand_locales'")
		.fetch_one(&pool)
		.await
		.unwrap();
	assert_eq!(tables, 0);
	migrator.run(&pool).await.unwrap();
	panel.set_pricing(&admin(), &brand(), &cleaning(), Expected::Any, now()).await.unwrap();
	assert_eq!(panel.pricing_history(&brand()).await.unwrap().len(), 1);
}

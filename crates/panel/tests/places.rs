//! A place's live settings against a real SQLite file: every change journaled, optimistic
//! concurrency, revert, withdrawal, what a site is answered, and the schema's guards.

use jiff::{SignedDuration, Timestamp};
use panel::{
	place::{Expected, Live, PlaceError},
	testing::{TestDb, event, panel, sign},
};
use panel_core::{
	event::SourceKind,
	ids::{BrandId, LocationId},
	place::{ChangeKind, Editor, PlaceSettings},
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

fn now() -> Timestamp {
	"2026-10-03T12:00:00Z".parse().unwrap()
}

fn ids() -> (BrandId, LocationId) {
	(BrandId::parse("aquafix").unwrap(), LocationId::parse("royat").unwrap())
}

fn admin() -> Editor {
	Editor::User {
		id: Uuid::now_v7(),
		label: "admin@evinvest.ltd".into(),
	}
}

fn settings(v: Value) -> PlaceSettings {
	PlaceSettings::parse(&v).unwrap()
}

#[tokio::test]
async fn edits_are_journaled_and_checked_against_what_was_read() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (brand, slug) = ids();
	let by = admin();

	let unknown = panel.place(&brand, &slug).await.unwrap();
	assert!(unknown.settings.is_empty() && !unknown.withdrawn && unknown.updated_at.is_none());
	assert_eq!(
		panel.live_place(&brand, &slug).await.unwrap(),
		Live::Settings(PlaceSettings::default()),
		"unknown: {{}}, never withdrawn"
	);

	let first = settings(json!({"phone": "+33423500640", "serviceArea": ["Royat"]}));
	let v1 = panel.set_place(&by, &brand, &slug, first.clone(), Expected::At(None), now()).await.unwrap();
	assert_eq!(v1.settings, first);
	assert_eq!((v1.updated_at, v1.updated_by.as_deref()), (Some(now()), Some("admin@evinvest.ltd")));

	let stale = panel.set_place(&by, &brand, &slug, settings(json!({})), Expected::At(None), now()).await;
	assert!(matches!(stale, Err(PlaceError::Conflict)), "a second first edit is stale");

	// Within the same microsecond: updated_at still moves on, so the first editor's view is stale.
	let second = settings(json!({"phone": "+33612345678"}));
	let v2 = panel.set_place(&by, &brand, &slug, second.clone(), Expected::At(v1.updated_at), now()).await.unwrap();
	assert!(v2.updated_at > v1.updated_at, "{:?} after {:?}", v2.updated_at, v1.updated_at);
	assert!(matches!(
		panel.set_place(&by, &brand, &slug, first.clone(), Expected::At(v1.updated_at), now()).await,
		Err(PlaceError::Conflict)
	));
	let same = panel.set_place(&by, &brand, &slug, second.clone(), Expected::At(v2.updated_at), now()).await.unwrap();
	assert_eq!(same, v2, "nothing to change: nothing written");

	let history = panel.place_history(&brand, &slug).await.unwrap();
	let kinds: Vec<ChangeKind> = history.iter().map(|c| c.kind).collect();
	assert_eq!(kinds, [ChangeKind::Set, ChangeKind::Set], "newest first, the no-op left out");
	assert_eq!((history[0].before.clone(), history[0].after.clone()), (first.clone(), second.clone()));
	assert_eq!(history[1].before, PlaceSettings::default());

	let later = now() + SignedDuration::from_mins(1);
	let reverted = panel.revert_place(&Editor::Cli, &brand, &slug, history[0].id, Expected::Any, later).await.unwrap();
	assert_eq!(reverted.settings, first, "the before of that change is current");
	assert_eq!(reverted.updated_by.as_deref(), Some("cli"));
	let top = &panel.place_history(&brand, &slug).await.unwrap()[0];
	assert_eq!((top.kind, top.reverts, top.by.as_str()), (ChangeKind::Revert, Some(history[0].id), "cli"));
	assert!(matches!(
		panel.revert_place(&by, &brand, &slug, Uuid::now_v7(), Expected::Any, later).await,
		Err(PlaceError::NotFound)
	));
	let elsewhere = LocationId::parse("chamalieres").unwrap();
	assert!(
		matches!(panel.revert_place(&by, &brand, &elsewhere, history[0].id, Expected::Any, later).await, Err(PlaceError::NotFound)),
		"a change of another place"
	);

	let patched = panel
		.patch_place(
			&Editor::Cli,
			&brand,
			&slug,
			Map::from_iter([("whatsapp".to_owned(), json!("+33612345678"))]),
			&["serviceArea".to_owned()],
			later,
		)
		.await
		.unwrap();
	assert_eq!(patched.settings, settings(json!({"phone": "+33423500640", "whatsapp": "+33612345678"})));
	let refused = panel
		.patch_place(&Editor::Cli, &brand, &slug, Map::from_iter([("phone".to_owned(), json!("06"))]), &[], later)
		.await;
	assert!(matches!(refused, Err(PlaceError::Invalid(f)) if f.contains_key("phone")));

	let withdrawn = panel.withdraw_place(&by, &brand, &slug, true, later).await.unwrap();
	assert!(withdrawn.withdrawn);
	assert_eq!(withdrawn.settings, patched.settings, "the settings are kept");
	assert_eq!(panel.live_place(&brand, &slug).await.unwrap(), Live::Withdrawn);
	let restored = panel.withdraw_place(&by, &brand, &slug, false, later).await.unwrap();
	assert!(!restored.withdrawn);
	assert_eq!(panel.live_place(&brand, &slug).await.unwrap(), Live::Settings(patched.settings));
	let kinds: Vec<ChangeKind> = panel.place_history(&brand, &slug).await.unwrap().iter().map(|c| c.kind).collect();
	assert_eq!(kinds[..3], [ChangeKind::Restore, ChangeKind::Withdraw, ChangeKind::Set]);
}

#[tokio::test]
async fn the_places_list_knows_leads_counts_and_hand_registered_ones() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (brand, slug) = ids();
	let site = panel.add_source("aquafix-site", SourceKind::Site, [brand.clone()].into()).await.unwrap().unwrap();
	let lead = event(
		"lead.created",
		now(),
		"site",
		json!({"brandId": "aquafix", "locationId": "paris-11", "leadId": "L-1"}),
		json!({"channel": "form"}),
	);
	panel.ingest(sign("aquafix-site", &site.secret, &[lead], now()).batch(), now()).await.unwrap();

	let (v, added) = panel.register_place(&admin(), &brand, &slug, now()).await.unwrap();
	assert!(added && v.settings.is_empty());
	assert!(!panel.register_place(&admin(), &brand, &slug, now()).await.unwrap().1, "once");
	let vichy = LocationId::parse("vichy").unwrap();
	panel
		.set_place(&admin(), &brand, &vichy, settings(json!({"phone": "+33423500640"})), Expected::At(None), now())
		.await
		.unwrap();
	panel.withdraw_place(&admin(), &brand, &vichy, true, now()).await.unwrap();

	let rows: Vec<(String, bool, bool, bool)> = panel
		.places()
		.await
		.unwrap()
		.into_iter()
		.map(|p| (p.location_id, p.last_lead_at.is_some(), p.has_settings, p.withdrawn))
		.collect();
	assert_eq!(
		rows,
		[
			("paris-11".to_owned(), true, false, false),
			("royat".to_owned(), false, false, false),
			("vichy".to_owned(), false, true, true)
		]
	);
	let kinds: Vec<ChangeKind> = panel.place_history(&brand, &slug).await.unwrap().iter().map(|c| c.kind).collect();
	assert_eq!(kinds, [ChangeKind::Register]);

	// Cleared down to nothing, a place has no settings, and the site serves its baked config.
	let current = panel.place(&brand, &vichy).await.unwrap();
	panel
		.set_place(&admin(), &brand, &vichy, PlaceSettings::default(), Expected::At(current.updated_at), now())
		.await
		.unwrap();
	assert!(!panel.places().await.unwrap().iter().any(|p| p.has_settings));
}

#[tokio::test]
async fn the_history_is_append_only_and_places_are_never_deleted() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (brand, slug) = ids();
	panel
		.set_place(&admin(), &brand, &slug, settings(json!({"phone": "+33423500640"})), Expected::At(None), now())
		.await
		.unwrap();
	let pool = db.pool().await;
	for (sql, why) in [
		("UPDATE place_changes SET changed_by = 'someone else'", "append-only: UPDATE refused"),
		("DELETE FROM place_changes", "append-only: DELETE refused"),
		("DELETE FROM places", "DELETE refused"),
		("UPDATE places SET registered_by = 'someone else'", "only ever gets withdrawn or restored"),
		("DELETE FROM place_settings", "cleared to {}, not deleted"),
		("UPDATE place_settings SET settings = '[]'", "CHECK constraint failed"),
	] {
		let err = sqlx::query(sql).execute(&pool).await.unwrap_err().to_string();
		assert!(err.contains(why), "{sql}: {err}");
	}
	sqlx::query("UPDATE places SET withdrawn = 1").execute(&pool).await.expect("withdrawing is the one update");
}

/// Undone and applied again, the migration leaves a database the panel works on.
#[tokio::test]
async fn the_place_settings_migration_goes_both_ways() {
	const PLACES: i64 = 20261003130000;
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (brand, slug) = ids();
	let pool = db.pool().await;
	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, PLACES - 1).await.unwrap();
	let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'place%'").fetch_one(&pool).await.unwrap();
	assert_eq!(tables, 0);
	migrator.run(&pool).await.unwrap();
	panel
		.set_place(&admin(), &brand, &slug, settings(json!({"phone": "+33423500640"})), Expected::At(None), now())
		.await
		.unwrap();
	assert_eq!(panel.place_history(&brand, &slug).await.unwrap().len(), 1);
}

/// The messengers a place offers: its Telegram bot and the switches, set and cleared field by
/// field as the CLI does, and served to the sites as they are.
#[tokio::test]
async fn a_places_messengers_reach_the_sites() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (brand, slug) = ids();
	let set = Map::from_iter([
		("telegram".to_owned(), json!("aquafix_devis_bot")),
		("messengers".to_owned(), panel_core::place::messengers_from_spec("whatsapp=on,telegram=off").unwrap()),
		("whatsapp".to_owned(), json!("+33612345678")),
	]);
	panel.patch_place(&Editor::Cli, &brand, &slug, set, &[], now()).await.unwrap();
	assert_eq!(
		panel.live_place(&brand, &slug).await.unwrap(),
		Live::Settings(settings(
			json!({"whatsapp": "+33612345678", "telegram": "aquafix_devis_bot", "messengers": {"whatsapp": true, "telegram": false}})
		))
	);

	let bad = Map::from_iter([("telegram".to_owned(), json!("@aquafix")), ("messengers".to_owned(), json!({"sms": true}))]);
	let Err(PlaceError::Invalid(fields)) = panel.patch_place(&Editor::Cli, &brand, &slug, bad, &[], now()).await else {
		panic!("a bad bot and an unknown messenger passed")
	};
	assert_eq!(fields.keys().collect::<Vec<_>>(), ["messengers.sms", "telegram"]);

	let later = now() + SignedDuration::from_mins(1);
	panel
		.patch_place(&Editor::Cli, &brand, &slug, Map::new(), &["telegram".to_owned(), "messengers".to_owned()], later)
		.await
		.unwrap();
	assert_eq!(panel.live_place(&brand, &slug).await.unwrap(), Live::Settings(settings(json!({"whatsapp": "+33612345678"}))));
}

/// The review link of a place: served to the sites under `reviewUrl` as it was set, refused with a
/// reason when it is not Google's, kept in the history and put back by a revert.
#[tokio::test]
async fn a_places_review_link_is_set_served_and_reverted() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (brand, slug) = ids();
	let put = |url: &str| Map::from_iter([("reviewUrl".to_owned(), json!(url))]);

	let short = "https://g.page/r/CabcDEF123/review";
	panel.patch_place(&Editor::Cli, &brand, &slug, put(short), &[], now()).await.unwrap();
	assert_eq!(panel.live_place(&brand, &slug).await.unwrap(), Live::Settings(settings(json!({ "reviewUrl": short }))));

	let later = now() + SignedDuration::from_mins(1);
	let long = "https://search.google.com/local/writereview?placeid=ChIJN1t_tDeuEmsRUsoyG83frY4";
	panel.patch_place(&Editor::Cli, &brand, &slug, put(long), &[], later).await.unwrap();
	assert_eq!(panel.live_place(&brand, &slug).await.unwrap(), Live::Settings(settings(json!({ "reviewUrl": long }))));

	for bad in [
		"http://g.page/r/x/review",
		"javascript:alert(1)",
		"https://evil.example/review",
		"https://user@g.page/r/x",
		"https://g.page:444/r/x",
	] {
		let Err(PlaceError::Invalid(fields)) = panel.patch_place(&Editor::Cli, &brand, &slug, put(bad), &[], later).await else {
			panic!("{bad} passed")
		};
		assert_eq!(fields.keys().collect::<Vec<_>>(), ["reviewUrl"], "{bad}");
	}
	assert_eq!(
		panel.live_place(&brand, &slug).await.unwrap(),
		Live::Settings(settings(json!({ "reviewUrl": long }))),
		"a refusal changes nothing"
	);

	// Newest first: the second change is the one whose `before` held the short link.
	let history = panel.place_history(&brand, &slug).await.unwrap();
	assert_eq!(history[0].before, settings(json!({ "reviewUrl": short })));
	let at = later + SignedDuration::from_mins(1);
	panel.revert_place(&Editor::Cli, &brand, &slug, history[0].id, Expected::Any, at).await.unwrap();
	assert_eq!(panel.live_place(&brand, &slug).await.unwrap(), Live::Settings(settings(json!({ "reviewUrl": short }))));
}

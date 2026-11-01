//! The form variants (FORM-VARIANTS-SPEC) in the engine, on a real SQLite file: a lead's flow
//! and price through ingest, the projection, the list's filter, a rebuild and the migration.

use jiff::{SignedDuration, Timestamp};
use panel::{
	Outcome, Panel,
	operator::{LeadQuery, Pii},
	testing::{TestDb, event, panel, sign},
};
use panel_core::{Invalid, fact::LeadFlow, ids::BrandId};
use sa_auth::SourceKind;
use serde_json::{Value, json};

fn now() -> Timestamp {
	"2026-10-04T12:00:00Z".parse().unwrap()
}

fn at(minutes: i64) -> Timestamp {
	"2026-10-04T09:00:00Z".parse::<Timestamp>().unwrap() + SignedDuration::from_mins(minutes)
}

fn aquafix() -> BrandId {
	BrandId::parse("aquafix").unwrap()
}

/// A panel with aquafix's site key; its secret.
async fn setup(db: &TestDb) -> (Panel, String) {
	let panel = panel(db).await;
	let site = panel.add_source("aquafix-site", SourceKind::Site, [aquafix()].into()).await.unwrap().unwrap();
	(panel, site.secret.to_string())
}

fn created(lead: &str, minutes: i64, properties: Value) -> Value {
	event(
		"lead.created",
		at(minutes),
		"site",
		json!({"brandId": "aquafix", "locationId": "royat", "leadId": lead}),
		properties,
	)
}

async fn ingest(panel: &Panel, secret: &str, events: &[Value]) -> Vec<Outcome> {
	let got = panel.ingest(sign("aquafix-site", secret, events, now()).batch(), now()).await.unwrap();
	got.into_iter().map(|v| v.outcome).collect()
}

const ACCEPTED: Outcome = Outcome::Accepted { unregistered: false };

fn rejected(why: &str) -> Outcome {
	Outcome::Rejected(Invalid::new(why))
}

type OfferRow = (String, Option<String>, Option<i64>, Option<String>, Option<String>);

async fn offers(pool: &sqlx::SqlitePool) -> Vec<OfferRow> {
	sqlx::query_as("SELECT lead_id, flow, quoted_cents, pricing_valid_from, estimate_inputs FROM reporting_leads ORDER BY lead_id")
		.fetch_all(pool)
		.await
		.unwrap()
}

#[tokio::test]
async fn a_lead_says_its_flow_and_price_and_the_registry_holds_them_together() {
	let db = TestDb::create().await;
	let (panel, secret) = setup(&db).await;
	let inputs = |n: usize| Value::Object((0..n).map(|i| (format!("input-{i}"), json!("v"))).collect());
	let got = ingest(
		&panel,
		&secret,
		&[
			created("L-0", 0, json!({"channel": "form"})),
			created("L-1", 1, json!({"channel": "form", "flow": "quote"})),
			created(
				"L-2",
				2,
				json!({"channel": "form", "flow": "estimate", "quotedCents": "12900", "pricingValidFrom": "2026-10-01", "estimateInputs": {"zone": "a", "bedrooms": "2"}}),
			),
			created(
				"L-3",
				3,
				json!({"channel": "callback", "flow": "fixed", "quoted_cents": 8000, "pricing_valid_from": "2026-09-15"}),
			),
			created("L-4", 4, json!({"channel": "form", "flow": "quote", "quotedCents": 100, "pricingValidFrom": "2026-10-01"})),
			created("L-5", 5, json!({"channel": "form", "flow": "estimate", "quotedCents": 100})),
			created(
				"L-6",
				6,
				json!({"channel": "form", "flow": "estimate", "quotedCents": 100, "pricingValidFrom": "2026-10-01", "estimateInputs": {"zone": "Zone A"}}),
			),
			created(
				"L-7",
				7,
				json!({"channel": "form", "flow": "estimate", "quotedCents": 100, "pricingValidFrom": "2026-10-01", "estimateInputs": inputs(13)}),
			),
			created(
				"L-8",
				8,
				json!({"channel": "form", "flow": "fixed", "quotedCents": 100, "pricingValidFrom": "2026-10-01", "estimateInputs": {"zone": "a"}}),
			),
			created("L-9", 9, json!({"channel": "form", "flow": "subscription"})),
			created("L-10", 10, json!({"channel": "form", "quotedCents": 100, "pricingValidFrom": "2026-10-01"})),
			created("L-11", 11, json!({"channel": "form", "flow": "estimate", "quotedCents": -1, "pricingValidFrom": "2026-10-01"})),
			created("L-12", 12, json!({"channel": "form", "flow": "fixed", "quotedCents": 100, "pricingValidFrom": "1 oct 2026"})),
			created(
				"L-13",
				13,
				json!({"channel": "form", "flow": "estimate", "quotedCents": 100, "pricingValidFrom": "2026-10-01", "estimateInputs": inputs(12)}),
			),
		],
	)
	.await;
	let only_priced = "properties.quoted_cents and properties.pricing_valid_from are only for flow estimate or fixed";
	assert_eq!(
		got,
		[
			ACCEPTED,
			ACCEPTED,
			ACCEPTED,
			ACCEPTED,
			rejected(only_priced),
			rejected("properties.quoted_cents and properties.pricing_valid_from go together"),
			rejected("properties.estimate_inputs keys and values are 1–40 of [a-z0-9_-]"),
			rejected("properties.estimate_inputs holds more than 12 inputs"),
			rejected("properties.estimate_inputs is only for flow estimate"),
			rejected("properties.flow is not one of quote, estimate, fixed"),
			rejected(only_priced),
			rejected("properties.quoted_cents is negative"),
			rejected("properties.pricing_valid_from is not a date like \"2026-10-01\""),
			ACCEPTED,
		]
	);
	let pool = db.pool().await;
	let want: Vec<OfferRow> = vec![
		("L-0".into(), None, None, None, None),
		("L-1".into(), Some("quote".into()), None, None, None),
		("L-13".into(), Some("estimate".into()), Some(100), Some("2026-10-01".into()), Some(inputs(12).to_string())),
		(
			"L-2".into(),
			Some("estimate".into()),
			Some(12_900),
			Some("2026-10-01".into()),
			Some(r#"{"bedrooms":"2","zone":"a"}"#.into()),
		),
		("L-3".into(), Some("fixed".into()), Some(8000), Some("2026-09-15".into()), None),
	];
	assert_eq!(offers(&pool).await, want);
	panel.rebuild_projections().await.unwrap();
	assert_eq!(offers(&pool).await, want, "the rebuild agrees");

	// The list, by flow.
	let listed = |flow| {
		let panel = panel.clone();
		async move {
			let q = LeadQuery {
				flow,
				limit: 50,
				..LeadQuery::default()
			};
			let page = panel.leads(&q, Pii::Withhold, now()).await.unwrap();
			page.leads.into_iter().map(|l| l.row.lead_id).collect::<Vec<_>>()
		}
	};
	assert_eq!(listed(Some(LeadFlow::Estimate)).await, ["L-13", "L-2"]);
	assert_eq!(listed(Some(LeadFlow::Fixed)).await, ["L-3"]);
	assert_eq!(listed(Some(LeadFlow::Quote)).await, ["L-1"], "a lead that said no flow is not listed as a quote");
	assert_eq!(listed(None).await.len(), 5);
}

/// The migration adds the columns and remakes the views: undone, they are gone and the leads
/// are not; applied again, the rebuild brings the flows back from the journal.
#[tokio::test]
async fn the_flow_migration_keeps_the_leads_both_ways() {
	const FLOW: i64 = 20261004090000;
	let db = TestDb::create().await;
	let (panel, secret) = setup(&db).await;
	let events = [
		created("L-1", 0, json!({"channel": "form", "flow": "fixed", "quotedCents": 8000, "pricingValidFrom": "2026-09-15"})),
		created("L-2", 1, json!({"channel": "form"})),
	];
	assert_eq!(ingest(&panel, &secret, &events).await, [ACCEPTED, ACCEPTED]);
	let pool = db.pool().await;
	let leads = "SELECT sum(leads) FROM reporting_funnel_daily";
	let count = |sql: &'static str| {
		let pool = pool.clone();
		async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await.unwrap() }
	};
	assert_eq!(count("SELECT sum(fixed) FROM reporting_funnel_daily").await, 1);

	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, FLOW - 1).await.unwrap();
	assert!(sqlx::query("SELECT flow FROM leads").execute(&pool).await.is_err(), "no flow before it");
	assert_eq!(count("SELECT count(*) FROM reporting_leads").await, 2);
	assert_eq!(count(leads).await, 2);

	migrator.run(&pool).await.unwrap();
	assert_eq!(offers(&pool).await[0], ("L-1".into(), None, None, None, None), "NULL until rebuilt");
	let refused = sqlx::query("UPDATE leads SET flow = 'subscription' WHERE lead_id = 'L-1'").execute(&pool).await;
	assert!(refused.is_err(), "the CHECK holds the vocabulary");
	let refused = sqlx::query("UPDATE leads SET pricing_valid_from = '2026-02-30' WHERE lead_id = 'L-1'").execute(&pool).await;
	assert!(refused.is_err(), "a real day");
	panel.rebuild_projections().await.unwrap();
	assert_eq!(
		offers(&pool).await[0],
		("L-1".into(), Some("fixed".into()), Some(8000), Some("2026-09-15".into()), None),
		"the journal still has it"
	);
	assert_eq!(count(leads).await, 2);
}

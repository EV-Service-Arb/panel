//! The Connected sites table (docs/ARCHITECTURE.md, the conductor's API): every active site key's
//! own report beside what sites saw; whatever fails is a value in its place, not the page's error.

use std::collections::BTreeMap;

use axum::{
	Json, Router,
	body::Body,
	http::{Method, StatusCode},
	response::Redirect,
	routing::get,
};
use sa_auth::{Principal, Service, SourceKind};
use sa_core::ids::{BrandId, SiteOrigin};
use sa_panel::testing::{TestDb, admin, operator, panel};
use sa_server::testing::{Browser, app, upstream};
use serde_json::{Value, json};

fn report(brand: &str) -> Value {
	json!({
		"brand": brand,
		"kitstart": "0.23.0",
		"checked_at": "2026-11-01T15:00:00Z",
		"sources": {
			"places": {"royat": {"state": "live"}, "lyon-est": {"state": "withdrawn"}},
			"pricing": {"state": "empty"},
			"experiments": {"state": "down", "detail": "503 from sites_panel"},
		},
	})
}

async fn add(panel: &sa_panel::Panel, key_id: &str, kind: SourceKind, brand: &str, origin: Option<&str>) {
	let origin = origin.map(|o| SiteOrigin::parse(o).unwrap());
	panel.add_source(key_id, kind, [BrandId::parse(brand).unwrap()].into(), origin).await.unwrap().unwrap();
}

async fn connected(app: &Router, b: &mut Browser) -> (StatusCode, Value) {
	let (status, _, body) = b.send_body(app, Method::GET, "/api/v1/sources/sites", &[], false, Body::empty()).await;
	(status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn every_site_key_shows_its_own_report_beside_what_sites_saw() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (landing, _) = upstream(Router::new().route("/.well-known/sa-site", get(|| async { Json(report("aquafix")) }))).await;
	let (moved, _) = upstream(Router::new().route("/.well-known/sa-site", get(|| async { Redirect::temporary("https://elsewhere.test/") }))).await;
	let received = json!({"aquafix-site": "2026-11-01T14:00:00Z"});
	let reads = json!({"since": "2026-11-01T10:00:00Z", "reads": {"aquafix": {"locations": "2026-11-01T14:30:00Z"}}});
	let (sites, seen) = upstream(
		Router::new()
			.route("/api/conductor/sources/received", get(move || async move { Json(received) }))
			.route("/api/conductor/internal-reads", get(move || async move { Json(reads) })),
	)
	.await;

	add(&panel, "aquafix-site", SourceKind::Site, "aquafix", Some(&landing)).await;
	add(&panel, "vifnet-site", SourceKind::Site, "vifnet", Some(&moved)).await;
	add(&panel, "lyon-site", SourceKind::Site, "lyon", Some("http://127.0.0.1:9")).await;
	add(&panel, "wrong-site", SourceKind::Site, "vifnet", Some(&landing)).await;
	add(&panel, "gone-site", SourceKind::Site, "aquafix", Some(&landing)).await;
	assert!(panel.revoke_source("gone-site").await.unwrap());
	add(&panel, "aquafix-bot", SourceKind::Bot, "aquafix", None).await;
	sqlx::query("INSERT INTO sources (key_id, kind, brand_ids, secret_sealed, data_key_fp) VALUES ('old-site', 'site', '[\"old\"]', x'00', zeroblob(32))")
		.execute(panel.store().pool())
		.await
		.unwrap();

	let mut op = Browser::default();
	let op_app = app(panel.clone(), operator(), &sites);
	op.sign_in(&op_app).await;
	assert_eq!(connected(&op_app, &mut op).await.0, StatusCode::FORBIDDEN, "an admin's screen");
	assert!(seen.lock().unwrap().is_empty(), "nothing asked of sites for a refused caller");

	let app = app(panel.clone(), admin(), &sites);
	let mut b = Browser::default();
	b.sign_in(&app).await;
	let (status, got) = connected(&app, &mut b).await;
	assert_eq!(status, StatusCode::OK, "{got}");
	assert_eq!(got["received"], json!({"state": "read", "value": {"aquafix-site": "2026-11-01T14:00:00Z"}}));
	assert_eq!(
		got["internal_reads"],
		json!({"state": "read", "value": {"since": "2026-11-01T10:00:00Z", "reads": {"aquafix": {"locations": "2026-11-01T14:30:00Z", "pricing": null, "experiments": null}}}})
	);
	let rows: BTreeMap<&str, &Value> = got["sites"].as_array().unwrap().iter().map(|r| (r["key_id"].as_str().unwrap(), r)).collect();
	assert_eq!(
		rows.keys().copied().collect::<Vec<_>>(),
		["aquafix-site", "lyon-site", "old-site", "vifnet-site", "wrong-site"],
		"active site keys only"
	);
	assert_eq!(
		*rows["aquafix-site"],
		json!({"key_id": "aquafix-site", "brands": ["aquafix"], "origin": landing, "report": {"state": "reported", "report": report("aquafix")}})
	);
	assert_eq!(rows["old-site"]["report"], json!({"state": "no_origin"}));
	assert_eq!(rows["old-site"]["origin"], Value::Null);
	for (key, why) in [("vifnet-site", "307"), ("lyon-site", "127.0.0.1:9"), ("wrong-site", "aquafix")] {
		let r = &rows[key]["report"];
		assert_eq!(r["state"], "failed", "{key}: {r}");
		assert!(r["detail"].as_str().unwrap().contains(why), "{key}: {r}");
	}
	let asked = seen.lock().unwrap().clone();
	assert_eq!(asked.len(), 2);
	for s in &asked {
		assert!(matches!(s.assertion(Service::Sites, None).principal, Principal::Conductor), "{}", s.path_and_query);
	}

	// sites failing: its two columns say so, the sites' own reports still show.
	let (broken, _) = upstream(
		Router::new()
			.route("/api/conductor/sources/received", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
			.route("/api/conductor/internal-reads", get(|| async { StatusCode::INTERNAL_SERVER_ERROR })),
	)
	.await;
	let app = sa_server::testing::app(panel.clone(), admin(), &broken);
	let mut b = Browser::default();
	b.sign_in(&app).await;
	let (status, got) = connected(&app, &mut b).await;
	assert_eq!(status, StatusCode::OK, "{got}");
	assert_eq!(got["received"]["state"], "failed", "{got}");
	assert_eq!(got["internal_reads"]["state"], "failed", "{got}");
	assert_eq!(got["sites"][0]["report"]["state"], "reported", "{got}");
}

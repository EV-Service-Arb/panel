//! A GMB's point: Places asked once for it, carried by `GET /gmails`, filled in later by
//! `locate`, placed by hand where Places has none; and the browser's map.

use std::sync::{Arc, Mutex};

use axum::{
	Json, Router,
	body::Body,
	extract::{Path, State},
	http::{Method, StatusCode},
	response::{IntoResponse, Response},
	routing::get,
};
use sa_panel::{
	Panel,
	import::Export,
	seal::DataKey,
	testing::{TestDb, operator, panel_on},
};
use sa_places::{Google, Keys};
use sa_server::{
	concierge::{Concierge, DevIdentity},
	gmbs::MapDto,
	http,
	signin::{SignIn, SignInConfig},
	testing::{Browser, ORIGIN},
};
use serde_json::{Value, json};

const PROCOPE: &str = "ChIJprocopeprocopeproc";
const FLORE: &str = "ChIJfloreflorefloreflo";
/// A service-area business: Places gives it no point.
const ASSIST: &str = "ChIJassistassistassis";

/// Places, counting what it is asked.
async fn places() -> (Arc<Mutex<Vec<String>>>, String) {
	async fn details(State(asked): State<Arc<Mutex<Vec<String>>>>, Path(id): Path<String>) -> Response {
		asked.lock().unwrap().push(id.clone());
		let (lat, lng) = match id.as_str() {
			PROCOPE => (48.853, 2.3389),
			FLORE => (48.854, 2.3326),
			ASSIST => return Json(json!({ "id": id, "displayName": { "text": "Mold Assist" } })).into_response(),
			_ => return (StatusCode::NOT_FOUND, "not found").into_response(),
		};
		Json(json!({ "id": id, "displayName": { "text": id }, "formattedAddress": "Paris", "location": { "latitude": lat, "longitude": lng } })).into_response()
	}
	let asked = Arc::<Mutex<Vec<String>>>::default();
	let app = Router::new().route("/v1/places/{id}", get(details)).with_state(asked.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let base = format!("http://{}/v1/places", listener.local_addr().unwrap());
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(asked, base)
}

/// A panel on `db` asking `base` with a key (`None`: keyless), its cache in `cache`, GMBs open.
async fn panel(db: &TestDb, base: Option<&str>, cache: &std::path::Path) -> Panel {
	let keys = Keys::new(base.map(|_| vec!["k".to_owned()]).unwrap_or_default());
	let google = Google::new(keys, cache.to_owned()).unwrap();
	let google = match base {
		Some(b) => google.at(b),
		None => google,
	};
	let key = DataKey::from_hex(&DataKey::generate_hex().unwrap()).unwrap();
	panel_on(db.store().await, key, google)
}

async fn opened(p: &Panel) {
	let empty = Export {
		people: vec![],
		gmails: vec![],
		gmbs: vec![],
		ledger: vec![],
		kickbacks: vec![],
	};
	p.import_review_archive(empty).await.unwrap();
}

fn app(panel: Panel, map: Option<MapDto>) -> Router {
	let who = DevIdentity {
		user_id: DevIdentity::named("dev@localhost"),
		permissions: operator(),
		email: "dev@localhost".to_owned(),
	};
	let config = SignInConfig {
		panel_origin: ORIGIN.to_owned(),
		concierge_origin: ORIGIN.to_owned(),
	};
	http::app(SignIn::new(panel, Concierge::dev(who), config), None, None, Default::default(), None, map)
}

async fn call(b: &mut Browser, app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
	let json = [("content-type", "application/json")];
	let (status, _, bytes) = b
		.send_body(
			app,
			method,
			uri,
			if body.is_some() { &json } else { &[] },
			true,
			body.map_or(Body::empty(), |b| Body::from(b.to_string())),
		)
		.await;
	(status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_gmb_is_located_when_added_and_its_place_asked_once() {
	let (asked, base) = places().await;
	let (db, cache) = (TestDb::create().await, tempfile::tempdir().unwrap());
	let p = panel(&db, Some(&base), cache.path()).await;
	opened(&p).await;
	let app = app(p, None);
	let mut b = Browser::default();
	b.sign_in(&app).await;

	let mut gmails = Vec::new();
	for gmail in ["a@gmail.com", "b@gmail.com"] {
		let (status, added) = call(&mut b, &app, Method::POST, "/api/v1/gmails", Some(json!({ "gmail": gmail }))).await;
		assert_eq!(status, StatusCode::CREATED, "{added}");
		gmails.push(added["id"].as_i64().unwrap());
	}
	for gmail in &gmails {
		let (status, gmb) = call(&mut b, &app, Method::POST, &format!("/api/v1/gmails/{gmail}/gmbs"), Some(json!({ "place_id": PROCOPE }))).await;
		assert_eq!(status, StatusCode::CREATED, "{gmb}");
		assert_eq!(gmb["location"], json!({ "lat": 48.853, "lng": 2.3389, "placed": false }));
	}
	assert_eq!(asked.lock().unwrap().len(), 1, "the same place under another gmail is not asked again");

	let (_, listed) = call(&mut b, &app, Method::GET, "/api/v1/gmails", None).await;
	for g in listed["gmails"].as_array().unwrap() {
		assert_eq!(g["gmbs"][0]["location"], json!({ "lat": 48.853, "lng": 2.3389, "placed": false }), "{listed}");
	}
}

#[tokio::test]
async fn locate_fills_only_the_missing_points_and_pays_for_each_once() {
	let (asked, base) = places().await;
	let (db, cache) = (TestDb::create().await, tempfile::tempdir().unwrap());
	let keyless = panel(&db, None, cache.path()).await;
	opened(&keyless).await;
	let owner = seen(&keyless, "dev@localhost").await;
	let gmail = keyless.add_gmail(owner, "a@gmail.com", jiff::Timestamp::now()).await.unwrap().unwrap();
	for place in [PROCOPE, FLORE] {
		let gmb = keyless.add_gmb(Some(owner), gmail.id, place, jiff::Timestamp::now()).await.unwrap().unwrap();
		assert_eq!(gmb.location, None, "no key, no point");
	}

	let keyed = panel(&db, Some(&base), cache.path()).await;
	assert_eq!(keyed.locate().await.unwrap(), (2, vec![], vec![]));
	assert!(keyed.gmails(owner).await.unwrap()[0].1.iter().all(|b| b.location.is_some()));
	assert_eq!(keyed.locate().await.unwrap(), (0, vec![], vec![]));
	assert_eq!(asked.lock().unwrap().len(), 2, "a second run asks nothing");
}

async fn seen(p: &Panel, email: &str) -> uuid::Uuid {
	let user_id = DevIdentity::named(email);
	let user = sa_panel::tokens::User {
		user_id,
		email: email.into(),
		name: email.into(),
	};
	p.user_seen(&user, jiff::Timestamp::now()).await.unwrap();
	user_id
}

#[tokio::test]
async fn a_service_area_gmb_has_no_point_until_placed_on_the_map() {
	let (asked, base) = places().await;
	let (db, cache) = (TestDb::create().await, tempfile::tempdir().unwrap());
	let keyless = panel(&db, None, cache.path()).await;
	opened(&keyless).await;
	let owner = seen(&keyless, "dev@localhost").await;
	let gmail = keyless.add_gmail(owner, "a@gmail.com", jiff::Timestamp::now()).await.unwrap().unwrap();
	keyless.add_gmb(Some(owner), gmail.id, FLORE, jiff::Timestamp::now()).await.unwrap().unwrap();

	let keyed = panel(&db, Some(&base), cache.path()).await;
	let app = app(keyed.clone(), None);
	let mut b = Browser::default();
	b.sign_in(&app).await;
	let (status, assist) = call(&mut b, &app, Method::POST, &format!("/api/v1/gmails/{}/gmbs", gmail.id), Some(json!({ "place_id": ASSIST }))).await;
	assert_eq!((status, &assist["location"]), (StatusCode::CREATED, &Value::Null), "{assist}");
	assert_eq!(
		keyed.locate().await.unwrap(),
		(1, vec![ASSIST.to_owned()], vec![]),
		"listed, and the run goes on to the next place"
	);

	let location = format!("/api/v1/gmbs/{}/location", assist["id"]);
	let point = |gmails: &Value| gmails["gmails"][0]["gmbs"].as_array().unwrap().iter().find(|g| g["id"] == assist["id"]).unwrap()["location"].clone();
	for (lat, lng) in [(43.589, -79.644), (43.6, -79.65)] {
		let (status, _) = call(&mut b, &app, Method::PUT, &location, Some(json!({ "lat": lat, "lng": lng }))).await;
		assert_eq!(status, StatusCode::NO_CONTENT);
		let (_, listed) = call(&mut b, &app, Method::GET, "/api/v1/gmails", None).await;
		assert_eq!(point(&listed), json!({ "lat": lat, "lng": lng, "placed": true }), "placed, then moved");
	}
	assert_eq!(keyed.locate().await.unwrap(), (0, vec![], vec![]));
	assert_eq!(*asked.lock().unwrap(), [ASSIST, FLORE], "a placed place is not asked of Places again");
}

#[tokio::test]
async fn a_point_is_placed_only_on_the_callers_gmb_without_googles_and_on_earth() {
	let (_asked, base) = places().await;
	let (db, cache) = (TestDb::create().await, tempfile::tempdir().unwrap());
	let p = panel(&db, Some(&base), cache.path()).await;
	opened(&p).await;
	let other = seen(&p, "other@localhost").await;
	let theirs = p.add_gmail(other, "other@gmail.com", jiff::Timestamp::now()).await.unwrap().unwrap();
	let theirs = p.add_gmb(Some(other), theirs.id, ASSIST, jiff::Timestamp::now()).await.unwrap().unwrap();
	let app = app(p, None);
	let mut b = Browser::default();
	b.sign_in(&app).await;
	let (_, gmail) = call(&mut b, &app, Method::POST, "/api/v1/gmails", Some(json!({ "gmail": "a@gmail.com" }))).await;
	let mut mine = Vec::new();
	for place in [PROCOPE, ASSIST] {
		let (_, gmb) = call(&mut b, &app, Method::POST, &format!("/api/v1/gmails/{}/gmbs", gmail["id"]), Some(json!({ "place_id": place }))).await;
		mine.push(gmb["id"].as_i64().unwrap());
	}

	let paris = json!({ "lat": 48.86, "lng": 2.35 });
	for (gmb, at, want) in [
		(mine[0], &paris, StatusCode::CONFLICT),
		(theirs.id, &paris, StatusCode::NOT_FOUND),
		(mine[1], &json!({ "lat": 91.0, "lng": 2.35 }), StatusCode::BAD_REQUEST),
		(mine[1], &json!({ "lat": 48.86, "lng": -180.5 }), StatusCode::BAD_REQUEST),
	] {
		let (status, body) = call(&mut b, &app, Method::PUT, &format!("/api/v1/gmbs/{gmb}/location"), Some(at.clone())).await;
		assert_eq!(status, want, "GMB {gmb} at {at}: {body}");
	}
}

#[tokio::test]
async fn the_map_is_told_only_when_configured() {
	let db = TestDb::create().await;
	let cache = tempfile::tempdir().unwrap();
	let pair = MapDto {
		key: "browser-key".into(),
		map_id: "DEMO_MAP_ID".into(),
	};
	for (map, want) in [
		(None, (StatusCode::NOT_FOUND, json!({ "error": "not found" }))),
		(Some(pair), (StatusCode::OK, json!({ "key": "browser-key", "map_id": "DEMO_MAP_ID" }))),
	] {
		let app = app(panel(&db, None, cache.path()).await, map);
		let mut b = Browser::default();
		b.sign_in(&app).await;
		assert_eq!(call(&mut b, &app, Method::GET, "/api/v1/map", None).await, want);
	}
}

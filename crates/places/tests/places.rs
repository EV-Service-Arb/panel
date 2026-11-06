//! What people paste, and the Places API (New) against a stub that counts what it is asked: an
//! answer is paid for once, a refusal never kept, a spent key passed over until its window ends.

use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
	time::{SystemTime, UNIX_EPOCH},
};

use axum::{
	Router,
	extract::{Path, State},
	http::{HeaderMap, StatusCode},
	response::{IntoResponse, Response},
	routing::{get, post},
};
use sa_places::{
	Ask, Error, Google, Keys, LatLng, Spent,
	parse::{Parsed, parse},
};
use serde_json::{Value, json};

/// What an ended trial returns.
const NO_BILLING: &str = r#"{"error":{"code":403,"status":"PERMISSION_DENIED","details":[
	{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"BILLING_DISABLED","domain":"googleapis.com",
		"metadata":{"consumer":"projects/1","service":"places.googleapis.com"}}]}}"#;

const PROCOPE: &str = "ChIJprocopeprocopeproc";

const MOLD_ASSIST: &str = "ChIJmoldassistmoldassi";

const SEARCH: &str = "https://www.google.com/maps/place/Le+Procope/@48.853,2.3389,17z";

type Refusals = HashMap<String, Vec<(StatusCode, String)>>;

/// A spent window as Google spells it, from `start` (unix seconds).
fn refusal(start: u64) -> String {
	json!({"error": {"code": 429, "status": "RESOURCE_EXHAUSTED", "details": [
		{"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "RATE_LIMIT_EXCEEDED", "metadata": {
			"window_start_time": start.to_string(), "quota_limit_value": "100",
			"quota_metric": "places.googleapis.com/SearchTextRequest",
			"quota_limit": "SearchTextRequestPerDayPerProject"}}]}})
	.to_string()
}

fn now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

#[derive(Clone, Default)]
struct Stub {
	/// Each request: its key, field mask, and what it asked.
	got: Arc<Mutex<Vec<(String, String, Value)>>>,
	/// A key's next answers are these refusals.
	refuse: Arc<Mutex<Refusals>>,
}

impl Stub {
	fn asked(&self) -> usize {
		self.got.lock().unwrap().len()
	}

	fn keys(&self) -> Vec<String> {
		self.got.lock().unwrap().iter().map(|(k, _, _)| k.clone()).collect()
	}

	fn refusal(&self, headers: &HeaderMap, asked: Value) -> Option<Response> {
		let key = headers["x-goog-api-key"].to_str().unwrap().to_owned();
		self.got.lock().unwrap().push((key.clone(), headers["x-goog-fieldmask"].to_str().unwrap().to_owned(), asked));
		let mut refuse = self.refuse.lock().unwrap();
		let next = refuse.get_mut(&key).filter(|r| !r.is_empty())?.remove(0);
		Some(next.into_response())
	}
}

fn procope() -> Value {
	json!({ "id": PROCOPE, "displayName": { "text": "Le Procope" }, "formattedAddress": "13 Rue de l'Ancienne Comédie, Paris", "location": { "latitude": 48.853, "longitude": 2.3389 } })
}

/// A service-area business: Places answers it with no address and no point.
fn mold_assist() -> Value {
	json!({ "id": MOLD_ASSIST, "displayName": { "text": "Mold Assist" } })
}

async fn search_text(State(s): State<Stub>, headers: HeaderMap, body: String) -> Response {
	let body: Value = serde_json::from_str(&body).unwrap();
	let query = body["textQuery"].as_str().unwrap().to_owned();
	if let Some(refused) = s.refusal(&headers, body) {
		return refused;
	}
	match query.as_str() {
		"Nowhere" => axum::Json(json!({})).into_response(),
		"Broken" => (StatusCode::FORBIDDEN, "API key not valid").into_response(),
		"Mold Assist" => axum::Json(json!({ "places": [mold_assist()] })).into_response(),
		_ => axum::Json(json!({ "places": [
			procope(),
			{ "id": "ChIJsecondsecondsecond" },
			{ "id": "ChIJnamesakenamesakena", "displayName": { "text": "le procope" }, "formattedAddress": "1 Rue de Lyon, Marseille", "location": { "latitude": 43.3, "longitude": 5.4 } }
		] }))
		.into_response(),
	}
}

async fn place_details(State(s): State<Stub>, headers: HeaderMap, Path(id): Path<String>) -> Response {
	if let Some(refused) = s.refusal(&headers, json!(id)) {
		return refused;
	}
	match id.as_str() {
		PROCOPE | "ChIJmovedmovedmovedmo" => axum::Json(procope()).into_response(),
		MOLD_ASSIST => axum::Json(mold_assist()).into_response(),
		"ChIJotherotherotherot" => axum::Json(json!({ "id": id, "location": { "latitude": 0.5, "longitude": 0.5 } })).into_response(),
		_ => (StatusCode::NOT_FOUND, "not found").into_response(),
	}
}

/// The stub, and a `Google` on it with `keys` and a fresh cache.
async fn google(keys: &[&str]) -> (Stub, Google, tempfile::TempDir, tokio::task::JoinHandle<()>) {
	let s = Stub::default();
	let app = Router::new()
		.route("/v1/places:searchText", post(search_text))
		.route("/v1/places/{id}", get(place_details))
		.with_state(s.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let base = format!("http://{}/v1/places", listener.local_addr().unwrap());
	let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	let cache = tempfile::tempdir().unwrap();
	let google = Google::try_new(Keys::new(keys.iter().map(|k| k.to_string()).collect()), cache.path().join("google_cache"))
		.unwrap()
		.at(base);
	(s, google, cache, server)
}

#[tokio::test]
async fn every_place_bearing_the_name_is_found_and_the_search_is_biased_to_the_map() {
	let (s, google, _cache, server) = google(&["key-123"]).await;
	let got = google.candidates(SEARCH).await.unwrap();
	server.abort();
	let ids: Vec<&str> = got.iter().map(|(id, _)| id.as_str()).collect();
	assert_eq!(ids, [PROCOPE, "ChIJnamesakenamesakena"]);
	assert_eq!(got[0].1.as_ref().unwrap().location.unwrap().lat, 48.853);
	let (key, mask, body) = s.got.lock().unwrap()[0].clone();
	assert_eq!((key.as_str(), mask.as_str()), ("key-123", "places.id,places.displayName,places.formattedAddress,places.location"));
	assert_eq!(body["locationBias"]["circle"]["center"], json!({ "latitude": 48.853, "longitude": 2.3389 }));
}

#[tokio::test]
async fn nothing_or_another_place_found_is_the_callers_mistake_and_a_refusal_is_not() {
	let (_s, google, _cache, server) = google(&["k"]).await;
	let nothing = google.resolve("https://www.google.com/maps/place/Nowhere").await.unwrap_err();
	let refused = google.resolve("https://www.google.com/maps/place/Broken").await.unwrap_err();
	let another = google.resolve("https://www.google.com/maps/place/Le+Procope+of+Hicksville").await.unwrap_err(); // #37: the first hit was a namesake
	let ambiguous = google.resolve(SEARCH).await.unwrap_err();
	server.abort();
	assert!(matches!(nothing, Error::Invalid(_)), "{nothing:#}");
	assert!(matches!(another, Error::Invalid(_)), "{another:#}");
	assert!(matches!(ambiguous, Error::Invalid(_)), "{ambiguous:#}");
	assert!(matches!(refused, Error::Api { status: 403, .. }), "{refused:#}");
}

#[tokio::test]
async fn a_place_added_by_its_id_is_named_and_located_and_an_unknown_id_refused() {
	let (s, google, _cache, server) = google(&["key-123"]).await;
	let (_, got) = google.resolve(PROCOPE).await.unwrap();
	let unknown = google.resolve("ChIJunknownunknownunk").await.unwrap_err();
	let (moved, _) = google.resolve("ChIJmovedmovedmovedmo").await.unwrap();
	server.abort();
	assert_eq!(moved, PROCOPE, "a moved place comes back under the id Google gives it now");
	let got = got.unwrap();
	assert_eq!((got.name.as_deref(), got.address.as_deref()), (Some("Le Procope"), Some("13 Rue de l'Ancienne Comédie, Paris")));
	assert_eq!(got.location, Some(LatLng { lat: 48.853, lng: 2.3389 }));
	assert!(matches!(unknown, Error::Invalid(_)), "{unknown:#}");
	assert_eq!(s.got.lock().unwrap()[0].1, "id,displayName,formattedAddress,location");
}

#[tokio::test]
async fn a_service_area_place_has_no_point() {
	let (_s, google, _cache, server) = google(&["k"]).await;
	let detailed = google.details(MOLD_ASSIST, Ask::Afresh).await.unwrap();
	let (_, searched) = google.resolve("https://www.google.com/maps/place/Mold+Assist").await.unwrap();
	server.abort();
	assert_eq!((detailed.name.as_deref(), detailed.location), (Some("Mold Assist"), None));
	assert_eq!(searched.unwrap().location, None);
}

#[tokio::test]
async fn without_a_key_a_place_id_resolves_unnamed() {
	let (s, google, _cache, server) = google(&[]).await;
	let got = google.candidates(PROCOPE).await.unwrap();
	let search = google.resolve(SEARCH).await.unwrap_err();
	server.abort();
	assert_eq!(got, [(PROCOPE.to_owned(), None)]);
	assert!(matches!(search, Error::Invalid(_)), "{search:#}");
	assert_eq!(s.asked(), 0);
}

#[tokio::test]
async fn an_answer_is_paid_for_once() {
	let (s, google, _cache, server) = google(&["k"]).await;
	google.details(PROCOPE, Ask::Cached).await.unwrap();
	google.details(PROCOPE, Ask::Cached).await.unwrap();
	assert_eq!(s.asked(), 1, "the same details twice");

	google.candidates(SEARCH).await.unwrap();
	let picked = google.details("ChIJnamesakenamesakena", Ask::Cached).await.unwrap();
	google.candidates(SEARCH).await.unwrap();
	assert_eq!(s.asked(), 2, "a search, then details of a place it returned, then the search again");
	assert_eq!(picked.address.as_deref(), Some("1 Rue de Lyon, Marseille"));

	google.details(PROCOPE, Ask::Afresh).await.unwrap();
	server.abort();
	assert_eq!(s.asked(), 3, "asked afresh");
}

#[tokio::test]
async fn a_refusal_is_never_kept() {
	let (s, google, _cache, server) = google(&["k"]).await;
	for _ in 0..2 {
		google.resolve("https://www.google.com/maps/place/Broken").await.unwrap_err();
		google.details("ChIJunknownunknownunk", Ask::Cached).await.unwrap_err();
	}
	server.abort();
	assert_eq!(s.asked(), 4);
}

#[tokio::test]
async fn a_spent_key_sits_out_until_its_window_ends() {
	let (s, google, _cache, server) = google(&["free", "billed"]).await;
	// the free key's day ends in 2 s
	s.refuse.lock().unwrap().insert("free".into(), vec![(StatusCode::TOO_MANY_REQUESTS, refusal(now() - 86_400 + 2))]);
	google.details(PROCOPE, Ask::Cached).await.unwrap();
	google.details("ChIJotherotherotherot", Ask::Cached).await.unwrap();
	assert_eq!(s.keys(), ["free", "billed", "billed"], "refused, the next key is asked, and asked again while the first sits out");

	tokio::time::sleep(std::time::Duration::from_secs(3)).await;
	google.details(PROCOPE, Ask::Afresh).await.unwrap();
	assert_eq!(s.keys().last().unwrap(), "free", "back once its window ended");

	s.refuse.lock().unwrap().extend([
		("free".into(), vec![(StatusCode::FORBIDDEN, NO_BILLING.to_owned())]),
		("billed".into(), vec![(StatusCode::TOO_MANY_REQUESTS, refusal(now()))]),
	]);
	let spent = google.details(PROCOPE, Ask::Afresh).await.unwrap_err();
	server.abort();
	let Error::Exhausted(why) = spent else { panic!("{spent:#}") };
	assert!(matches!(why[..], [Spent::Billing, Spent::Quota { .. }]), "{why:?}");
}

/// Decoding must turn `%2B` into `+` once, and leave it so.
#[test]
fn a_plus_encoded_in_a_maps_url_stays_in_the_search_query() {
	let got = parse("https://www.google.com/maps/place/C%2B%2B+Bar/@48.85,2.33,17z").unwrap();
	assert_eq!(
		got,
		Parsed::Search {
			query: "C++ Bar".into(),
			near: Some((48.85, 2.33)),
		}
	);
}

/// become that place's id: a search by name took Shiny Maids Queens for Shiny Maids of Hicksville.
#[test]
fn a_maps_url_names_its_place_by_the_feature_id() {
	let got = parse(
		"https://www.google.com/maps/place/Shiny+Maids+House+Cleaning+Services+of+Hicksville/@40.7837389,-73.5036176,17z/data=!3m1!4b1!4m6!3m5!1s0x89c281e03866d8f3:0xa74f91a21ce5b00e!8m2!3d40.7837389!4d-73.5036176!16s%2Fg%2F11zy58lwft?entry=tts",
	)
	.unwrap();
	assert_eq!(got, Parsed::PlaceId("ChIJ89hmOOCBwokRDrDlHKKRT6c".into()));
}

/// that name is what Places is searched for.
#[test]
fn a_google_search_url_searches_for_its_query() {
	let got = parse("https://www.google.com/search?hl=en&gl=US&output=search&kgmid=/g/11zgy_6kp_&q=Bayou+Buff+Mobile+Car+Detailing+Houston+TX&shndl=30").unwrap();
	assert_eq!(
		got,
		Parsed::Search {
			query: "Bayou Buff Mobile Car Detailing Houston TX".into(),
			near: None,
		}
	);
}

/// the place's cid; the id built from it is one Places answers with the place's own.
#[test]
fn a_cid_names_its_place_by_an_alias() {
	let viewer = parse("https://www.google.com/searchviewer/10?output=search&svid=CAwSKBImCgNwdnESH09oVXdlREE2TUhoalpXVm1PVFEwWVdWalkySXhNbVkYCg&hl=en&gl=US").unwrap();
	let maps = parse("https://maps.google.com/?cid=931956246298013999").unwrap();
	assert_eq!(viewer, Parsed::Alias("ChIJAAAAAAAAAAARL7HMrkT57gw".into()));
	assert_eq!(maps, viewer);
}

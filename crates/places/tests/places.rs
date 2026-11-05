//! What people paste, and Places API (New) against a stub: the text search a URL without a place
//! id takes, and the name of a place added by its id.

use std::sync::{Arc, Mutex};

use axum::{
	Json, Router,
	extract::{Path, State},
	http::{HeaderMap, StatusCode},
	response::{IntoResponse, Response},
	routing::{get, post},
};
use sa_places::{
	Error, details,
	parse::{Parsed, parse},
	search,
};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Stub {
	got: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

async fn search_text(State(s): State<Stub>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
	let query = body["textQuery"].as_str().unwrap_or_default().to_owned();
	s.got.lock().unwrap().push((headers, body));
	if query == "Nowhere" {
		return Json(json!({})).into_response();
	}
	if query == "Broken" {
		return (StatusCode::FORBIDDEN, "API key not valid").into_response();
	}
	Json(json!({ "places": [
		{ "id": "ChIJprocopeprocopeproc", "displayName": { "text": "Le Procope" }, "formattedAddress": "13 Rue de l'Ancienne Comédie, Paris" },
		{ "id": "ChIJsecondsecondsecond" },
		{ "id": "ChIJnamesakenamesakena", "displayName": { "text": "le procope" }, "formattedAddress": "1 Rue de Lyon, Marseille" }
	] }))
	.into_response()
}

async fn place_details(State(s): State<Stub>, headers: HeaderMap, Path(id): Path<String>) -> Response {
	s.got.lock().unwrap().push((headers, json!(id)));
	match id.as_str() {
		"ChIJprocopeprocopeproc" | "ChIJmovedmovedmovedmo" =>
			Json(json!({ "id": "ChIJprocopeprocopeproc", "displayName": { "text": "Le Procope" }, "formattedAddress": "13 Rue de l'Ancienne Comédie, Paris" })).into_response(),
		_ => (StatusCode::NOT_FOUND, "not found").into_response(),
	}
}

/// The stub and its base URL (`…/v1/places`).
async fn stub() -> (Stub, String, tokio::task::JoinHandle<()>) {
	let s = Stub::default();
	let app = Router::new()
		.route("/v1/places:searchText", post(search_text))
		.route("/v1/places/{id}", get(place_details))
		.with_state(s.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("http://{}/v1/places", listener.local_addr().unwrap());
	let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(s, url, server)
}

#[tokio::test]
async fn every_place_bearing_the_name_is_found_and_the_search_is_biased_to_the_map() {
	let (s, url, server) = stub().await;
	let got = search(&reqwest::Client::new(), &format!("{url}:searchText"), "key-123", "Le Procope", Some((48.853, 2.3389)))
		.await
		.unwrap();
	server.abort();
	let ids: Vec<&str> = got.iter().map(|p| p.place_id.as_str()).collect();
	assert_eq!(ids, ["ChIJprocopeprocopeproc", "ChIJnamesakenamesakena"]);
	let (headers, body) = s.got.lock().unwrap()[0].clone();
	assert_eq!(headers["x-goog-api-key"], "key-123");
	assert_eq!(headers["x-goog-fieldmask"], "places.id,places.displayName,places.formattedAddress");
	assert_eq!(body["locationBias"]["circle"]["center"], json!({ "latitude": 48.853, "longitude": 2.3389 }));
}

#[tokio::test]
async fn nothing_or_another_place_found_is_the_callers_mistake_and_a_refusal_is_not() {
	let (_s, url, server) = stub().await;
	let http = reqwest::Client::new();
	let url = format!("{url}:searchText");
	let nothing = search(&http, &url, "k", "Nowhere", None).await.unwrap_err();
	let refused = search(&http, &url, "k", "Broken", None).await.unwrap_err();
	let another = search(&http, &url, "k", "Le Procope of Hicksville", None).await.unwrap_err(); // #37: the first hit was a namesake
	server.abort();
	assert!(matches!(nothing, Error::Invalid(_)), "{nothing:#}");
	assert!(matches!(another, Error::Invalid(_)), "{another:#}");
	assert!(matches!(refused, Error::Api { status: 403, .. }), "{refused:#}");
}

#[tokio::test]
async fn a_place_added_by_its_id_is_named_and_an_unknown_id_refused() {
	let (s, url, server) = stub().await;
	let http = reqwest::Client::new();
	let got = details(&http, &url, "key-123", "ChIJprocopeprocopeproc").await.unwrap();
	let unknown = details(&http, &url, "key-123", "ChIJunknownunknownunk").await.unwrap_err();
	let moved = details(&http, &url, "key-123", "ChIJmovedmovedmovedmo").await.unwrap();
	server.abort();
	assert_eq!(moved.place_id, "ChIJprocopeprocopeproc", "a moved place comes back under the id Google gives it now");
	assert_eq!((got.name.as_deref(), got.address.as_deref()), (Some("Le Procope"), Some("13 Rue de l'Ancienne Comédie, Paris")));
	assert!(matches!(unknown, Error::Invalid(_)), "{unknown:#}");
	let (headers, _) = s.got.lock().unwrap()[0].clone();
	assert_eq!(
		(&headers["x-goog-api-key"], &headers["x-goog-fieldmask"]),
		(&"key-123".parse().unwrap(), &"id,displayName,formattedAddress".parse().unwrap())
	);
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

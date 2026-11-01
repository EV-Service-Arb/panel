//! A place id from what a person pastes. The id, or a Maps URL carrying it, needs nothing;
//! a URL with only a name in it is looked up with the Places API (New).

pub mod parse;

use parse::Parsed;
use serde::{Deserialize, Serialize};

/// A Google place.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Place {
	pub place_id: String,
	/// Its display name.
	pub name: Option<String>,
	pub address: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
	/// What the person pasted names no one place: theirs to fix.
	#[error("{0}")]
	Invalid(String),
	/// The Places API refused a request.
	#[error("Places API, {asked}: {status}: {body}")]
	Api { asked: String, status: u16, body: String },
	/// The request itself failed, or its answer did not decode.
	#[error(transparent)]
	Http(#[from] reqwest::Error),
}

/// Places API (New) text search.
pub const SEARCH_TEXT: &str = "https://places.googleapis.com/v1/places:searchText";
/// Places API (New) place details, as `{PLACES}/{place_id}`.
pub const PLACES: &str = "https://places.googleapis.com/v1/places";
const FIELD_MASK: &str = "places.id,places.displayName,places.formattedAddress";

/// The place id in `input`, and the place when a search found it: one place only, an
/// ambiguous name is refused with what it may mean. `key` is the Places API key, needed
/// only for a search.
pub async fn resolve(http: &reqwest::Client, key: Option<&str>, input: &str) -> Result<(String, Option<Place>), Error> {
	match parsed(input).await? {
		Parsed::PlaceId(id) => Ok((id, None)),
		Parsed::Alias(alias) => {
			let place = details(http, PLACES, needed(key)?, &alias).await?;
			Ok((place.place_id.clone(), Some(place)))
		}
		Parsed::Search { query, near } => match <[Place; 1]>::try_from(search(http, SEARCH_TEXT, needed(key)?, &query, near).await?) {
			Ok([one]) => Ok((one.place_id.clone(), Some(one))),
			Err(many) => {
				let each = many
					.iter()
					.map(|p| [Some(p.place_id.as_str()), p.address.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(", "));
				Err(Error::Invalid(format!(
					"{} places are named {query:?}; add one by its id: {}",
					many.len(),
					each.collect::<Vec<_>>().join("; ")
				)))
			}
		},
	}
}

/// Every place `input` may mean: the one its id names, else each Places hit bearing the
/// name in the URL. Named and addressed when `key` is set.
pub async fn candidates(http: &reqwest::Client, key: Option<&str>, input: &str) -> Result<Vec<Place>, Error> {
	match parsed(input).await? {
		Parsed::PlaceId(id) => Ok(vec![match key {
			Some(key) => details(http, PLACES, key, &id).await?,
			None => Place {
				place_id: id,
				name: None,
				address: None,
			},
		}]),
		Parsed::Alias(alias) => Ok(vec![details(http, PLACES, needed(key)?, &alias).await?]),
		Parsed::Search { query, near } => search(http, SEARCH_TEXT, needed(key)?, &query, near).await,
	}
}

fn needed(key: Option<&str>) -> Result<&str, Error> {
	key.ok_or_else(|| Error::Invalid("the URL has no place id; resolving it needs GOOGLE_MAPS_KEY".into()))
}

async fn parsed(input: &str) -> Result<Parsed, Error> {
	let mut expanded = input.trim().to_owned();
	for _ in 0..3 {
		if !["https://maps.app.goo.gl/", "https://share.google/", "https://www.google.com/share.google?"]
			.iter()
			.any(|p| expanded.starts_with(p))
		{
			break;
		}
		expanded = expand_short_link(&expanded).await?;
	}
	parse::parse(&expanded).map_err(|e| Error::Invalid(format!("{e:#}")))
}

/// One hop: following further can land on consent.google.com instead of the place.
async fn expand_short_link(url: &str) -> Result<String, Error> {
	let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
	let resp = client.get(url).send().await?;
	let location = resp
		.headers()
		.get(reqwest::header::LOCATION)
		.ok_or_else(|| Error::Invalid(format!("{url:?} answered {} without a redirect", resp.status())))?;
	Ok(location
		.to_str()
		.map_err(|e| Error::Invalid(format!("{url:?} redirected to a non-UTF-8 location: {e}")))?
		.to_owned())
}

/// Places API (New) text search: every hit bearing the name searched for, best first.
pub async fn search(http: &reqwest::Client, endpoint: &str, key: &str, query: &str, near: Option<(f64, f64)>) -> Result<Vec<Place>, Error> {
	#[derive(Deserialize)]
	struct Resp {
		#[serde(default)]
		places: Vec<Hit>,
	}
	#[derive(Deserialize)]
	#[serde(rename_all = "camelCase")]
	struct Hit {
		id: String,
		display_name: Option<Text>,
		formatted_address: Option<String>,
	}
	#[derive(Deserialize)]
	struct Text {
		text: String,
	}

	let mut body = serde_json::json!({ "textQuery": query, "pageSize": 20 });
	if let Some((lat, lng)) = near {
		body["locationBias"] = serde_json::json!({ "circle": { "center": { "latitude": lat, "longitude": lng }, "radius": 500.0 } });
	}
	let resp = http
		.post(endpoint)
		.header("X-Goog-Api-Key", key)
		.header("X-Goog-FieldMask", FIELD_MASK)
		.json(&body)
		.send()
		.await?;
	let status = resp.status();
	if !status.is_success() {
		let body = resp.text().await?;
		return Err(Error::Api {
			asked: format!("text search for {query:?}"),
			status: status.as_u16(),
			body,
		});
	}
	let resp: Resp = resp.json().await?;
	let named: Vec<Place> = resp
		.places
		.into_iter()
		.filter(|p| p.display_name.as_ref().is_some_and(|t| t.text.eq_ignore_ascii_case(query)))
		.map(|p| Place {
			place_id: p.id,
			name: p.display_name.map(|t| t.text),
			address: p.formatted_address,
		})
		.collect();
	if named.is_empty() {
		return Err(Error::Invalid(format!("Places found no place named {query:?}; paste a URL with its id")));
	}
	Ok(named)
}

/// The place with this id, from Places API (New) details, under the id Google now gives it:
/// a moved or merged place answers with its newer one.
pub async fn details(http: &reqwest::Client, endpoint: &str, key: &str, place_id: &str) -> Result<Place, Error> {
	#[derive(Deserialize)]
	#[serde(rename_all = "camelCase")]
	struct Hit {
		id: String,
		display_name: Option<Text>,
		formatted_address: Option<String>,
	}
	#[derive(Deserialize)]
	struct Text {
		text: String,
	}

	let resp = http
		.get(format!("{endpoint}/{place_id}"))
		.header("X-Goog-Api-Key", key)
		.header("X-Goog-FieldMask", "id,displayName,formattedAddress")
		.send()
		.await?;
	let status = resp.status();
	if matches!(status, reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::BAD_REQUEST) {
		return Err(Error::Invalid(format!("Places knows no place {place_id:?}")));
	}
	if !status.is_success() {
		let body = resp.text().await?;
		return Err(Error::Api {
			asked: format!("details of {place_id}"),
			status: status.as_u16(),
			body,
		});
	}
	let place: Hit = resp.json().await?;
	Ok(Place {
		place_id: place.id,
		name: place.display_name.map(|t| t.text),
		address: place.formatted_address,
	})
}

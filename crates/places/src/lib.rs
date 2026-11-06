//! A Google place from what a person pastes. The id, or a Maps URL carrying it, needs nothing;
//! a URL with only a name in it is looked up with the Places API (New). Every Places request
//! goes through [`Google`]: its cache answers what was paid for once, its [`Keys`] rotate.

mod cache;
mod keys;
pub mod parse;

use std::{path::PathBuf, time::Duration};

use cache::{Cache, Request};
pub use keys::{Keys, Spent};
use parse::Parsed;
use serde::{Deserialize, Serialize};

const SEARCH_MASK: &str = "places.id,places.displayName,places.formattedAddress,places.location";

const DETAILS_MASK: &str = "id,displayName,formattedAddress,location";
/// A GMB created on Google today is findable by name tomorrow.
const SEARCH_AGE: Duration = Duration::from_secs(86_400);
/// A Google place, as Places answers for it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Place {
	pub place_id: String,
	/// Its display name.
	pub name: Option<String>,
	pub address: Option<String>,
	/// None for a service-area business: Google shows it no point.
	pub location: Option<LatLng>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct LatLng {
	pub lat: f64,
	pub lng: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
	/// What the person pasted names no one place: theirs to fix.
	#[error("{0}")]
	Invalid(String),
	/// The Places API refused a request.
	#[error("Places API, {asked}: {status}: {body}")]
	Api { asked: String, status: u16, body: String },
	/// Every key is sitting out.
	#[error("every Places key is spent: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
	Exhausted(Vec<Spent>),
	/// The request itself failed.
	#[error(transparent)]
	Http(#[from] reqwest::Error),
	/// An answer, paid or cached, is not the shape asked for.
	#[error("decoding a Places answer")]
	Decode(#[from] serde_json::Error),
	#[error("the Places cache, at {}", .0.display())]
	Cache(PathBuf, #[source] std::io::Error),
}

/// Whether a cached answer will do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ask {
	Cached,
	/// Asked again, and the cache overwritten.
	Afresh,
}

/// The Places API (New), through the cache and the keys.
#[derive(Debug)]
pub struct Google {
	http: reqwest::Client,
	keys: Keys,
	cache: Cache,
	/// `…/v1/places`.
	base: String,
}

impl Google {
	/// With no keys, a place id still resolves, unnamed and unlocated.
	pub fn try_new(keys: Keys, cache_dir: PathBuf) -> Result<Self, Error> {
		let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(10)).build()?;
		Ok(Self {
			http,
			keys,
			cache: Cache(cache_dir),
			base: "https://places.googleapis.com/v1/places".to_owned(),
		})
	}

	/// Places at `base` (`…/v1/places`) in place of Google's own: a stub's.
	pub fn at(self, base: impl Into<String>) -> Self {
		Self { base: base.into(), ..self }
	}

	/// The place id in `input`, and the place unless there is no key: one place only, an
	/// ambiguous name is refused with what it may mean.
	pub async fn resolve(&self, input: &str) -> Result<(String, Option<Place>), Error> {
		match parsed(input).await? {
			Parsed::PlaceId(id) if self.keys.is_empty() => Ok((id, None)),
			Parsed::PlaceId(id) | Parsed::Alias(id) => {
				let place = self.details(&id, Ask::Cached).await?;
				Ok((place.place_id.clone(), Some(place)))
			}
			Parsed::Search { query, near } => match <[Place; 1]>::try_from(self.search(&query, near).await?) {
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

	/// Every place `input` may mean, each as [`Self::resolve`] gives one: the one its id names,
	/// else each Places hit bearing the name in the URL.
	pub async fn candidates(&self, input: &str) -> Result<Vec<(String, Option<Place>)>, Error> {
		let places = match parsed(input).await? {
			Parsed::PlaceId(id) if self.keys.is_empty() => return Ok(vec![(id, None)]),
			Parsed::PlaceId(id) | Parsed::Alias(id) => vec![self.details(&id, Ask::Cached).await?],
			Parsed::Search { query, near } => self.search(&query, near).await?,
		};
		Ok(places.into_iter().map(|p| (p.place_id.clone(), Some(p))).collect())
	}

	/// The place with this id, under the id Google now gives it: a moved or merged place answers
	/// with its newer one.
	pub async fn details(&self, place_id: &str, ask: Ask) -> Result<Place, Error> {
		let url = format!("{}/{place_id}", self.base);
		let (json, _) = match self.ask(&Request::details(&url), None, ask).await {
			Err(Error::Api { status: 400 | 404, .. }) => return Err(Error::Invalid(format!("Places knows no place {place_id:?}"))),
			r => r?,
		};
		Ok(serde_json::from_value::<Hit>(json)?.into())
	}

	/// Text search: every hit bearing the name searched for, best first. Each place it pays for is
	/// cached as that place's details.
	async fn search(&self, query: &str, near: Option<(f64, f64)>) -> Result<Vec<Place>, Error> {
		#[derive(Deserialize)]
		struct Resp {
			#[serde(default)]
			places: Vec<serde_json::Value>,
		}

		let mut body = serde_json::json!({ "textQuery": query, "pageSize": 20 });
		if let Some((lat, lng)) = near {
			body["locationBias"] = serde_json::json!({ "circle": { "center": { "latitude": lat, "longitude": lng }, "radius": 500.0 } });
		}
		let (url, body) = (format!("{}:searchText", self.base), body.to_string());
		let req = Request {
			method: "POST",
			url: &url,
			mask: SEARCH_MASK,
			body: &body,
		};
		let (json, paid) = self.ask(&req, Some(SEARCH_AGE), Ask::Cached).await?;
		let mut named = Vec::new();
		for raw in serde_json::from_value::<Resp>(json)?.places {
			let place = Place::from(serde_json::from_value::<Hit>(raw.clone())?);
			if paid {
				self.cache.write(&Request::details(&format!("{}/{}", self.base, place.place_id)), &raw.to_string()).await?;
			}
			if place.name.as_ref().is_some_and(|n| n.eq_ignore_ascii_case(query)) {
				named.push(place);
			}
		}
		if named.is_empty() {
			return Err(Error::Invalid(format!("Places found no place named {query:?}; paste a URL with its id")));
		}
		Ok(named)
	}

	/// The answer to `req`, and whether it was paid for now: from the cache unless older than
	/// `max_age`, else from the first key not sitting out.
	async fn ask(&self, req: &Request<'_>, max_age: Option<Duration>, ask: Ask) -> Result<(serde_json::Value, bool), Error> {
		if ask == Ask::Cached
			&& let Some(hit) = self.cache.read(req, max_age).await?
		{
			return Ok((hit, false));
		}
		let mut tried = Vec::new();
		loop {
			let key = match self.keys.next(&tried) {
				Ok(k) => k,
				Err(spent) if spent.is_empty() => return Err(Error::Invalid("asking Google Places needs an API key, and none is set".into())),
				Err(spent) => return Err(Error::Exhausted(spent)),
			};
			let r = match req.method {
				"POST" => self.http.post(req.url).header(reqwest::header::CONTENT_TYPE, "application/json").body(req.body.to_owned()),
				"GET" => self.http.get(req.url),
				other => unreachable!("Places is asked by GET and POST only, not {other}"),
			};
			let resp = r.header("X-Goog-Api-Key", key.as_str()).header("X-Goog-FieldMask", req.mask).send().await?;
			let status = resp.status();
			tracing::info!(method = req.method, url = req.url, %status, "Places asked");
			let text = resp.text().await?;
			if status.is_success() {
				return Ok((self.cache.write(req, &text).await?, true));
			}
			match Spent::of(&text) {
				Some(why) => {
					self.keys.sit_out(&key, why);
					tried.push(key);
				}
				None => {
					return Err(Error::Api {
						asked: format!("{} {}", req.method, req.url),
						status: status.as_u16(),
						body: text,
					});
				}
			}
		}
	}
}

impl<'a> Request<'a> {
	fn details(url: &'a str) -> Self {
		Self {
			method: "GET",
			url,
			mask: DETAILS_MASK,
			body: "",
		}
	}
}

/// A place as Places spells it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hit {
	id: String,
	display_name: Option<Text>,
	formatted_address: Option<String>,
	location: Option<Location>,
}

#[derive(Deserialize)]
struct Text {
	text: String,
}

#[derive(Deserialize)]
struct Location {
	latitude: f64,
	longitude: f64,
}

impl From<Hit> for Place {
	fn from(h: Hit) -> Self {
		Self {
			place_id: h.id,
			name: h.display_name.map(|t| t.text),
			address: h.formatted_address,
			location: h.location.map(|l| LatLng { lat: l.latitude, lng: l.longitude }),
		}
	}
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

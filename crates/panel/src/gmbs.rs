//! The gmails a person manages GMBs with, and those GMBs. The ids are the conductor's, and every
//! service stores them, so nothing is deleted: a gmail or a GMB is removed, and a removed gmail's
//! GMBs count as removed. review_archive hears of every change through the snapshot
//! ([`crate::tokens`]).

use eyre::WrapErr;
use jiff::Timestamp;
use sa_places::Place;
use uuid::Uuid;
use zeroize::Zeroizing;

pub use crate::store::gmbs::{Gmail, Gmb};
use crate::{Panel, Refused, store::gmbs as db, tokens::is_open};

/// The Places API, as GMBs are named and addressed: without a key, only place ids resolve.
pub struct Places {
	http: reqwest::Client,
	key: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for Places {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Places").field("key", &self.key.as_ref().map(|_| "<redacted>")).finish()
	}
}

impl Places {
	pub fn new(key: Option<String>) -> eyre::Result<Self> {
		let http = reqwest::Client::builder()
			.connect_timeout(std::time::Duration::from_secs(5))
			.timeout(std::time::Duration::from_secs(10))
			.build()
			.wrap_err("building the Places API client")?;
		Ok(Self { http, key: key.map(Zeroizing::new) })
	}

	fn key(&self) -> Option<&str> {
		self.key.as_deref().map(String::as_str)
	}
}

fn places_error(e: sa_places::Error) -> eyre::Result<Refused> {
	match e {
		sa_places::Error::Invalid(why) => Ok(Refused::Invalid(why)),
		e => Err(eyre::eyre!(e).wrap_err("asking the Places API")),
	}
}

/// A gmail as a person types it: trimmed, lowercased, no space, one `@` between two parts.
fn address(raw: &str) -> Result<String, Refused> {
	let gmail = raw.trim().to_lowercase();
	match gmail.split_once('@') {
		Some((user, domain)) if !user.is_empty() && !domain.is_empty() && !domain.contains('@') && !gmail.contains(char::is_whitespace) => Ok(gmail),
		_ => Err(Refused::Invalid(format!("{raw:?} is not an email address"))),
	}
}

impl Panel {
	/// The owner's gmails, each with its GMBs; the removed left out.
	pub async fn gmails(&self, owner: Uuid) -> eyre::Result<Vec<(Gmail, Vec<Gmb>)>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for gmails")?;
		let gmails = db::gmails(&mut conn, owner).await?;
		let mut gmbs = db::gmbs(&mut conn, owner).await?;
		Ok(gmails
			.into_iter()
			.map(|g| {
				let (mine, rest) = std::mem::take(&mut gmbs).into_iter().partition(|b| b.gmail_id == g.id);
				gmbs = rest;
				(g, mine)
			})
			.collect())
	}

	pub async fn add_gmail(&self, owner: Uuid, raw: &str, now: Timestamp) -> eyre::Result<Result<Gmail, Refused>> {
		let gmail = match address(raw) {
			Ok(g) => g,
			Err(r) => return Ok(Err(r)),
		};
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		let Some(added) = db::add_gmail(&mut tx, None, owner, &gmail, now).await? else {
			return Ok(Err(Refused::Conflict(format!("{gmail} is one of your gmails already"))));
		};
		tx.commit().await.wrap_err("committing a gmail")?;
		self.snapshot_due.notify_one();
		Ok(Ok(added))
	}

	pub async fn remove_gmail(&self, owner: Uuid, id: i64, now: Timestamp) -> eyre::Result<Result<(), Refused>> {
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		if !db::remove_gmail(&mut tx, owner, id, now).await? {
			return Ok(Err(Refused::NotFound));
		}
		tx.commit().await.wrap_err("committing a gmail's removal")?;
		self.snapshot_due.notify_one();
		Ok(Ok(()))
	}

	/// Every place `input` may mean (see [`sa_places::candidates`]).
	pub async fn place_candidates(&self, input: &str) -> eyre::Result<Result<Vec<Place>, Refused>> {
		match sa_places::candidates(&self.places.http, self.places.key(), input).await {
			Ok(places) => Ok(Ok(places)),
			Err(e) => Ok(Err(places_error(e)?)),
		}
	}

	/// The place `input` names, as a GMB under `gmail_id`: named and addressed by the Places API
	/// when there is a key. `owner` is whose the gmail must be; `None`, the operator's CLI.
	pub async fn add_gmb(&self, owner: Option<Uuid>, gmail_id: i64, input: &str, now: Timestamp) -> eyre::Result<Result<Gmb, Refused>> {
		let (http, key) = (&self.places.http, self.places.key());
		let place = match sa_places::resolve(http, key, input).await {
			Ok((_, Some(place))) => place,
			Ok((place_id, None)) => match key {
				Some(key) => match sa_places::details(http, sa_places::PLACES, key, &place_id).await {
					Ok(place) => place,
					Err(e) => return Ok(Err(places_error(e)?)),
				},
				None => Place {
					place_id,
					name: None,
					address: None,
				},
			},
			Err(e) => return Ok(Err(places_error(e)?)),
		};
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		match db::gmail_owner(&mut tx, gmail_id).await? {
			Some(o) if owner.is_none_or(|owner| owner == o) => {}
			_ => return Ok(Err(Refused::NotFound)),
		}
		let Some(added) = db::add_gmb(&mut tx, None, gmail_id, &place, now).await? else {
			return Ok(Err(Refused::Conflict(format!("gmail {gmail_id} has place {} already", place.place_id))));
		};
		tx.commit().await.wrap_err("committing a GMB")?;
		self.snapshot_due.notify_one();
		Ok(Ok(added))
	}

	pub async fn remove_gmb(&self, owner: Uuid, id: i64, now: Timestamp) -> eyre::Result<Result<(), Refused>> {
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		if !db::remove_gmb(&mut tx, owner, id, now).await? {
			return Ok(Err(Refused::NotFound));
		}
		tx.commit().await.wrap_err("committing a GMB's removal")?;
		self.snapshot_due.notify_one();
		Ok(Ok(()))
	}
}

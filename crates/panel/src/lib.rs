//! The conductor's engine: who a person is (sessions through concierge), who a machine is (the
//! source keys and their HMAC), what users asked for, and the hubs' state.
//!
//! [`Panel`] is the facade the server talks to; [`store`] is SQLite; [`seal`] encrypts what must
//! not sit in the clear. [`session`] is signing in through concierge and the sessions that
//! follow; [`access`] the access requests; [`gmbs`] people's gmails and the GMBs they manage;
//! [`tokens`] their balances and review_archive's charges, and [`import`] both from there;
//! [`alerts`] the hub: users' destinations and rules, the feeds fanned out to them; [`telegram`]
//! the method that reaches them; [`live`] the bus that tells the server's sockets what changed.

pub mod access;
pub mod alerts;
pub mod gmbs;
pub mod import;
pub mod live;
pub mod seal;
pub mod session;
pub mod store;
pub mod telegram;
#[cfg(feature = "testing")]
pub mod testing;
pub mod tokens;

use std::{collections::BTreeSet, sync::Arc};

use eyre::WrapErr;
use jiff::Timestamp;
use panel_core::{
	ids::{BrandId, SiteOrigin},
	signature::{self, SignatureError},
	tokens::Renewal,
};
use sa_auth::{SourceKind, Sources};
use zeroize::Zeroizing;

pub use crate::store::sources::Grant;
use crate::{
	gmbs::Places,
	seal::{DataKey, source_secret_aad},
	store::Store,
};

/// The [`IngestError::Unauthorized`] of a delivery outside the replay window: the only one a
/// caller is told apart. It is checked before the key, and says only that the caller's clock
/// and ours disagree.
pub const STALE: &str = "stale or malformed timestamp";

/// Why a signed request was refused.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
	/// No such key, a revoked one, a bad signature, or a delivery outside the window. The
	/// caller is told only that it was refused; the log says which.
	#[error("unauthorized: {0}")]
	Unauthorized(&'static str),
	#[error(transparent)]
	Internal(#[from] eyre::Report),
}

/// A signed request, as it arrived: for a `GET`, `body` is what the server signs in its place.
#[derive(Clone, Copy, Debug)]
pub struct SignedBatch<'a> {
	pub key_id: &'a str,
	pub timestamp: &'a str,
	pub signature: &'a str,
	pub body: &'a [u8],
}

/// Why a person's request about their gmails, GMBs or tokens is refused.
#[derive(Debug, thiserror::Error)]
pub enum Refused {
	/// The caller's mistake: the message is what they are told.
	#[error("{0}")]
	Invalid(String),
	#[error("not found")]
	NotFound,
	#[error("{0}")]
	Conflict(String),
	/// Before the import from review_archive (see [`import`]).
	#[error("tokens and GMBs open after the import from review_archive")]
	Closed,
}

/// A new source's credentials; the secret is shown once and never again.
pub struct NewSource {
	pub key_id: String,
	pub secret: Zeroizing<String>,
}

impl std::fmt::Debug for NewSource {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("NewSource").field("key_id", &self.key_id).field("secret", &"<redacted>").finish()
	}
}

/// The sources' keys changed: no brand, no single subject a screen would refetch by.
fn sources_changed() -> live::Change {
	live::Change {
		topic: "sources".into(),
		brand: None,
		id: None,
		at: Timestamp::now(),
		audience: live::Audience::Holders(Sources::Manage.as_str().into()),
	}
}

/// The engine.
#[derive(Clone, Debug)]
pub struct Panel {
	store: Store,
	key: Arc<DataKey>,
	rotations: Arc<session::Rotations>,
	live: live::Bus,
	renewal: Renewal,
	places: Arc<Places>,
	/// Told of every change review_archive's snapshot carries.
	snapshot_due: Arc<tokio::sync::Notify>,
}

impl Panel {
	pub fn new(store: Store, key: DataKey, renewal: Renewal, places: Places) -> Self {
		Self {
			store,
			key: Arc::new(key),
			rotations: Arc::default(),
			live: live::Bus::default(),
			renewal,
			places: Arc::new(places),
			snapshot_due: Arc::default(),
		}
	}

	/// This panel on `bus` instead of its own: a bus of another capacity, or one shared.
	pub fn with_live(mut self, bus: live::Bus) -> Self {
		self.live = bus;
		self
	}

	pub fn store(&self) -> &Store {
		&self.store
	}

	/// What changed: see [`live`].
	pub fn bus(&self) -> &live::Bus {
		&self.live
	}

	/// Registers a source that may write events of `kind` for `brands`, with a fresh
	/// secret; a site's with its `origin`. `None` when the key id is taken.
	pub async fn add_source(&self, key_id: &str, kind: SourceKind, brands: BTreeSet<BrandId>, origin: Option<SiteOrigin>) -> eyre::Result<Option<NewSource>> {
		eyre::ensure!(panel_core::ids::is_slug(key_id), "a key id is a lowercase slug of 1–64 of [a-z0-9_-]");
		eyre::ensure!(!brands.is_empty(), "a source writes for at least one brand");
		eyre::ensure!(kind.keyed(), "a key of kind {kind} is not issued: it is written without one");
		eyre::ensure!(origin.is_some() == (kind == SourceKind::Site), "a site key has an origin, and no other kind has one");
		let secret = Zeroizing::new(DataKey::generate_hex()?);
		let sealed = self.key.seal(&source_secret_aad(key_id), secret.as_bytes())?;
		let grant = Grant {
			key_id: key_id.to_owned(),
			kind,
			brands,
		};
		let added = self.store.insert_source(&grant, origin.as_ref(), &sealed, &self.key.fingerprint()).await?;
		if added {
			self.live.changed(sources_changed());
		}
		Ok(added.then(|| NewSource { key_id: key_id.to_owned(), secret }))
	}

	/// Gives a site key issued before origins its origin, once; the same origin again is no change.
	pub async fn set_site_origin(&self, key_id: &str, origin: &SiteOrigin) -> eyre::Result<()> {
		eyre::ensure!(self.store.set_site_origin(key_id, origin).await?, "no active site key {key_id} without another origin");
		self.live.changed(sources_changed());
		Ok(())
	}

	/// Revokes a source's key; `false` when there was no such key, or it was revoked already.
	pub async fn revoke_source(&self, key_id: &str) -> eyre::Result<bool> {
		let revoked = self.store.revoke_source(key_id).await?;
		if revoked {
			self.live.changed(sources_changed());
		}
		Ok(revoked)
	}

	/// Who signed a request: the window, the key (active), and the MAC over `request.body`.
	/// What that key may write is sites' to judge.
	pub async fn authenticate(&self, request: SignedBatch<'_>, now: Timestamp) -> Result<Grant, IngestError> {
		let refuse = |e: SignatureError| {
			IngestError::Unauthorized(match e {
				SignatureError::MalformedTimestamp | SignatureError::Stale => STALE,
				SignatureError::Mismatch => "bad signature",
			})
		};
		// Free checks first, before the database: the window, and whether the key id could
		// be one at all. A key id that could not is not logged — it is the caller's string.
		signature::check_window(request.timestamp, now).map_err(refuse)?;
		if !panel_core::ids::is_slug(request.key_id) {
			return Err(IngestError::Unauthorized("unknown key"));
		}
		let Some(source) = self.store.active_source(request.key_id).await? else {
			// The MAC is computed anyway, against a key nobody holds, so an unknown key takes
			// as long to refuse as a bad signature and key ids cannot be told apart by timing.
			let _refused = signature::verify(&[0u8; 32], request.timestamp, request.signature, request.body, now);
			tracing::warn!(key_id = request.key_id, "ingest: unknown or revoked key");
			return Err(IngestError::Unauthorized("unknown key"));
		};
		if source.data_key_fp != self.key.fingerprint() {
			return Err(IngestError::Internal(eyre::eyre!("source {} was sealed under another PANEL_DATA_KEY", request.key_id)));
		}
		let secret = self
			.key
			.open(&source_secret_aad(request.key_id), &source.secret_sealed)
			.wrap_err_with(|| format!("opening the secret of source {}", request.key_id))?;
		signature::verify(&secret, request.timestamp, request.signature, request.body, now).map_err(|e| {
			tracing::warn!(key_id = request.key_id, error = %e, "ingest: signature refused");
			refuse(e)
		})?;
		Ok(source.grant)
	}
}

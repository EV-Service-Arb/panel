//! Signing in through concierge (spec §4): the pre-login a browser carries to concierge and
//! back, and the server-side sessions that follow.
//!
//! The panel is a relying party: it sends the browser to concierge's `authorize` with a
//! `state` and a PKCE challenge, and redeems the code that comes back for concierge tokens
//! meant for it alone. Those tokens never reach the browser. The browser holds a random
//! session id; the database holds its hash and the tokens, sealed. What talks to concierge
//! is the server's; this is told the tokens and, to rotate them, asks a [`Refresher`].

use std::{
	collections::HashMap,
	future::Future,
	sync::{Arc, Mutex},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use eyre::WrapErr;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
	Panel,
	live::{Ended, Signal},
	store::sessions::{self, SealedTokens, SessionRow},
};

/// How long a pre-login lives: the time to sign in at concierge (Google included).
pub const PRELOGIN_TTL: SignedDuration = SignedDuration::from_mins(10);

/// An access token this close to expiry is rotated before it is used.
pub const REFRESH_AHEAD: SignedDuration = SignedDuration::from_secs(30);

/// How long a replica's lease on a rotation holds: longer than a call to concierge (5 s),
/// so it lapses only when the holder is gone.
pub const ROTATION_LEASE: SignedDuration = SignedDuration::from_secs(15);

/// How long a request waits for another replica's rotation before answering 503.
pub const ROTATION_WAIT: SignedDuration = SignedDuration::from_secs(6);

const ROTATION_POLL: std::time::Duration = std::time::Duration::from_millis(100);

const PRELOGIN_AAD: &[u8] = b"sa-panel/prelogin/v1";

/// Rotates a refresh token at concierge. The port the session asks when its access token is
/// about to expire; the server implements it over gRPC.
pub trait Refresher: Sync {
	fn refresh(&self, refresh_token: &str) -> impl Future<Output = Result<Tokens, RefreshError>> + Send;
}

/// The rotations under way in this process, one lock per session: its single flight.
#[derive(Debug, Default)]
pub struct Rotations(Mutex<HashMap<SessionKey, Arc<tokio::sync::Mutex<()>>>>);

impl Rotations {
	fn of(&self, key: SessionKey) -> Arc<tokio::sync::Mutex<()>> {
		let mut map = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		map.entry(key).or_default().clone()
	}

	/// Drops the session's lock once nobody else holds or waits for it.
	fn done(&self, key: SessionKey, flight: Arc<tokio::sync::Mutex<()>>) {
		let mut map = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		// The map's clone and this one: no one else is in line.
		if Arc::strong_count(&flight) == 2 {
			map.remove(&key);
		}
	}
}

/// A relying party's token pair, as concierge issues it.
pub struct Tokens {
	pub access: Zeroizing<String>,
	pub access_expires_at: Timestamp,
	pub refresh: Zeroizing<String>,
	pub refresh_expires_at: Timestamp,
}

impl std::fmt::Debug for Tokens {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Tokens")
			.field("access_expires_at", &self.access_expires_at)
			.field("refresh_expires_at", &self.refresh_expires_at)
			.finish_non_exhaustive()
	}
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
	/// Concierge refused: the family is revoked or expired, or the user lost access.
	#[error("concierge refused the refresh")]
	Rejected,
	/// Concierge could not be asked; the session stays as it is.
	#[error("concierge is unavailable: {0}")]
	Unavailable(String),
	#[error(transparent)]
	Failed(eyre::Report),
}

/// Why a request carries no usable session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
	/// No such session, or one past its deadline: sign in.
	#[error("no session")]
	Missing,
	/// Concierge refused to rotate its tokens; the session is closed.
	#[error("the session was refused by concierge")]
	Rejected,
	/// Its access token needs rotating and concierge cannot be reached. Nothing is closed.
	#[error("concierge is unavailable")]
	Unavailable,
	#[error(transparent)]
	Internal(#[from] eyre::Report),
}

/// Names a session without being its cookie: the SHA-256 of the cookie's value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionKey([u8; 32]);

impl SessionKey {
	/// `None` for a value that cannot be a session id, which then costs no query.
	pub fn of_cookie(value: &str) -> Option<Self> {
		if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
			return None;
		}
		let mut h = Sha256::new();
		h.update(b"sa-panel/session/v1/");
		h.update(value.as_bytes());
		Some(Self(h.finalize().into()))
	}

	fn as_bytes(&self) -> &[u8] {
		&self.0
	}
}

/// A live session: whose, and an access token good for at least [`REFRESH_AHEAD`].
pub struct Session {
	pub key: SessionKey,
	pub user_id: Uuid,
	pub access: Zeroizing<String>,
	pub access_expires_at: Timestamp,
}

impl std::fmt::Debug for Session {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Session")
			.field("user_id", &self.user_id)
			.field("access_expires_at", &self.access_expires_at)
			.finish_non_exhaustive()
	}
}

/// A new session: its cookie's value, shown to the browser once, and when it ends.
pub struct Opened {
	pub cookie: Zeroizing<String>,
	pub expires_at: Timestamp,
}

/// The start of a sign-in: what goes to concierge in the URL, and what the browser keeps in
/// the pre-login cookie for the callback.
pub struct Begun {
	/// 256 random bits, hex: the `state` of the authorize request.
	pub state: String,
	/// `BASE64URL(SHA-256(verifier))`, the `code_challenge` (S256).
	pub challenge: String,
	/// The sealed pre-login: state, verifier, where to land and when it was made.
	pub cookie: String,
}

/// A pre-login cookie, opened and checked.
pub struct PreLogin {
	pub state: String,
	/// The PKCE verifier, presented with the code.
	pub verifier: Zeroizing<String>,
	/// The path `/auth/login` was asked to come back to, checked there.
	pub return_to: Option<String>,
}

/// 256 random bits as 64 hex characters: states, verifiers, session ids, CSRF tokens.
pub fn random_token() -> eyre::Result<String> {
	let mut raw = Zeroizing::new([0u8; 32]);
	getrandom::fill(raw.as_mut_slice()).map_err(|e| eyre::eyre!("the OS random source failed: {e}"))?;
	Ok(hex::encode(raw.as_slice()))
}

/// The S256 code challenge of a PKCE verifier.
pub fn pkce_challenge(verifier: &str) -> String {
	URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[derive(Deserialize, Serialize)]
struct PreLoginPlain {
	state: String,
	verifier: String,
	return_to: Option<String>,
	issued_at: i64,
}

/// Which of a session's two tokens.
#[derive(Clone, Copy)]
enum Which {
	Access,
	Refresh,
}

/// Associated data of a session's token: which one, and whose row, so neither opens as the
/// other nor on another session.
fn token_aad(which: Which, key: &SessionKey) -> Vec<u8> {
	let which: &[u8] = match which {
		Which::Access => b"access/",
		Which::Refresh => b"refresh/",
	};
	[b"sa-panel/session/v1/".as_slice(), which, key.as_bytes()].concat()
}

impl Panel {
	/// A fresh state and PKCE verifier, and the cookie that carries them and `return_to` to
	/// the callback.
	pub fn begin_sign_in(&self, return_to: Option<&str>, now: Timestamp) -> eyre::Result<Begun> {
		let state = random_token()?;
		// 64 hex characters: inside PKCE's 43–128 unreserved characters.
		let verifier = Zeroizing::new(random_token()?);
		let plain = Zeroizing::new(
			serde_json::to_vec(&PreLoginPlain {
				state: state.clone(),
				verifier: verifier.to_string(),
				return_to: return_to.map(str::to_owned),
				issued_at: now.as_second(),
			})
			.wrap_err("serializing a pre-login")?,
		);
		let cookie = URL_SAFE_NO_PAD.encode(self.key.seal(PRELOGIN_AAD, &plain)?);
		Ok(Begun {
			challenge: pkce_challenge(&verifier),
			state,
			cookie,
		})
	}

	/// Opens a pre-login cookie and checks that it is this panel's, younger than
	/// [`PRELOGIN_TTL`], and made for `state` — compared in constant time. `None` for
	/// anything else: the callback then presents no code.
	pub fn finish_sign_in(&self, cookie: &str, state: &str, now: Timestamp) -> Option<PreLogin> {
		let blob = URL_SAFE_NO_PAD.decode(cookie).ok()?;
		let plain = self.key.open(PRELOGIN_AAD, &blob).ok()?;
		let pre: PreLoginPlain = serde_json::from_slice(&plain).ok()?;
		let verifier = Zeroizing::new(pre.verifier);
		let issued = Timestamp::from_second(pre.issued_at).ok()?;
		let age = now.duration_since(issued);
		if age.is_negative() || age > PRELOGIN_TTL {
			return None;
		}
		let same = pre.state.len() == state.len() && bool::from(pre.state.as_bytes().ct_eq(state.as_bytes()));
		same.then_some(PreLogin {
			state: pre.state,
			verifier,
			return_to: pre.return_to,
		})
	}

	/// Opens a session for a user concierge has just issued tokens for. Sessions past their
	/// deadline are dropped on the way.
	pub async fn open_session(&self, user_id: Uuid, tokens: &Tokens, now: Timestamp) -> eyre::Result<Opened> {
		let cookie = Zeroizing::new(random_token()?);
		let key = SessionKey::of_cookie(&cookie).ok_or_else(|| eyre::eyre!("a fresh session id is not one"))?;
		let (access, refresh) = self.seal_tokens(&key, tokens)?;
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
		let pruned = sessions::prune(&mut conn, now).await?;
		if pruned > 0 {
			tracing::debug!(pruned, "dropped expired sessions");
		}
		sessions::insert(&mut conn, key.as_bytes(), user_id, &self.sealed(&access, &refresh, tokens), now).await?;
		Ok(Opened {
			cookie,
			expires_at: tokens.refresh_expires_at,
		})
	}

	/// The session a cookie names, its access token rotated first when it is about to expire.
	///
	/// A refresh token is presented to concierge once: presenting it twice has concierge
	/// revoke the whole family. No pool connection is held while concierge is asked:
	///
	/// - within this process, one rotation per session at a time ([`Rotations`]); the others
	///   wait for it, then find the fresh row;
	/// - across replicas, a lease on the row (`rotating_until`, taken by one conditional
	///   UPDATE): only its holder asks concierge; the others poll the row until the rotated
	///   tokens land or [`ROTATION_WAIT`] passes (then 503). A lease outlives a call to
	///   concierge, so it lapses only when its holder died, and is then taken over;
	/// - the rotated tokens are written only if the row still has the tokens the rotation
	///   started from, so nothing overwrites a newer pair.
	///
	/// If the holder's answer is lost after concierge rotated, the next rotation presents a
	/// spent token and concierge closes the session: it fails closed, never open.
	pub async fn session(&self, cookie: &str, now: Timestamp, refresher: &impl Refresher) -> Result<Session, SessionError> {
		let key = SessionKey::of_cookie(cookie).ok_or(SessionError::Missing)?;
		self.session_of_key(key, now, refresher).await
	}

	/// The user's newest live session used at or after `seen_since`, rotated like
	/// [`Self::session`]: what the panel asks concierge with on the user's behalf when there
	/// is no request of theirs to carry a cookie (a Telegram button). `Missing` when they have
	/// none — so a session the user abandoned is not kept alive on their behalf.
	pub async fn session_of_user(&self, user_id: Uuid, now: Timestamp, seen_since: Timestamp, refresher: &impl Refresher) -> Result<Session, SessionError> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
		let id_hash = sessions::newest_of_user(&mut conn, user_id, now, seen_since).await?.ok_or(SessionError::Missing)?;
		drop(conn);
		let key = SessionKey(id_hash.as_slice().try_into().wrap_err("a stored session key")?);
		self.session_of_key(key, now, refresher).await
	}

	async fn session_of_key(&self, key: SessionKey, now: Timestamp, refresher: &impl Refresher) -> Result<Session, SessionError> {
		let row = self.session_row(&key, now).await?;
		if row.access_expires_at > now + REFRESH_AHEAD {
			return self.live(key, &row).map_err(SessionError::Internal);
		}
		let flight = self.rotations.of(key);
		let turn = flight.lock().await;
		let result = self.rotate(key, now, refresher).await;
		drop(turn);
		self.rotations.done(key, flight);
		result
	}

	async fn session_row(&self, key: &SessionKey, now: Timestamp) -> Result<SessionRow, SessionError> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
		let row = sessions::get(&mut conn, key.as_bytes()).await?.ok_or(SessionError::Missing)?;
		if row.expires_at <= now {
			sessions::delete(&mut conn, key.as_bytes()).await?;
			return Err(SessionError::Missing);
		}
		Ok(row)
	}

	async fn rotate(&self, key: SessionKey, now: Timestamp, refresher: &impl Refresher) -> Result<Session, SessionError> {
		let waited_since = std::time::Instant::now();
		let row = loop {
			let row = self.session_row(&key, now).await?;
			if row.access_expires_at > now + REFRESH_AHEAD {
				// Rotated while this one waited: here, or by another replica.
				return self.live(key, &row).map_err(SessionError::Internal);
			}
			let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
			if sessions::claim_rotation(&mut conn, key.as_bytes(), row.access_expires_at, now, now + ROTATION_LEASE).await? {
				break row;
			}
			drop(conn);
			if waited_since.elapsed() >= ROTATION_WAIT.unsigned_abs() {
				tracing::warn!(user_id = %row.user_id, "another replica holds the session's rotation");
				return Err(SessionError::Unavailable);
			}
			tokio::time::sleep(ROTATION_POLL).await;
		};
		let seen = row.access_expires_at;
		let refresh = self.open_token(Which::Refresh, &key, &row)?;
		let answer = refresher.refresh(&refresh).await;
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
		match answer {
			Ok(tokens) => {
				let (access, refresh) = self.seal_tokens(&key, &tokens)?;
				if !sessions::set_tokens(&mut conn, key.as_bytes(), seen, &self.sealed(&access, &refresh, &tokens)).await? {
					// The lease was taken over while concierge answered: that holder's pair,
					// presented from the same refresh token, is the one concierge keeps.
					tracing::warn!(user_id = %row.user_id, "a session was rotated twice; keeping the row's pair");
					return Err(SessionError::Unavailable);
				}
				Ok(Session {
					key,
					user_id: row.user_id,
					access: tokens.access,
					access_expires_at: tokens.access_expires_at,
				})
			}
			Err(RefreshError::Rejected) => {
				sessions::delete_refused(&mut conn, key.as_bytes(), seen).await?;
				tracing::info!(user_id = %row.user_id, "concierge refused a session refresh; session closed");
				self.live.publish(Signal::SessionsEnded(Ended::Session(key)));
				Err(SessionError::Rejected)
			}
			Err(RefreshError::Unavailable(why)) => {
				sessions::release_rotation(&mut conn, key.as_bytes(), seen).await?;
				tracing::warn!(user_id = %row.user_id, why, "concierge unavailable for a session refresh");
				Err(SessionError::Unavailable)
			}
			Err(RefreshError::Failed(e)) => {
				sessions::release_rotation(&mut conn, key.as_bytes(), seen).await?;
				Err(SessionError::Internal(e))
			}
		}
	}

	/// Signs a user out everywhere: every session of the user whose session `key` is, closed.
	/// Whose they were, or `None` when `key` names no session.
	pub async fn close_all_sessions(&self, key: &SessionKey) -> eyre::Result<Option<Uuid>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
		let user = sessions::delete_all_of(&mut conn, key.as_bytes()).await?;
		if let Some(user) = user {
			self.live.publish(Signal::SessionsEnded(Ended::User(user)));
		}
		Ok(user)
	}

	/// Redeems a callback's `state`, once: `false` when it was already, and the callback must
	/// then not present its code. The mark lives as long as the pre-login could.
	pub async fn consume_state(&self, state: &str, now: Timestamp) -> eyre::Result<bool> {
		let mut h = Sha256::new();
		h.update(b"sa-panel/state/v1/");
		h.update(state.as_bytes());
		let hash: [u8; 32] = h.finalize().into();
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a sign-in state")?;
		sessions::consume_state(&mut conn, &hash, now, now + PRELOGIN_TTL).await
	}

	/// Marks a session used by its user (a request of theirs), at most once a minute.
	pub async fn touch_session(&self, key: &SessionKey, now: Timestamp) -> eyre::Result<()> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
		sessions::touch(&mut conn, key.as_bytes(), now).await
	}

	/// Closes a session; `false` when there was none.
	pub async fn close_session(&self, key: &SessionKey) -> eyre::Result<bool> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a session")?;
		let gone = sessions::delete(&mut conn, key.as_bytes()).await?;
		if gone {
			self.live.publish(Signal::SessionsEnded(Ended::Session(*key)));
		}
		Ok(gone)
	}

	fn live(&self, key: SessionKey, row: &SessionRow) -> eyre::Result<Session> {
		Ok(Session {
			key,
			user_id: row.user_id,
			access: self.open_token(Which::Access, &key, row)?,
			access_expires_at: row.access_expires_at,
		})
	}

	fn seal_tokens(&self, key: &SessionKey, tokens: &Tokens) -> eyre::Result<(Vec<u8>, Vec<u8>)> {
		Ok((
			self.key.seal(&token_aad(Which::Access, key), tokens.access.as_bytes())?,
			self.key.seal(&token_aad(Which::Refresh, key), tokens.refresh.as_bytes())?,
		))
	}

	fn sealed<'a>(&self, access: &'a [u8], refresh: &'a [u8], tokens: &Tokens) -> SealedTokens<'a> {
		SealedTokens {
			access,
			access_expires_at: tokens.access_expires_at,
			refresh,
			data_key_fp: self.key.fingerprint(),
			expires_at: tokens.refresh_expires_at,
		}
	}

	fn open_token(&self, which: Which, key: &SessionKey, row: &SessionRow) -> eyre::Result<Zeroizing<String>> {
		eyre::ensure!(row.data_key_fp == self.key.fingerprint(), "a session was sealed under another PANEL_DATA_KEY");
		let blob = match which {
			Which::Access => &row.access_sealed,
			Which::Refresh => &row.refresh_sealed,
		};
		let plain = self.key.open(&token_aad(which, key), blob)?;
		let text = String::from_utf8(plain.to_vec()).wrap_err("a sealed token is not UTF-8")?;
		Ok(Zeroizing::new(text))
	}
}

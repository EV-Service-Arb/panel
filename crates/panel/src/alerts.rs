//! Alerts: the rules' notes — the services' (read from their feeds) and the conductor's own — each
//! fanned out to the destinations its audience reaches, and each user's destinations and the rules
//! on each. How a destination is reached is its method's ([`crate::telegram`]).
//!
//! ```text
//! sites, review_archive   GET /api/conductor/notifications?after=<cursor> → a page: each note
//!                         checked against its feed, claimed (rule, event) and queued, the cursor
//!                         moved — one transaction, taken only from where the cursor stands
//! the conductor's own     sources silent, access requests
//! a rule's audience       a permission rule: the destinations whose user's permissions open it
//!                         a GMB rule: the destinations of the named GMBs' owners (the GMB on,
//!                         it and its gmail not removed, the destination's gmail, if any, theirs)
//!                         either: the rule on there, the user's access confirmed within ACCESS_TTL
//! ```
//!
//! **Who has access.** The conductor learns a user's permissions only from concierge's `GetMe`,
//! which takes the user's own access token. The hub keeps what concierge last confirmed and when:
//! the `/api/v1` gate records every answer it gets, and [`Hub::recheck_access`] asks again through
//! the user's newest session for a user with a destination reached, not confirmed in
//! [`RECHECK_AFTER`] — through a session the user used within [`SESSION_IDLE`], so the hub never
//! keeps an abandoned one alive. A message is queued, and again sent, only to a user confirmed
//! within [`ACCESS_TTL`]. So: a grant revoked at concierge, or a session concierge refuses to
//! rotate, stops messages at the next check, and what was queued for the user is dropped; a user
//! with no session used within [`SESSION_IDLE`], or concierge unreachable, stops them after
//! [`ACCESS_TTL`] — PII does not flow on a stale answer.

use std::{collections::BTreeMap, future::Future};

use eyre::WrapErr;
use jiff::{SignedDuration, Timestamp};
use sa_alerts::{Audience, Call, NotePage, Origin, Rule};
use sa_auth::{PermissionSet, Pii};
use sa_core::notify::{self, Locale, Note, Rendered};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use crate::store::alerts::Destination;
use crate::{
	Panel, Refused,
	session::{Refresher, SessionError},
	store::alerts::{self as db, Recipient},
};

/// Nothing is sent to a user whose access concierge has not confirmed for this long.
pub const ACCESS_TTL: SignedDuration = SignedDuration::from_hours(1);

/// A user not confirmed for this long is asked about again.
pub const RECHECK_AFTER: SignedDuration = SignedDuration::from_mins(15);

/// The hub asks concierge only with a session its user used within this long.
pub const SESSION_IDLE: SignedDuration = SignedDuration::from_hours(24 * 7);

/// Notes asked of a service at once.
pub const NOTES_PAGE: usize = 500;

/// Fan-out marks and finished messages are kept this long.
const KEEP: SignedDuration = SignedDuration::from_hours(24 * 7);

/// Access requests taken per pass.
const BATCH: i64 = 50;

/// Users re-confirmed per pass.
const RECHECKS: i64 = 20;

/// Who concierge says the holder of an access token is.
#[derive(Clone, Debug)]
pub struct Identity {
	pub user_id: Uuid,
	pub email: String,
	pub email_verified: bool,
	pub preferred_name: String,
	pub permissions: PermissionSet,
}

impl Identity {
	/// preferred_name, or the email when there is none.
	pub fn display_name(&self) -> &str {
		if self.preferred_name.trim().is_empty() { &self.email } else { &self.preferred_name }
	}
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
	/// The token is refused: revoked, expired, the user held or disabled.
	#[error("concierge refused the token")]
	Refused,
	#[error("concierge is unavailable: {0}")]
	Unavailable(String),
	#[error("concierge failed: {0}")]
	Failed(String),
}

/// concierge's `GetMe`: the port the hub confirms a user's access through.
pub trait Directory: Sync {
	fn me(&self, access: &str) -> impl Future<Output = Result<Identity, DirectoryError>> + Send;
}

/// The services, as the hub needs them; each call under the conductor's assertion.
pub trait Feeds: Sync {
	/// Up to `limit` of `service`'s notes after `after`; `None` while it publishes none (404).
	fn notes(&self, service: Origin, after: i64, limit: usize) -> impl Future<Output = eyre::Result<Option<NotePage>>> + Send;
	/// When each source key's last event arrived at sites.
	fn received(&self) -> impl Future<Output = eyre::Result<BTreeMap<String, Timestamp>>> + Send;
	/// `call` made to sites as `who`, under `idempotency_key`: the status sites answered.
	fn press(&self, who: &Identity, call: &Call, idempotency_key: &str) -> impl Future<Output = eyre::Result<u16>> + Send;
	/// The photo at `path` of `service`, as a note named it.
	fn photo(&self, service: Origin, path: &str) -> impl Future<Output = eyre::Result<Vec<u8>>> + Send;
}

/// What concierge said of a user just now.
#[derive(Clone, Debug)]
pub enum Access {
	Granted(Identity),
	/// No live session to ask with, or concierge refused its token.
	NoSession,
	Unavailable,
}

/// A user's alerts, as their page shows them.
#[derive(Clone, Debug)]
pub struct Alerts {
	/// The rules open to the user: their permissions' and, holding a GMB, the GMB rules.
	pub rules: Vec<Rule>,
	pub destinations: Vec<Destination>,
}

/// What a recipient is sent.
pub(crate) struct Outgoing {
	pub text: Rendered,
	/// `(label, url)` buttons.
	pub links: Vec<(String, String)>,
	pub buttons: Vec<sa_alerts::Button>,
	pub photo: Option<String>,
}

/// The id a rule fans out under for what the conductor tells of itself: a UUID (version 8)
/// named by its parts.
pub(crate) fn derived_event_id(parts: &[&str]) -> Uuid {
	let mut h = Sha256::new();
	h.update(b"sa-panel/tg-fanout/v1");
	for p in parts {
		h.update([0]);
		h.update(p.as_bytes());
	}
	let digest = h.finalize();
	let mut bytes = [0u8; 16];
	bytes.copy_from_slice(&digest[..16]);
	uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// A user's destinations or rules changed. Stamped here: the calls that change them take no clock.
pub(crate) fn alerts_changed(user: Uuid) -> crate::live::Change {
	crate::live::Change {
		topic: "alerts".into(),
		brand: None,
		id: None,
		at: Timestamp::now(),
		audience: crate::live::Audience::User(user),
	}
}

impl Panel {
	pub async fn alerts(&self, user: Uuid, permissions: &PermissionSet) -> eyre::Result<Alerts> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for alerts")?;
		let gmbs = db::has_gmbs(&mut conn, user).await?;
		Ok(Alerts {
			rules: Rule::ALL.into_iter().filter(|r| open(*r, permissions, gmbs)).collect(),
			destinations: db::destinations(&mut conn, user).await?,
		})
	}

	/// Turns rules on or off at one of the user's destinations. A rule not open to them is
	/// refused, not stored: it would only mislead the page.
	pub async fn alerts_set_rules(&self, user: Uuid, permissions: &PermissionSet, destination: i64, rules: &[(Rule, bool)]) -> eyre::Result<Result<(), Refused>> {
		let mut tx = self.store.begin_write().await?;
		if db::destination_of(&mut tx, user, destination).await?.is_none() {
			return Ok(Err(Refused::NotFound));
		}
		let gmbs = db::has_gmbs(&mut tx, user).await?;
		if let Some((r, _)) = rules.iter().find(|(r, _)| !open(*r, permissions, gmbs)) {
			return Ok(Err(Refused::Invalid(format!("{r} is not a rule open to you"))));
		}
		for (rule, on) in rules {
			db::set_rule(&mut tx, destination, *rule, *on).await?;
		}
		tx.commit().await.wrap_err("committing a rules change")?;
		self.live.changed(alerts_changed(user));
		Ok(Ok(()))
	}

	/// Scopes one of the user's destinations' GMB rules to one of their gmails, or to none.
	pub async fn alerts_set_gmail(&self, user: Uuid, destination: i64, gmail: Option<i64>) -> eyre::Result<Result<(), Refused>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection to scope a destination")?;
		if !db::set_gmail(&mut conn, user, destination, gmail).await? {
			return Ok(Err(Refused::NotFound));
		}
		self.live.changed(alerts_changed(user));
		Ok(Ok(()))
	}

	/// Removes one of the user's destinations, with what was queued for it; `false` when it was not
	/// theirs.
	pub async fn alerts_remove(&self, user: Uuid, destination: i64) -> eyre::Result<bool> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection to remove a destination")?;
		let gone = db::remove(&mut conn, user, destination).await?;
		if gone {
			self.live.changed(alerts_changed(user));
		}
		Ok(gone)
	}

	/// What concierge just told the `/api/v1` gate of a user: their permissions and name.
	pub async fn alerts_access_seen(&self, user: Uuid, permissions: &PermissionSet, display: &str, now: Timestamp) -> eyre::Result<()> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection to record access")?;
		db::access_seen(&mut conn, user, permissions, display, now).await
	}

	/// Queues what the conductor's own rules say is due at `now`: silent sources (by `received`,
	/// sites' last receipt per key) and access requests, each told once per destination. Safe on
	/// several replicas at once: each candidate is claimed, and the outbox refuses a second
	/// `(rule, event, destination)`. `cabinet` is concierge's origin, where the admins grant access.
	/// How many messages were queued.
	pub async fn alerts_fan_out_own(&self, now: Timestamp, locale: Locale, cabinet: &url::Url, received: &BTreeMap<String, Timestamp>) -> eyre::Result<usize> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for the fan-out")?;
		let mut own = Vec::new();
		for (key_id, kind, created_at) in db::active_sources(&mut conn).await? {
			let last = received.get(&key_id).copied();
			let since = last.unwrap_or(created_at);
			if since >= now - notify::SOURCE_SILENCE {
				continue;
			}
			// One message per full day of the same silence: at 24 h, at 48 h, …
			let days = now.duration_since(since).as_hours() / 24;
			let event = derived_event_id(&["source_silent", &key_id, &since.to_string(), &days.to_string()]);
			own.push((event, Note::SourceSilent { key_id, kind, last }));
		}
		for r in crate::store::access::to_tell(&mut conn, now - crate::access::ASK_AGAIN_AFTER, BATCH).await? {
			let note = Note::AccessRequested {
				email: r.email,
				name: r.name,
				need: r.need,
			};
			own.push((r.event_id, note));
		}
		let mut queued = 0;
		for (event, note) in own {
			// An access request names who asked: for a person's own chat only.
			let pii = matches!(note, Note::AccessRequested { .. });
			let outgoing = |r: &Recipient| {
				(r.private || !pii).then(|| Outgoing {
					text: note.render(locale),
					links: note.link(locale, cabinet).into_iter().filter(|_| r.private).map(|(label, url)| (label.to_owned(), url)).collect(),
					buttons: Vec::new(),
					photo: None,
				})
			};
			let mut tx = crate::store::begin_write(&mut conn).await?;
			queued += self.fan_out_one(&mut tx, note.rule(), event, None, None, outgoing, now, now).await?;
			tx.commit().await.wrap_err("committing a fan-out")?;
		}
		Ok(queued)
	}

	/// The last of a service's notes already fanned out: where its next page starts.
	pub async fn alerts_feed_after(&self, service: Origin) -> eyre::Result<i64> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a feed's cursor")?;
		db::feed_after(&mut conn, service).await
	}

	/// A page of `service`'s notes, which started after `after`, into the outbox: each note checked
	/// against its feed, claimed and queued for its audience, and the cursor moved to the page's
	/// `next`, in one transaction — taken only while the cursor still stands at `after`. A note the
	/// hub cannot send fails the page whole: the cursor stays, and the next pass stops on it again.
	/// How many messages were queued.
	pub async fn alerts_take_notes(&self, service: Origin, page: NotePage, after: i64, now: Timestamp) -> eyre::Result<usize> {
		eyre::ensure!(page.next >= after, "{service} answered next {} for notes after {after}", page.next);
		for note in &page.notes {
			note.check(service).wrap_err_with(|| format!("{service}'s feed"))?;
		}
		let mut tx = self.store.begin_write().await?;
		let mut queued = 0;
		for note in &page.notes {
			let outgoing = |r: &Recipient| {
				let text = match &note.text_pii {
					Some(pii) if r.private && r.permissions.may(Pii::See) => pii,
					_ => &note.text,
				};
				Some(Outgoing {
					text: Rendered {
						text: text.text.clone(),
						entities: text.entities.clone(),
					},
					links: Vec::new(),
					buttons: if r.private { note.buttons.clone() } else { Vec::new() },
					photo: note.photo.clone(),
				})
			};
			let gmbs = (note.rule.audience() == Audience::GmbOwners).then_some(note.gmbs.as_slice());
			queued += self.fan_out_one(&mut tx, note.rule, note.event_id, note.skip_user, gmbs, outgoing, note.created_at, now).await?;
		}
		if !db::move_feed(&mut tx, service, after, page.next).await? {
			tracing::info!(%service, after, "alerts: the page was taken meanwhile");
			return Ok(0);
		}
		tx.commit().await.wrap_err("committing a page of notes")?;
		Ok(queued)
	}

	/// One candidate, inside the caller's write transaction: claimed, then queued for every
	/// destination its audience reaches but `skip`'s, so a claim never stands without its messages.
	/// `outgoing` is what a recipient is sent, `None` for nothing.
	#[expect(clippy::too_many_arguments, reason = "the candidate and the pass's context, each named at the call site")]
	async fn fan_out_one(
		&self,
		tx: &mut sqlx::SqliteConnection,
		rule: Rule,
		event: Uuid,
		skip: Option<Uuid>,
		gmbs: Option<&[i64]>,
		outgoing: impl Fn(&Recipient) -> Option<Outgoing>,
		noted_at: Timestamp,
		now: Timestamp,
	) -> eyre::Result<usize> {
		if !db::claim_fanout(tx, rule, event, now).await? {
			return Ok(0);
		}
		let mut queued = 0;
		for r in db::recipients(tx, rule, now - ACCESS_TTL, gmbs).await? {
			if !rule.open_to(&r.permissions) || skip == Some(r.user_id) {
				continue;
			}
			let Some(out) = outgoing(&r) else { continue };
			let to = crate::telegram::Target {
				chat_id: r.chat_id,
				topic_id: r.topic_id,
			};
			if self.queue(tx, (r.destination_id, r.user_id, to), rule, event, &out, noted_at, now).await? {
				queued += 1;
			}
		}
		if queued > 0 {
			tracing::info!(%rule, %event, queued, "alerts: queued");
		}
		Ok(queued)
	}
}

/// Whether a rule is open to a user holding `permissions`, and a GMB if `gmbs`.
fn open(rule: Rule, permissions: &PermissionSet, gmbs: bool) -> bool {
	rule.open_to(permissions) && (rule.audience() == Audience::Permission || gmbs)
}

/// The hub at work: the feeds read, the conductor's own rules, and the access checks; a method's
/// delivery runs on it too ([`crate::telegram`]).
#[derive(Clone, Debug)]
pub struct Hub<B, C, F> {
	pub panel: Panel,
	pub bot: B,
	/// concierge: to rotate a session's tokens and to ask `GetMe` with them.
	pub concierge: C,
	/// Where the services' notes come from, and what a button calls.
	pub feeds: F,
	pub locale: Locale,
	/// concierge's origin: where the admins grant the access a user asks for.
	pub cabinet: url::Url,
}

impl<B, C: Refresher + Directory, F: Feeds> Hub<B, C, F> {
	/// Queues what the conductor's own rules say is due.
	pub async fn fan_out_own(&self, now: Timestamp) -> eyre::Result<usize> {
		let received = self.feeds.received().await?;
		self.panel.alerts_fan_out_own(now, self.locale, &self.cabinet, &received).await
	}

	/// A page of `service`'s notes into the outbox: how many messages were queued, `None` while it
	/// publishes none.
	pub async fn take_feed(&self, service: Origin, now: Timestamp) -> eyre::Result<Option<usize>> {
		let after = self.panel.alerts_feed_after(service).await?;
		let Some(page) = self.feeds.notes(service, after, NOTES_PAGE).await? else {
			return Ok(None);
		};
		self.panel.alerts_take_notes(service, page, after, now).await.map(Some)
	}

	/// Confirms the access of users not confirmed for [`RECHECK_AFTER`], and prunes what is past
	/// keeping. How many were asked about.
	pub async fn recheck_access(&self, now: Timestamp) -> eyre::Result<usize> {
		let users = {
			let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection for access checks")?;
			db::prune(&mut conn, now - KEEP).await?;
			crate::store::telegram::prune(&mut conn, now - KEEP).await?;
			db::stale_access(&mut conn, now - RECHECK_AFTER, RECHECKS).await?
		};
		for user in &users {
			let access = self.confirm_access(*user, now).await?;
			tracing::debug!(user_id = %user, ?access, "alerts: access rechecked");
		}
		Ok(users.len())
	}

	/// Asks concierge, now, what the user may do — with their newest panel session used within
	/// [`SESSION_IDLE`], rotated when due — and records the answer. A session concierge refuses (to
	/// rotate, or `GetMe` twice running) ends the user's alerts and drops what was queued for them.
	pub async fn confirm_access(&self, user: Uuid, now: Timestamp) -> eyre::Result<Access> {
		let panel = &self.panel;
		let conn = || async { panel.store.pool().acquire().await.wrap_err("a connection to record access") };
		let session = match panel.session_of_user(user, now, now - SESSION_IDLE, &self.concierge).await {
			Ok(s) => s,
			Err(SessionError::Missing) => {
				db::access_tried(&mut *conn().await?, user, now).await?;
				return Ok(Access::NoSession);
			}
			Err(SessionError::Rejected) => {
				db::access_lost(&mut *conn().await?, user, now).await?;
				return Ok(Access::NoSession);
			}
			Err(SessionError::Unavailable) => {
				db::access_tried(&mut *conn().await?, user, now).await?;
				return Ok(Access::Unavailable);
			}
			Err(SessionError::Internal(e)) => return Err(e),
		};
		let answer = match self.concierge.me(&session.access).await {
			// One refusal may be a hiccup on concierge's side; the session is closed only on a
			// second.
			Err(DirectoryError::Refused) => self.concierge.me(&session.access).await,
			other => other,
		};
		match answer {
			Ok(id) if id.user_id != user => Err(eyre::eyre!("GetMe answered another user than the session's")),
			Ok(identity) => {
				db::access_seen(&mut *conn().await?, user, &identity.permissions, identity.display_name(), now).await?;
				Ok(Access::Granted(identity))
			}
			Err(DirectoryError::Refused) => {
				// The token is dead at concierge: so is the session, and whatever it vouched for.
				panel.close_session(&session.key).await?;
				db::access_lost(&mut *conn().await?, user, now).await?;
				Ok(Access::NoSession)
			}
			Err(DirectoryError::Unavailable(why)) => {
				tracing::warn!(why, "alerts: concierge unavailable for an access check");
				db::access_tried(&mut *conn().await?, user, now).await?;
				Ok(Access::Unavailable)
			}
			Err(e @ DirectoryError::Failed(_)) => Err(eyre::eyre!(e)),
		}
	}
}

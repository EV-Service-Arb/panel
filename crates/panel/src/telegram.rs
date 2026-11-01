//! Telegram notifications in private chats: linking a chat to a user, the rules' messages into
//! the outbox, delivering the outbox, and what the bot does with the updates it receives —
//! `/start <token>`, `/stop` and the buttons under a message.
//!
//! ```text
//! POST /api/v1/telegram/link ─ token (256 bits; its hash, 10 min) ─ t.me/<bot>?start=<token>
//!                         /start <token> in a private chat ─ link user ⇄ chat
//! sites' notes (a page, the cursor in the same transaction) ┐
//! source_silent, access_requested (the conductor's own)     ┴─ a rule × its recipients ─ outbox
//! outbox ─ claim (1/s per chat, ≤ 25/s in all, across replicas) ─ sendMessage ─ sent | retry | dead
//! button ─ signed data ─ the chat's user, access asked of concierge now ─ sites, as that user
//! ```
//!
//! **Who has access.** The conductor learns a user's permissions only from concierge's `GetMe`,
//! which takes the user's own access token. A link keeps what concierge last confirmed and when:
//! the `/api/v1` gate records every answer it gets, and [`Notifier::recheck_access`] asks
//! again through the user's newest session for a link not confirmed in [`RECHECK_AFTER`] —
//! through a session the user used within [`SESSION_IDLE`], so the bot never keeps an abandoned
//! one alive. A message is queued, and again sent, only to a link confirmed within
//! [`ACCESS_TTL`]. So: a grant revoked at concierge, or a session concierge refuses to rotate,
//! stops messages at the next check, and what was queued for the user is dropped; a user with
//! no session used within [`SESSION_IDLE`], or concierge unreachable, stops them after
//! [`ACCESS_TTL`] — PII does not flow on a stale answer. A button asks concierge at the moment
//! it is pressed, and without such a session to ask with, the user is told to open the panel.
//!
//! The Bot API is the server's; this sees it through [`Bot`], concierge through [`Directory`]
//! and [`Refresher`], and sites through [`Sites`].

use std::{collections::BTreeMap, future::Future};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use eyre::WrapErr;
use futures::{StreamExt, TryStreamExt};
use jiff::{SignedDuration, Timestamp};
use panel_core::{
	Invalid,
	notify::{self, Failure, Locale, MAX_NAME, Next, Note, Rendered, Reply, Rule},
};
use sa_auth::{PermissionSet, Pii};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

pub use crate::store::telegram::Account;
use crate::{
	Panel,
	seal::telegram_text_aad,
	session::{Refresher, SessionError},
	store::telegram::{self as db, NewMessage},
};

/// How long a link token may wait for its `/start`.
pub const LINK_TTL: SignedDuration = SignedDuration::from_mins(10);

/// Nothing is sent to a user whose access concierge has not confirmed for this long.
pub const ACCESS_TTL: SignedDuration = SignedDuration::from_hours(1);

/// A link not confirmed for this long is asked about again.
pub const RECHECK_AFTER: SignedDuration = SignedDuration::from_mins(15);

/// The bot asks concierge only with a session its user used within this long.
pub const SESSION_IDLE: SignedDuration = SignedDuration::from_hours(24 * 7);

/// At most one unsolicited reply (help, "not linked", …) per chat this often.
pub const REPLY_EVERY: SignedDuration = SignedDuration::from_mins(10);

/// Past this many lead messages waiting for one chat, they are folded into one summary.
pub const BACKLOG: i64 = 20;

/// How long a replica sending a message holds it: past the Bot API's timeout, so it lapses
/// only when the replica is gone.
pub const SEND_LEASE: SignedDuration = SignedDuration::from_secs(60);

/// Notes asked of sites at once.
pub const NOTES_PAGE: usize = 500;

/// A lead message noted longer ago than this is news nobody needs.
const STALE_AFTER: SignedDuration = SignedDuration::from_hours(1);

/// Fan-out marks and finished messages are kept this long.
const KEEP: SignedDuration = SignedDuration::from_hours(24 * 7);

/// Access requests taken per pass.
const BATCH: i64 = 50;

/// Messages sent at once in a pass.
const PARALLEL_SENDS: usize = 8;

/// Links re-confirmed per pass.
const RECHECKS: i64 = 20;

/// A button under a message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InlineButton {
	/// Pressed, it comes back to the bot with its signed data.
	Callback { label: String, data: String },
	/// Opens `url`.
	Url { label: String, url: String },
}

/// The Bot API, as the notifier needs it. Every call answers the failure as
/// [`notify::Failure`], so the outbox can decide what becomes of the message.
pub trait Bot: Sync {
	/// Sends a plain-text message with its entities to a chat; the id Telegram gave it.
	fn send(&self, chat_id: i64, message: &Rendered, buttons: &[InlineButton]) -> impl Future<Output = Result<i64, Failure>> + Send;
	/// Replaces a message's text and buttons (none: the keyboard is removed).
	fn edit(&self, chat_id: i64, message_id: i64, message: &Rendered, buttons: &[InlineButton]) -> impl Future<Output = Result<(), Failure>> + Send;
	/// Answers a button press, with a short notice the user sees.
	fn answer(&self, callback_id: &str, text: &str) -> impl Future<Output = Result<(), Failure>> + Send;
}

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

/// concierge's `GetMe`: the port the notifier confirms a user's access through.
pub trait Directory: Sync {
	fn me(&self, access: &str) -> impl Future<Output = Result<Identity, DirectoryError>> + Send;
}

/// What sites does for a press: `call` sent as `who`; the status sites answered.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
	pub method: String,
	pub path: String,
	/// `null`: no body.
	pub body: Value,
}

/// A button under a message, as sites describes it: what it says, what pressing it calls,
/// what the message then says, and which buttons it leaves.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Button {
	pub id: String,
	pub label: String,
	pub pressed: String,
	pub call: Call,
	pub leaves: Vec<String>,
}

/// A text as sites renders it: Telegram `MessageEntity` objects over it, sent as given.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteText {
	pub text: String,
	pub entities: Vec<Value>,
}

/// One of sites' notes (`GET /api/conductor/notifications`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SitesNote {
	pub seq: i64,
	pub rule: String,
	pub event_id: Uuid,
	pub created_at: Timestamp,
	pub text: NoteText,
	/// Present iff the note carries PII: for holders of `sa:sites:work:pii:see` only.
	pub text_pii: Option<NoteText>,
	/// Who did what it tells of, and so is not told.
	pub skip_user: Option<Uuid>,
	pub buttons: Vec<Button>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotePage {
	pub notes: Vec<SitesNote>,
	pub next: i64,
}

/// sites, as the hub needs it.
pub trait Sites: Sync {
	/// Up to `limit` notes after `after`.
	fn notes(&self, after: i64, limit: usize) -> impl Future<Output = eyre::Result<NotePage>> + Send;
	/// When each source key's last event arrived there.
	fn received(&self) -> impl Future<Output = eyre::Result<BTreeMap<String, Timestamp>>> + Send;
	/// `call` made as `who`, under `idempotency_key`: the status sites answered.
	fn press(&self, who: &Identity, call: &Call, idempotency_key: &str) -> impl Future<Output = eyre::Result<u16>> + Send;
}

/// A chat an update came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chat {
	pub id: i64,
	/// A one-to-one chat with the bot; groups and channels are never linked.
	pub private: bool,
}

/// What the bot received, as far as the conductor cares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Update {
	/// `/start`, with the payload of a `t.me/<bot>?start=<payload>` link when there is one,
	/// typed (not forwarded) by `from`.
	Start {
		chat: Chat,
		payload: Option<String>,
		from: Account,
	},
	Stop {
		chat: Chat,
	},
	/// Any other message.
	Other {
		chat: Chat,
	},
	/// A button pressed under one of the bot's messages.
	Callback {
		id: String,
		chat_id: i64,
		message_id: i64,
		data: String,
		/// The message as the user sees it, to extend with who pressed.
		message: Rendered,
	},
}

/// What concierge said of a user just now.
#[derive(Clone, Debug)]
pub enum Access {
	Granted(Identity),
	/// No live session to ask with, or concierge refused its token.
	NoSession,
	Unavailable,
}

/// A user's Telegram settings, as the profile shows them.
#[derive(Clone, Debug)]
pub struct Settings {
	pub linked: bool,
	/// Linked, but the bot was blocked: nothing arrives until they link again.
	pub blocked: bool,
	/// The linked Telegram account: `@username`, else its first name.
	pub account: Option<String>,
	/// Every rule the user's permissions open, with whether it is on.
	pub rules: Vec<(Rule, bool)>,
}

/// What one delivery pass did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Delivered {
	pub sent: usize,
	pub retrying: usize,
	pub dead: usize,
}

/// What a recipient is sent.
struct Outgoing {
	text: Rendered,
	links: Vec<(String, String)>,
	buttons: Vec<Button>,
}

/// The rule a note is under, if the note is one the hub can send: its buttons callable and
/// their data within Telegram's bounds, its entities entities.
fn check(note: &SitesNote) -> eyre::Result<Rule> {
	let rule: Rule = note.rule.parse().map_err(|e: Invalid| eyre::eyre!("note {}: {e}", note.seq))?;
	let fail = |why: String| Err(eyre::eyre!("note {} ({rule}): {why}", note.seq));
	for b in &note.buttons {
		if !notify::is_button_id(&b.id) || note.buttons.iter().filter(|o| o.id == b.id).count() > 1 {
			return fail(format!("button id {:?} is not 2–16 of [a-z0-9_], once", b.id));
		}
		if let Some(gone) = b.leaves.iter().find(|l| !note.buttons.iter().any(|o| &o.id == *l)) {
			return fail(format!("button {} leaves {gone:?}, which is not one of the note's", b.id));
		}
		let path = &b.call.path;
		let plain = path.strip_prefix("/api/v1/").is_some_and(|rest| rest.split('/').all(|s| !s.is_empty() && s != "." && s != ".."))
			&& path.bytes().all(|c| c.is_ascii_graphic() && c != b'?' && c != b'#');
		if !matches!(b.call.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") || !plain {
			return fail(format!("button {} calls {} {path}", b.id, b.call.method));
		}
	}
	for t in std::iter::once(&note.text).chain(&note.text_pii) {
		let shaped = |e: &Value| e["type"].is_string() && e["offset"].is_u64() && e["length"].is_u64();
		if let Some(bad) = t.entities.iter().find(|e| !shaped(e)) {
			return fail(format!("{bad} is not a MessageEntity"));
		}
	}
	Ok(rule)
}

fn link_token_hash(token: &str) -> [u8; 32] {
	let mut h = Sha256::new();
	h.update(b"sa-panel/tg-link/v1/");
	h.update(token.as_bytes());
	h.finalize().into()
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

/// A user's link or rules changed. Stamped here: the calls that change them take no clock.
fn telegram_changed(user: Uuid) -> crate::live::Change {
	crate::live::Change {
		topic: "telegram".into(),
		brand: None,
		id: None,
		at: Timestamp::now(),
		audience: crate::live::Audience::User(user),
	}
}

impl Panel {
	/// A one-time token for `t.me/<bot>?start=<token>`, for a signed-in user: 256 random bits,
	/// base64url (the 43 characters a start payload may hold); only its hash is stored, for
	/// [`LINK_TTL`].
	pub async fn telegram_link_token(&self, user: Uuid, permissions: &PermissionSet, display: &str, now: Timestamp) -> eyre::Result<Zeroizing<String>> {
		let mut raw = Zeroizing::new([0u8; 32]);
		getrandom::fill(raw.as_mut_slice()).map_err(|e| eyre::eyre!("the OS random source failed: {e}"))?;
		let token = Zeroizing::new(URL_SAFE_NO_PAD.encode(raw.as_slice()));
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a link token")?;
		db::insert_link_token(&mut conn, &link_token_hash(&token), user, permissions, display, now, now + LINK_TTL).await?;
		Ok(token)
	}

	/// Unlinks the user's chat; `false` when there was none.
	pub async fn telegram_unlink(&self, user: Uuid) -> eyre::Result<bool> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection to unlink")?;
		let gone = db::unlink_user(&mut conn, user).await?;
		if gone {
			self.live.changed(telegram_changed(user));
		}
		Ok(gone)
	}

	pub async fn telegram_settings(&self, user: Uuid, permissions: &PermissionSet) -> eyre::Result<Settings> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for Telegram settings")?;
		let link = db::link_of_user(&mut conn, user).await?;
		let chosen = db::rules(&mut conn, user).await?;
		let rules = Rule::ALL
			.into_iter()
			.filter(|r| r.open_to(permissions))
			.map(|r| (r, chosen.iter().find(|(c, _)| *c == r).map_or(r.on_by_default(), |(_, on)| *on)))
			.collect();
		Ok(Settings {
			linked: link.is_some(),
			blocked: link.as_ref().is_some_and(|l| l.dead),
			account: link.and_then(|l| l.account),
			rules,
		})
	}

	/// Turns rules on or off for the user. A rule their permissions do not open is refused,
	/// not stored: it would only mislead the profile.
	pub async fn telegram_set_rules(&self, user: Uuid, permissions: &PermissionSet, rules: &[(Rule, bool)]) -> eyre::Result<Result<(), Invalid>> {
		if let Some((r, _)) = rules.iter().find(|(r, _)| !r.open_to(permissions)) {
			return Ok(Err(Invalid::new(format!("{r} is not a rule your permissions open"))));
		}
		let mut tx = self.store.begin_write().await?;
		for (rule, on) in rules {
			db::set_rule(&mut tx, user, *rule, *on).await?;
		}
		tx.commit().await.wrap_err("committing a rules change")?;
		self.live.changed(telegram_changed(user));
		Ok(Ok(()))
	}

	/// What concierge just told the `/api/v1` gate of a user: their permissions and name.
	/// Kept on their link, if they have one.
	pub async fn telegram_access_seen(&self, user: Uuid, permissions: &PermissionSet, display: &str, now: Timestamp) -> eyre::Result<()> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection to record access")?;
		db::access_seen(&mut conn, user, permissions, display, now).await
	}

	/// Queues what the conductor's own rules say is due at `now`: silent sources (by `received`,
	/// sites' last receipt per key) and access requests, each told once per recipient. Safe on
	/// several replicas at once: each candidate is claimed, and the outbox refuses a second
	/// `(rule, event, chat)`. `cabinet` is concierge's origin, where the admins grant access. How
	/// many messages were queued.
	pub async fn telegram_fan_out(&self, now: Timestamp, locale: Locale, cabinet: &url::Url, received: &BTreeMap<String, Timestamp>) -> eyre::Result<usize> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for the fan-out")?;
		let mut local = Vec::new();
		for (key_id, kind, created_at) in db::active_sources(&mut conn).await? {
			let last = received.get(&key_id).copied();
			let since = last.unwrap_or(created_at);
			if since >= now - notify::SOURCE_SILENCE {
				continue;
			}
			// One message per full day of the same silence: at 24 h, at 48 h, …
			let days = now.duration_since(since).as_hours() / 24;
			let event = derived_event_id(&["source_silent", &key_id, &since.to_string(), &days.to_string()]);
			local.push((event, Note::SourceSilent { key_id, kind, last }));
		}
		for r in crate::store::access::to_tell(&mut conn, now - crate::access::ASK_AGAIN_AFTER, BATCH).await? {
			let note = Note::AccessRequested {
				email: r.email,
				name: r.name,
				need: r.need,
			};
			local.push((r.event_id, note));
		}
		let mut queued = 0;
		for (event, note) in local {
			let outgoing = |_: &PermissionSet| Outgoing {
				text: note.render(locale),
				links: note.link(locale, cabinet).into_iter().map(|(label, url)| (label.to_owned(), url)).collect(),
				buttons: Vec::new(),
			};
			let mut tx = crate::store::begin_write(&mut conn).await?;
			queued += self.fan_out_one(&mut tx, note.rule(), event, None, outgoing, now, now).await?;
			tx.commit().await.wrap_err("committing a fan-out")?;
		}
		Ok(queued)
	}

	/// The last of sites' notes already in the outbox: where the next page starts.
	pub async fn telegram_notes_after(&self) -> eyre::Result<i64> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for the notes' cursor")?;
		db::notes_after(&mut conn).await
	}

	/// A page of sites' notes, which started after `after`, into the outbox: each note claimed and
	/// queued for its recipients, and the cursor moved to the page's `next`, in one transaction.
	/// A note the hub cannot send (an unknown rule, a button it cannot call) fails the page whole:
	/// the cursor stays, and the next pass stops on it again. How many messages were queued.
	pub async fn telegram_take_notes(&self, page: NotePage, after: i64, now: Timestamp) -> eyre::Result<usize> {
		eyre::ensure!(page.next >= after, "sites answered next {} for notes after {after}", page.next);
		let rules = page.notes.iter().map(check).collect::<eyre::Result<Vec<_>>>()?;
		let mut tx = self.store.begin_write().await?;
		let mut queued = 0;
		for (note, rule) in page.notes.iter().zip(rules) {
			let outgoing = |permissions: &PermissionSet| {
				let text = match &note.text_pii {
					Some(pii) if permissions.may(Pii::See) => pii,
					_ => &note.text,
				};
				Outgoing {
					text: Rendered {
						text: text.text.clone(),
						entities: text.entities.clone(),
					},
					links: Vec::new(),
					buttons: note.buttons.clone(),
				}
			};
			queued += self.fan_out_one(&mut tx, rule, note.event_id, note.skip_user, outgoing, note.created_at, now).await?;
		}
		db::set_notes_after(&mut tx, page.next).await?;
		tx.commit().await.wrap_err("committing a page of notes")?;
		Ok(queued)
	}

	/// One candidate, inside the caller's write transaction: claimed, then queued for everyone
	/// who may hear of it but `skip`, so a claim never stands without its messages.
	#[expect(clippy::too_many_arguments, reason = "the candidate and the pass's context, each named at the call site")]
	async fn fan_out_one(
		&self,
		tx: &mut sqlx::SqliteConnection,
		rule: Rule,
		event: Uuid,
		skip: Option<Uuid>,
		outgoing: impl Fn(&PermissionSet) -> Outgoing,
		noted_at: Timestamp,
		now: Timestamp,
	) -> eyre::Result<usize> {
		if !db::claim_fanout(tx, rule, event, now).await? {
			return Ok(0);
		}
		let mut queued = 0;
		for r in db::recipients(tx, rule, now - ACCESS_TTL).await? {
			if !rule.open_to(&r.permissions) || skip == Some(r.user_id) {
				continue;
			}
			let out = outgoing(&r.permissions);
			let sealed = self.seal_message(rule, event, r.chat_id, &out.text, &out.links)?;
			let message = NewMessage {
				user_id: r.user_id,
				chat_id: r.chat_id,
				rule,
				event_id: event,
				buttons: &out.buttons,
				text_sealed: &sealed,
				data_key_fp: self.key.fingerprint(),
				created_at: noted_at,
			};
			if db::enqueue(tx, &message, now).await? {
				queued += 1;
			}
		}
		if queued > 0 {
			tracing::info!(%rule, %event, queued, "telegram: queued");
		}
		Ok(queued)
	}

	/// A queued message and its links (`(label, url)`), sealed: its text is PII, and so may a
	/// link be.
	fn seal_message(&self, rule: Rule, event: Uuid, chat: i64, m: &Rendered, links: &[(String, String)]) -> eyre::Result<Vec<u8>> {
		let plain = Zeroizing::new(serde_json::to_vec(&serde_json::json!({"text": m.text, "entities": m.entities, "links": links})).wrap_err("serializing a message")?);
		Ok(self.key.seal(&telegram_text_aad(rule.as_str(), event, chat), &plain)?)
	}

	fn open_message(&self, d: &db::Due) -> eyre::Result<(Rendered, Vec<(String, String)>)> {
		#[derive(serde::Deserialize)]
		struct Plain {
			text: String,
			entities: Vec<Value>,
			links: Vec<(String, String)>,
		}
		eyre::ensure!(d.data_key_fp.as_slice() == self.key.fingerprint(), "sealed under another PANEL_DATA_KEY");
		let plain = self.key.open(&telegram_text_aad(&d.rule, d.event_id, d.chat_id), &d.text_sealed).wrap_err("does not open")?;
		let p: Plain = serde_json::from_slice(&plain).wrap_err("is not a message")?;
		Ok((Rendered { text: p.text, entities: p.entities }, p.links))
	}
}

/// The bot's side of the conductor: what it sends and what it does with what it receives.
#[derive(Clone, Debug)]
pub struct Notifier<B, C, S> {
	pub panel: Panel,
	pub bot: B,
	/// concierge: to rotate a session's tokens and to ask `GetMe` with them.
	pub concierge: C,
	/// Where the lead rules' notes come from, and what a button calls.
	pub sites: S,
	pub locale: Locale,
	/// concierge's origin: where the admins grant the access a user asks for.
	pub cabinet: url::Url,
}

impl<B: Bot, C: Refresher + Directory, S: Sites> Notifier<B, C, S> {
	fn buttons<'a>(&self, chat_id: i64, message: i64, buttons: impl IntoIterator<Item = &'a Button>) -> Vec<InlineButton> {
		let key = self.panel.key.telegram_callback_key();
		buttons
			.into_iter()
			.map(|b| InlineButton::Callback {
				label: b.label.clone(),
				data: notify::callback_data(&key, chat_id, message, &b.id),
			})
			.collect()
	}

	/// Queues what is due: the conductor's own rules, then a page of sites' notes.
	pub async fn fan_out(&self, now: Timestamp) -> eyre::Result<usize> {
		let received = self.sites.received().await?;
		let mut queued = self.panel.telegram_fan_out(now, self.locale, &self.cabinet, &received).await?;
		let after = self.panel.telegram_notes_after().await?;
		let page = self.sites.notes(after, NOTES_PAGE).await?;
		queued += self.panel.telegram_take_notes(page, after, now).await?;
		Ok(queued)
	}

	/// Sends what is due at `now`, within the pace (see `store::telegram::claim_due`). First,
	/// lead messages queued over an hour ago are given up, and a chat with more than
	/// [`BACKLOG`] of them waiting gets one summary, without PII, in their place.
	pub async fn deliver(&self, now: Timestamp) -> eyre::Result<Delivered> {
		let due = {
			let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to claim messages")?;
			db::expire_stale(&mut conn, now - STALE_AFTER).await?;
			for (chat, user, _) in db::crowded(&mut conn, BACKLOG, now).await? {
				self.summarize(&mut conn, chat, user, now).await?;
			}
			db::claim_due(&mut conn, now, now + SEND_LEASE).await?
		};
		futures::stream::iter(due)
			.map(|d| self.deliver_one(d, now))
			.buffer_unordered(PARALLEL_SENDS)
			.try_fold(Delivered::default(), |mut sum, one| async move {
				match one {
					Outcome::Sent => sum.sent += 1,
					Outcome::Retrying => sum.retrying += 1,
					Outcome::Dead => sum.dead += 1,
				}
				Ok(sum)
			})
			.await
	}

	async fn summarize(&self, conn: &mut sqlx::SqliteConnection, chat: i64, user: Uuid, now: Timestamp) -> eyre::Result<()> {
		let mut tx = crate::store::begin_write(&mut *conn).await?;
		let count = db::fold_into_summary(&mut tx, chat, now).await?;
		let event = derived_event_id(&["backlog", &chat.to_string(), &now.to_string()]);
		let note = Note::Backlog {
			count: i64::try_from(count).unwrap_or(i64::MAX),
		};
		let sealed = self.panel.seal_message(Rule::NewLead, event, chat, &note.render(self.locale), &[])?;
		let message = NewMessage {
			user_id: user,
			chat_id: chat,
			rule: Rule::NewLead,
			event_id: event,
			buttons: &[],
			text_sealed: &sealed,
			data_key_fp: self.panel.key.fingerprint(),
			created_at: now,
		};
		db::enqueue(&mut tx, &message, now).await?;
		tx.commit().await.wrap_err("committing a summary")?;
		tracing::info!(user_id = %user, count, "telegram: lead messages folded into a summary");
		Ok(())
	}

	/// Whether the message's user may still get it: the same chat, alive, permissions that
	/// open the rule, confirmed within [`ACCESS_TTL`] — asked again at the send, not only when it was
	/// queued, since PII waits in the outbox.
	async fn still_allowed(&self, conn: &mut sqlx::SqliteConnection, d: &db::Due, now: Timestamp) -> eyre::Result<bool> {
		let Ok(rule) = d.rule.parse::<Rule>() else { return Ok(false) };
		let link = db::link_of_user(conn, d.user_id).await?;
		Ok(link.is_some_and(|l| l.chat_id == d.chat_id && !l.dead && l.permissions.as_ref().is_some_and(|p| rule.open_to(p)) && l.permissions_checked_at >= now - ACCESS_TTL))
	}

	async fn deliver_one(&self, d: db::Due, now: Timestamp) -> eyre::Result<Outcome> {
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to deliver")?;
		if !self.still_allowed(&mut conn, &d, now).await? {
			db::dead(&mut conn, d.id, "access not confirmed").await?;
			return Ok(Outcome::Dead);
		}
		let (message, links) = match self.panel.open_message(&d) {
			Ok(m) => m,
			Err(e) => {
				// A message that cannot be read now never will be: it is given up, not retried.
				tracing::warn!(message = d.id, error = format!("{e:#}"), "telegram: a queued message is unreadable");
				db::dead(&mut conn, d.id, &format!("unreadable: {e:#}")).await?;
				return Ok(Outcome::Dead);
			}
		};
		let mut buttons = self.buttons(d.chat_id, d.id, &d.buttons);
		buttons.extend(links.into_iter().map(|(label, url)| InlineButton::Url { label, url }));
		// No pool connection is held while Telegram is asked.
		drop(conn);
		let answer = self.bot.send(d.chat_id, &message, &buttons).await;
		drop(message);
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to deliver")?;
		let failure = match answer {
			Ok(message_id) => {
				db::sent(&mut conn, d.id, message_id, now).await?;
				return Ok(Outcome::Sent);
			}
			Err(f) => f,
		};
		let error = match &failure {
			Failure::RetryAfter(wait) => format!("429, retry after {}s", wait.as_secs()),
			Failure::Transient(why) | Failure::Refused(why) => why.clone(),
			Failure::Blocked => "the chat is gone: blocked, not found or deactivated".to_owned(),
		};
		tracing::warn!(message = d.id, attempts = d.attempts, error, "telegram: a send failed");
		if let Failure::RetryAfter(wait) = failure {
			// The Bot API's limits are the bot's: the whole outbox waits, and the message keeps
			// its tries.
			db::pause(&mut conn, d.id, now.saturating_add(wait.max(notify::PER_CHAT_GAP)).unwrap_or(Timestamp::MAX), &error).await?;
			return Ok(Outcome::Retrying);
		}
		Ok(match notify::after_failure(d.attempts, &failure, now) {
			Next::Retry { at } => {
				db::retry(&mut conn, d.id, d.chat_id, at, &error, false).await?;
				Outcome::Retrying
			}
			Next::Dead => {
				db::dead(&mut conn, d.id, &error).await?;
				Outcome::Dead
			}
			Next::ChatDead => {
				db::chat_dead(&mut conn, d.chat_id, now).await?;
				db::dead(&mut conn, d.id, &error).await?;
				tracing::info!(user_id = %d.user_id, "telegram: the chat is gone; marked dead");
				self.panel.live.changed(telegram_changed(d.user_id));
				Outcome::Dead
			}
		})
	}

	/// Confirms the access of linked users not confirmed for [`RECHECK_AFTER`], and prunes
	/// what is past keeping. How many were asked about.
	pub async fn recheck_access(&self, now: Timestamp) -> eyre::Result<usize> {
		let users = {
			let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection for access checks")?;
			db::prune(&mut conn, now - KEEP).await?;
			db::stale_access(&mut conn, now - RECHECK_AFTER, RECHECKS).await?
		};
		for user in &users {
			let access = self.confirm_access(*user, now).await?;
			tracing::debug!(user_id = %user, ?access, "telegram: access rechecked");
		}
		Ok(users.len())
	}

	/// Asks concierge, now, what the user may do — with their newest panel session used within
	/// [`SESSION_IDLE`], rotated when due — and records the answer on their link. A session
	/// concierge refuses (to rotate, or `GetMe` twice running) ends the user's notifications
	/// and drops what was queued for them.
	pub async fn confirm_access(&self, user: Uuid, now: Timestamp) -> eyre::Result<Access> {
		let panel = &self.panel;
		let conn = || async { panel.store.pool().acquire().await.wrap_err("a connection to record access") };
		let session = match panel.session_of_user(user, now, now - SESSION_IDLE, &self.concierge).await {
			Ok(s) => s,
			Err(SessionError::Missing) => {
				self.tried(user, now).await?;
				return Ok(Access::NoSession);
			}
			Err(SessionError::Rejected) => {
				db::access_lost(&mut *conn().await?, user, now).await?;
				return Ok(Access::NoSession);
			}
			Err(SessionError::Unavailable) => {
				self.tried(user, now).await?;
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
				tracing::warn!(why, "telegram: concierge unavailable for an access check");
				self.tried(user, now).await?;
				Ok(Access::Unavailable)
			}
			Err(e @ DirectoryError::Failed(_)) => Err(eyre::eyre!(e)),
		}
	}

	async fn tried(&self, user: Uuid, now: Timestamp) -> eyre::Result<()> {
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to record access")?;
		db::access_tried(&mut conn, user, now).await
	}

	async fn say(&self, chat: i64, text: String) {
		let message = Rendered { text, entities: Vec::new() };
		if let Err(f) = self.bot.send(chat, &message, &[]).await {
			tracing::warn!(failure = ?f, "telegram: a reply was not delivered");
		}
	}

	async fn reply(&self, chat: i64, reply: Reply) {
		self.say(chat, reply.text(self.locale).to_owned()).await;
	}

	/// A reply nobody asked a linked chat for (help, "not linked", …): at most one per chat per
	/// [`REPLY_EVERY`], so a stranger cannot make the bot spend its rate on them.
	async fn reply_unsolicited(&self, chat: i64, reply: Reply, now: Timestamp) -> eyre::Result<()> {
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to throttle a reply")?;
		if db::may_reply(&mut conn, chat, now, REPLY_EVERY).await? {
			drop(conn);
			self.reply(chat, reply).await;
		}
		Ok(())
	}

	/// Handles one update. Groups and channels are ignored whatever they say, and so is any
	/// message that is not a command: the panel only ever talks to a person, one to one, and
	/// only when asked.
	pub async fn handle(&self, update: Update, now: Timestamp) -> eyre::Result<()> {
		match update {
			Update::Start { chat, .. } | Update::Stop { chat } | Update::Other { chat } if !chat.private => Ok(()),
			Update::Other { .. } => Ok(()),
			Update::Start { chat, payload: Some(token), from } => self.start(chat.id, token.trim(), &from, now).await,
			Update::Start { chat, payload: None, .. } => self.reply_unsolicited(chat.id, Reply::Help, now).await,
			Update::Stop { chat } => {
				let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to unlink")?;
				let gone = db::unlink_chat(&mut conn, chat.id).await?;
				drop(conn);
				if let Some(user) = gone {
					self.panel.live.changed(telegram_changed(user));
					self.reply(chat.id, Reply::Unlinked).await;
					Ok(())
				} else {
					self.reply_unsolicited(chat.id, Reply::NotLinked, now).await
				}
			}
			Update::Callback {
				id,
				chat_id,
				message_id,
				data,
				message,
			} => {
				let (reply, edit) = self.press(chat_id, &data, now).await?;
				if let Some(Pressed { outbox, line, left }) = edit {
					let mut edited = message;
					edited.push("\n\n");
					edited.push(&line);
					let buttons = self.buttons(chat_id, outbox, &left);
					if let Err(f) = self.bot.edit(chat_id, message_id, &edited, &buttons).await {
						tracing::warn!(failure = ?f, "telegram: a pressed message was not updated");
					}
				}
				if let Err(f) = self.bot.answer(&id, reply.text(self.locale)).await {
					tracing::warn!(failure = ?f, "telegram: a button press was not answered");
				}
				Ok(())
			}
		}
	}

	/// `/start <token>`: the token redeemed and the chat linked in one transaction — rolled
	/// back, the token kept, when the chat is another panel account's.
	async fn start(&self, chat: i64, token: &str, from: &Account, now: Timestamp) -> eyre::Result<()> {
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to link")?;
		let mut tx = crate::store::begin_write(&mut conn).await?;
		let Some(redeemed) = db::redeem_link_token(&mut tx, &link_token_hash(token), now).await? else {
			tx.commit().await.wrap_err("committing a spent token")?;
			drop(conn);
			return self.reply_unsolicited(chat, Reply::LinkInvalid, now).await;
		};
		if db::link_of_chat(&mut tx, chat).await?.is_some_and(|l| l.user_id != redeemed.user_id) {
			drop(tx);
			drop(conn);
			tracing::info!(user_id = %redeemed.user_id, "telegram: a link refused, the chat is another account's");
			self.reply(chat, Reply::ChatTaken).await;
			return Ok(());
		}
		db::link(&mut tx, &redeemed, chat, from, now).await?;
		tx.commit().await.wrap_err("committing a link")?;
		drop(conn);
		tracing::info!(user_id = %redeemed.user_id, "telegram: chat linked");
		self.panel.live.changed(telegram_changed(redeemed.user_id));
		self.say(chat, notify::linked_text(self.locale, &redeemed.display_name)).await;
		Ok(())
	}

	/// A button pressed in `chat_id`: what to answer, and — when something was recorded now —
	/// how the message changes. Nothing in the button is trusted but its signature: the message
	/// it names must be this chat's, the chat must be linked to the message's user, and that
	/// user's access is asked of concierge now; sites then judges the call as that user's.
	async fn press(&self, chat_id: i64, data: &str, now: Timestamp) -> eyre::Result<(Reply, Option<Pressed>)> {
		let Some((outbox, id)) = notify::parse_callback(&self.panel.key.telegram_callback_key(), chat_id, data) else {
			tracing::warn!("telegram: a button with data the panel did not sign");
			return Ok((Reply::BadButton, None));
		};
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection for a button")?;
		let message = db::message(&mut conn, outbox).await?;
		let link = db::link_of_chat(&mut conn, chat_id).await?;
		drop(conn);
		let Some((message, button)) = message
			.filter(|m| m.chat_id == chat_id)
			.and_then(|m| m.buttons.iter().find(|b| b.id == id).cloned().map(|b| (m, b)))
		else {
			return Ok((Reply::BadButton, None));
		};
		let Some(link) = link.filter(|l| l.user_id == message.user_id && !l.dead) else {
			return Ok((Reply::NotLinked, None));
		};
		let identity = match self.confirm_access(link.user_id, now).await? {
			Access::Granted(identity) => identity,
			Access::NoSession => return Ok((Reply::OpenPanel, None)),
			Access::Unavailable => return Ok((Reply::TryLater, None)),
		};
		// The same button of the same message is one action, however often it is pressed or
		// its update delivered: sites derives its event ids from this key.
		let key = format!("tg/{outbox}/{}", button.id);
		let status = match self.sites.press(&identity, &button.call, &key).await {
			Ok(status) => status,
			Err(e) => {
				tracing::warn!(error = format!("{e:#}"), "telegram: sites did not take a press");
				return Ok((Reply::TryLater, None));
			}
		};
		Ok(match status {
			201 => {
				tracing::info!(user_id = %link.user_id, button = button.id, "telegram: a button recorded");
				let line = format!("{}: {}", button.pressed, notify::one_line(identity.display_name(), MAX_NAME));
				let left = message.buttons.into_iter().filter(|b| button.leaves.contains(&b.id)).collect();
				(Reply::Done, Some(Pressed { outbox, line, left }))
			}
			200 | 400 | 409 => (Reply::AlreadyDone, None),
			404 => (Reply::LeadGone, None),
			403 => (Reply::NoAccess, None),
			other => {
				tracing::warn!(status = other, button = button.id, "telegram: sites refused a press");
				(Reply::TryLater, None)
			}
		})
	}
}

/// A press sites recorded: the message's line saying so, and the buttons it leaves.
struct Pressed {
	outbox: i64,
	line: String,
	left: Vec<Button>,
}

enum Outcome {
	Sent,
	Retrying,
	Dead,
}

impl Panel {
	/// Takes or renews the poller's lease for `holder`: the update id to ask Telegram for
	/// next, or `None` while another replica polls.
	pub async fn telegram_poll_lease(&self, holder: Uuid, now: Timestamp, until: Timestamp) -> eyre::Result<Option<i64>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for the poller")?;
		db::poll_lease(&mut conn, holder, now, until).await
	}

	/// Records that updates before `next_update_id` are handled; `false` when `holder` has
	/// lost the lease meanwhile.
	pub async fn telegram_poll_advance(&self, holder: Uuid, next_update_id: i64) -> eyre::Result<bool> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for the poller")?;
		db::poll_advance(&mut conn, holder, next_update_id).await
	}

	pub async fn telegram_poll_release(&self, holder: Uuid) -> eyre::Result<()> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for the poller")?;
		db::poll_release(&mut conn, holder).await
	}
}

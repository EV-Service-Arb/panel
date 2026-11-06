//! Telegram, the method a destination is reached by: a user's private chat linked by `/start
//! <token>`, a channel, group or forum topic they administer, the outbox delivered at Telegram's
//! pace, and what the bot does with the updates it receives — `/start <token>`, `/stop` and the
//! buttons under a message.
//!
//! ```text
//! POST /api/v1/alerts/telegram/link ─ token (256 bits; its hash, 10 min) ─ t.me/<bot>?start=<token>
//!                         /start <token> in a private chat ─ the user's private destination
//! a channel's paste ─ getChat ─ getChatMember(the user's private chat id): an admin ─ a test sent
//! outbox ─ claim (1/s per private chat, 20/min per channel or group, ≤ 25/s in all, across
//!          replicas) ─ sendMessage | sendPhoto ─ sent | retry | dead
//! button ─ signed data ─ the chat's user, access asked of concierge now ─ sites, as that user
//! ```
//!
//! PII and buttons go only to a private chat: a channel is read by whoever its admins let in.
//! The Bot API is the server's; this sees it through [`Bot`].

use std::future::Future;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use eyre::WrapErr;
use futures::{StreamExt, TryStreamExt};
use jiff::{SignedDuration, Timestamp};
use sa_alerts::{Button, Rule};
use sa_auth::PermissionSet;
use sa_core::notify::{self, Failure, MAX_NAME, Next, Note, Rendered, Reply};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

pub use crate::store::telegram::{Account, Kind};
use crate::{
	Panel, Refused,
	alerts::{Access, Directory, Feeds, Hub, Outgoing, alerts_changed, derived_event_id},
	seal::telegram_text_aad,
	session::Refresher,
	store::{
		alerts as hub_db,
		telegram::{self as db, NewMessage},
	},
};

/// How long a link token may wait for its `/start`.
pub const LINK_TTL: SignedDuration = SignedDuration::from_mins(10);

/// At most one unsolicited reply (help, "not linked", …) per chat this often.
pub const REPLY_EVERY: SignedDuration = SignedDuration::from_mins(10);

/// Past this many of one rule's messages waiting for one destination, they are folded into one
/// summary.
pub const BACKLOG: i64 = 20;

/// How long a replica sending a message holds it: past the Bot API's timeout, so it lapses
/// only when the replica is gone.
pub const SEND_LEASE: SignedDuration = SignedDuration::from_secs(60);

/// A lead message noted longer ago than this is news nobody needs.
const STALE_AFTER: SignedDuration = SignedDuration::from_hours(1);

/// Messages sent at once in a pass.
const PARALLEL_SENDS: usize = 8;

/// The Bot API, as the method needs it. Every call answers the failure as [`notify::Failure`],
/// so the outbox can decide what becomes of the message.
pub trait Bot: Sync {
	/// Sends a plain-text message with its entities, or a photo with it as the caption; the id
	/// Telegram gave it.
	fn send(&self, to: Target, message: &Rendered, buttons: &[InlineButton], photo: Option<Vec<u8>>) -> impl Future<Output = Result<i64, Failure>> + Send;
	/// Replaces a message's text and buttons (none: the keyboard is removed).
	fn edit(&self, chat_id: i64, message_id: i64, message: &Rendered, buttons: &[InlineButton]) -> impl Future<Output = Result<(), Failure>> + Send;
	/// Answers a button press, with a short notice the user sees.
	fn answer(&self, callback_id: &str, text: &str) -> impl Future<Output = Result<(), Failure>> + Send;
	/// `getChat`: `chat` is `@name` or a numeric id.
	fn chat(&self, chat: &str) -> impl Future<Output = Result<ChatInfo, Failure>> + Send;
	/// Whether `user` administers `chat` (`getChatMember`).
	fn is_admin(&self, chat_id: i64, user_id: i64) -> impl Future<Output = Result<bool, Failure>> + Send;
}

/// A button under a message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InlineButton {
	/// Pressed, it comes back to the bot with its signed data.
	Callback { label: String, data: String },
	/// Opens `url`.
	Url { label: String, url: String },
}

/// Where in Telegram a message goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
	pub chat_id: i64,
	/// A forum's topic.
	pub topic_id: Option<i64>,
}

/// What `getChat` says of a chat.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatInfo {
	pub id: i64,
	/// `private`, `group`, `supergroup` or `channel`.
	pub kind: String,
	pub title: Option<String>,
	pub forum: bool,
}

/// A chat an update came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chat {
	pub id: i64,
	/// A one-to-one chat with the bot; groups and channels are never linked by a command.
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

/// What one delivery pass did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Delivered {
	pub sent: usize,
	pub retrying: usize,
	pub dead: usize,
}

fn link_token_hash(token: &str) -> [u8; 32] {
	let mut h = Sha256::new();
	h.update(b"sa-panel/tg-link/v1/");
	h.update(token.as_bytes());
	h.finalize().into()
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

	/// A message for a destination of `user`'s, at `to`, into the outbox, inside the hub's
	/// transaction; `false` when it was queued already.
	#[expect(clippy::too_many_arguments, reason = "the message and the pass's context, each named at the call site")]
	pub(crate) async fn queue(
		&self,
		tx: &mut sqlx::SqliteConnection,
		(destination_id, user_id, to): (i64, Uuid, Target),
		rule: Rule,
		event: Uuid,
		out: &Outgoing,
		noted_at: Timestamp,
		now: Timestamp,
	) -> eyre::Result<bool> {
		let sealed = self.seal_message(rule, event, to, &out.text, &out.links)?;
		let message = NewMessage {
			destination_id,
			user_id,
			chat_id: to.chat_id,
			topic_id: to.topic_id,
			rule,
			event_id: event,
			buttons: &out.buttons,
			photo: out.photo.as_deref(),
			text_sealed: &sealed,
			data_key_fp: self.key.fingerprint(),
			created_at: noted_at,
		};
		db::enqueue(tx, &message, now).await
	}

	/// A queued message and its links (`(label, url)`), sealed: its text is PII, and so may a
	/// link be.
	fn seal_message(&self, rule: Rule, event: Uuid, to: Target, m: &Rendered, links: &[(String, String)]) -> eyre::Result<Vec<u8>> {
		let plain = Zeroizing::new(serde_json::to_vec(&serde_json::json!({"text": m.text, "entities": m.entities, "links": links})).wrap_err("serializing a message")?);
		Ok(self.key.seal(&telegram_text_aad(rule.as_str(), event, to.chat_id, to.topic_id), &plain)?)
	}

	fn open_message(&self, d: &db::Due) -> eyre::Result<(Rendered, Vec<(String, String)>)> {
		#[derive(serde::Deserialize)]
		struct Plain {
			text: String,
			entities: Vec<Value>,
			links: Vec<(String, String)>,
		}
		eyre::ensure!(d.data_key_fp.as_slice() == self.key.fingerprint(), "sealed under another PANEL_DATA_KEY");
		let plain = self
			.key
			.open(&telegram_text_aad(d.rule.as_str(), d.event_id, d.chat_id, d.topic_id), &d.text_sealed)
			.wrap_err("does not open")?;
		let p: Plain = serde_json::from_slice(&plain).wrap_err("is not a message")?;
		Ok((Rendered { text: p.text, entities: p.entities }, p.links))
	}

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

impl<B: Bot, C: Refresher + Directory, F: Feeds> Hub<B, C, F> {
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

	/// Adds a channel, a group or a forum topic, as `paste` names it, as a destination of the
	/// user's: only once the bot sees it, the user administers it (asked with their private chat's
	/// id — they link that first) and a test went out there. Nothing is stored otherwise.
	pub async fn add_chat(&self, user: Uuid, permissions: &PermissionSet, display: &str, paste: &str, gmail: Option<i64>, now: Timestamp) -> eyre::Result<Result<i64, Refused>> {
		let refuse = |why: String| Ok(Err(Refused::Invalid(why)));
		let named: tg_types::TelegramDestination = match paste.trim().parse() {
			Ok(named) => named,
			Err(e) => return refuse(format!("not a channel, group or topic: {e}")),
		};
		let (chat, topic) = match &named {
			tg_types::TelegramDestination::Channel(id) | tg_types::TelegramDestination::Group(id) => (id.chat_id_param(), None),
			tg_types::TelegramDestination::GroupTopic { group, topic_id } => (group.chat_id_param(), Some(i64::try_from(*topic_id)?)),
		};
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to add a chat")?;
		let Some((_, own_chat)) = db::private_of_user(&mut conn, user).await? else {
			return refuse("link your own Telegram first: the bot asks Telegram whether that account administers the chat".into());
		};
		if let Some(gmail) = gmail
			&& !hub_db::owns_gmail(&mut conn, user, gmail).await?
		{
			return Ok(Err(Refused::NotFound));
		}
		drop(conn);
		let info = match self.bot.chat(&chat).await {
			Ok(info) => info,
			Err(f) => return refuse(format!("the bot cannot see {chat}: add it there first ({})", failure_text(&f))),
		};
		let kind = match info.kind.as_str() {
			"channel" => Kind::Channel,
			"group" | "supergroup" => Kind::Group,
			_ => return refuse(format!("{chat} is a private chat; link your own with the link button")),
		};
		if topic.is_some() && !info.forum {
			return refuse(format!("{chat} has no topics"));
		}
		match self.bot.is_admin(info.id, own_chat).await {
			Ok(true) => {}
			Ok(false) => return refuse(format!("your linked Telegram account does not administer {chat}")),
			Err(f) => return refuse(format!("Telegram did not say whether you administer {chat} ({})", failure_text(&f))),
		}
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to add a chat")?;
		if db::chat_taken(&mut conn, info.id, topic).await? {
			return Ok(Err(Refused::Conflict(format!("{chat} is a destination already"))));
		}
		drop(conn);
		let to = Target { chat_id: info.id, topic_id: topic };
		let test = Rendered {
			text: notify::test_text(self.locale).to_owned(),
			entities: Vec::new(),
		};
		if let Err(f) = self.bot.send(to, &test, &[], None).await {
			return refuse(format!("the bot cannot post in {chat}: {}", failure_text(&f)));
		}
		let mut tx = self.panel.store.begin_write().await?;
		if db::chat_taken(&mut tx, info.id, topic).await? {
			return Ok(Err(Refused::Conflict(format!("{chat} is a destination already"))));
		}
		let destination = hub_db::add(&mut tx, user, gmail, now).await?;
		db::insert(&mut tx, destination, kind, info.id, topic, info.title.as_deref()).await?;
		hub_db::access_seen(&mut tx, user, permissions, display, now).await?;
		tx.commit().await.wrap_err("committing a chat")?;
		tracing::info!(user_id = %user, destination, "telegram: a chat added");
		self.panel.live.changed(alerts_changed(user));
		Ok(Ok(destination))
	}

	/// Sends a test to one of the user's destinations; one that arrives makes it reached again.
	pub async fn test(&self, user: Uuid, destination: i64) -> eyre::Result<Result<(), Refused>> {
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to test a destination")?;
		let Some(d) = hub_db::destination_of(&mut conn, user, destination).await? else {
			return Ok(Err(Refused::NotFound));
		};
		drop(conn);
		let test = Rendered {
			text: notify::test_text(self.locale).to_owned(),
			entities: Vec::new(),
		};
		let to = Target {
			chat_id: d.chat_id,
			topic_id: d.topic_id,
		};
		if let Err(f) = self.bot.send(to, &test, &[], None).await {
			return Ok(Err(Refused::Invalid(format!("Telegram refused the test: {}", failure_text(&f)))));
		}
		// A chat's title is what the bot sees once it is in: an imported one's is learned here; one
		// Telegram does not describe keeps what it had, the test having reached it all the same.
		let seen = match d.kind {
			Kind::Private => None,
			Kind::Channel | Kind::Group => self.bot.chat(&d.chat_id.to_string()).await.ok(),
		};
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to test a destination")?;
		if let Some(info) = seen {
			let kind = if info.kind == "channel" { Kind::Channel } else { Kind::Group };
			db::describe(&mut conn, destination, kind, info.title.as_deref()).await?;
		}
		db::revive(&mut conn, destination).await?;
		self.panel.live.changed(alerts_changed(user));
		Ok(Ok(()))
	}

	/// Sends what is due at `now`, within the pace (see `store::telegram::claim_due`). First,
	/// lead messages queued over an hour ago are given up, and a destination with more than
	/// [`BACKLOG`] of one rule's messages waiting gets one summary, without PII, in their place.
	pub async fn deliver(&self, now: Timestamp) -> eyre::Result<Delivered> {
		let due = {
			let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to claim messages")?;
			db::expire_stale(&mut conn, now - STALE_AFTER).await?;
			for crowded in db::crowded(&mut conn, BACKLOG, now).await? {
				self.summarize(&mut conn, crowded, now).await?;
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

	async fn summarize(&self, conn: &mut sqlx::SqliteConnection, c: db::Crowded, now: Timestamp) -> eyre::Result<()> {
		let mut tx = crate::store::begin_write(&mut *conn).await?;
		let count = db::fold_into_summary(&mut tx, c.destination_id, c.rule, now).await?;
		let event = derived_event_id(&["backlog", &c.destination_id.to_string(), c.rule.as_str(), &now.to_string()]);
		let note = Note::Backlog { rule: c.rule, count };
		let out = Outgoing {
			text: note.render(self.locale),
			links: Vec::new(),
			buttons: Vec::new(),
			photo: None,
		};
		let to = Target {
			chat_id: c.chat_id,
			topic_id: c.topic_id,
		};
		self.panel.queue(&mut tx, (c.destination_id, c.user_id, to), c.rule, event, &out, now, now).await?;
		tx.commit().await.wrap_err("committing a summary")?;
		tracing::info!(destination = c.destination_id, rule = %c.rule, count, "telegram: messages folded into a summary");
		Ok(())
	}

	/// Whether the message may still go: its destination reached at the same chat, its user's
	/// access confirmed within the TTL and opening the rule — asked again at the send, not only
	/// when it was queued, since PII waits in the outbox.
	async fn still_allowed(&self, conn: &mut sqlx::SqliteConnection, d: &db::Due, now: Timestamp) -> eyre::Result<bool> {
		let reached = db::reach(conn, d.destination_id)
			.await?
			.is_some_and(|(chat, topic, alive)| alive && chat == d.chat_id && topic == d.topic_id);
		let access = hub_db::access(conn, d.user_id).await?;
		Ok(reached && access.is_some_and(|(permissions, confirmed)| confirmed >= now - crate::alerts::ACCESS_TTL && permissions.is_some_and(|p| d.rule.open_to(&p))))
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
		// No pool connection is held while a service or Telegram is asked.
		drop(conn);
		let to = Target {
			chat_id: d.chat_id,
			topic_id: d.topic_id,
		};
		let answer = match &d.photo {
			Some(path) => match self.feeds.photo(d.rule.origin(), path).await {
				Ok(photo) => self.bot.send(to, &message, &buttons, Some(photo)).await,
				Err(e) => Err(Failure::Transient(format!("the photo {path}: {e:#}"))),
			},
			None => self.bot.send(to, &message, &buttons, None).await,
		};
		drop(message);
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to deliver")?;
		let failure = match answer {
			Ok(message_id) => {
				db::sent(&mut conn, d.id, message_id, now).await?;
				return Ok(Outcome::Sent);
			}
			Err(f) => f,
		};
		let error = failure_text(&failure);
		tracing::warn!(message = d.id, attempts = d.attempts, error, "telegram: a send failed");
		if let Failure::RetryAfter(wait) = failure {
			// The Bot API's limits are the bot's: the whole outbox waits, and the message keeps
			// its tries.
			db::pause(&mut conn, d.id, now.saturating_add(wait.max(notify::PER_CHAT_GAP)).unwrap_or(Timestamp::MAX), &error).await?;
			return Ok(Outcome::Retrying);
		}
		Ok(match notify::after_failure(d.attempts, &failure, now) {
			Next::Retry { at } => {
				db::retry(&mut conn, d.id, at, &error).await?;
				Outcome::Retrying
			}
			Next::Dead => {
				db::dead(&mut conn, d.id, &error).await?;
				Outcome::Dead
			}
			Next::ChatDead => {
				let users = db::chat_dead(&mut conn, d.chat_id, &error, now).await?;
				db::dead(&mut conn, d.id, &error).await?;
				tracing::info!(user_id = %d.user_id, "telegram: the chat is gone; marked dead");
				for user in users {
					self.panel.live.changed(alerts_changed(user));
				}
				Outcome::Dead
			}
		})
	}

	async fn say(&self, chat: i64, text: String) {
		let message = Rendered { text, entities: Vec::new() };
		if let Err(f) = self.bot.send(Target { chat_id: chat, topic_id: None }, &message, &[], None).await {
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
	/// message that is not a command: the bot only ever talks to a person, one to one, and
	/// only when asked.
	pub async fn handle(&self, update: Update, now: Timestamp) -> eyre::Result<()> {
		match update {
			Update::Start { chat, .. } | Update::Stop { chat } | Update::Other { chat } if !chat.private => Ok(()),
			Update::Other { .. } => Ok(()),
			Update::Start { chat, payload: Some(token), from } => self.start(chat.id, token.trim(), &from, now).await,
			Update::Start { chat, payload: None, .. } => self.reply_unsolicited(chat.id, Reply::Help, now).await,
			Update::Stop { chat } => {
				let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection to unlink")?;
				let gone = db::unlink_private(&mut conn, chat.id).await?;
				drop(conn);
				if let Some(user) = gone {
					self.panel.live.changed(alerts_changed(user));
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
		if db::owner_of_private(&mut tx, chat).await?.is_some_and(|user| user != redeemed.user_id) {
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
		self.panel.live.changed(alerts_changed(redeemed.user_id));
		self.say(chat, notify::linked_text(self.locale, &redeemed.display_name)).await;
		Ok(())
	}

	/// A button pressed in `chat_id`: what to answer, and — when something was recorded now —
	/// how the message changes. Nothing in the button is trusted but its signature: the message
	/// it names must be this chat's, the chat must be the private chat of the message's user, and
	/// that user's access is asked of concierge now; sites then judges the call as that user's.
	async fn press(&self, chat_id: i64, data: &str, now: Timestamp) -> eyre::Result<(Reply, Option<Pressed>)> {
		let Some((outbox, id)) = notify::parse_callback(&self.panel.key.telegram_callback_key(), chat_id, data) else {
			tracing::warn!("telegram: a button with data the panel did not sign");
			return Ok((Reply::BadButton, None));
		};
		let mut conn = self.panel.store.pool().acquire().await.wrap_err("a connection for a button")?;
		let message = db::message(&mut conn, outbox).await?;
		let owner = db::owner_of_private(&mut conn, chat_id).await?;
		drop(conn);
		let Some((message, button)) = message
			.filter(|m| m.chat_id == chat_id)
			.and_then(|m| m.buttons.iter().find(|b| b.id == id).cloned().map(|b| (m, b)))
		else {
			return Ok((Reply::BadButton, None));
		};
		if owner != Some(message.user_id) {
			return Ok((Reply::NotLinked, None));
		}
		let identity = match self.confirm_access(message.user_id, now).await? {
			Access::Granted(identity) => identity,
			Access::NoSession => return Ok((Reply::OpenPanel, None)),
			Access::Unavailable => return Ok((Reply::TryLater, None)),
		};
		// The same button of the same message is one action, however often it is pressed or
		// its update delivered: sites derives its event ids from this key.
		let key = format!("tg/{outbox}/{}", button.id);
		let status = match self.feeds.press(&identity, &button.call, &key).await {
			Ok(status) => status,
			Err(e) => {
				tracing::warn!(error = format!("{e:#}"), "telegram: sites did not take a press");
				return Ok((Reply::TryLater, None));
			}
		};
		Ok(match status {
			201 => {
				tracing::info!(user_id = %message.user_id, button = button.id, "telegram: a button recorded");
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

fn failure_text(f: &Failure) -> String {
	match f {
		Failure::RetryAfter(wait) => format!("429, retry after {}s", wait.as_secs()),
		Failure::Transient(why) | Failure::Refused(why) => why.clone(),
		Failure::Blocked => "the bot is blocked, or the chat is gone".to_owned(),
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

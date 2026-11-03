//! The conductor's Telegram bot: the Bot API over HTTPS, and the updates it polls for. What a
//! message says and where it goes, and what a button does, is the engine's (`panel::alerts`,
//! `panel::telegram`).
//!
//! **Updates arrive by long polling, not a webhook.** A webhook would need a public route and a
//! secret header check on it — surface for anyone on the internet — while `getUpdates` needs
//! only egress to `api.telegram.org`. Telegram answers a second poller 409, so one replica
//! polls, under a lease in the database that another takes over when it lapses; the offset lives
//! there too, so a new holder resumes where the last one stopped. Every update is handled
//! idempotently (a link token redeems once, a button's press carries the same Idempotency-Key),
//! so an update handled twice across a takeover does nothing twice.
//!
//! Without `TELEGRAM_BOT_TOKEN` none of this runs: `serve` warns, and the Alerts page answers
//! that Telegram is not configured.

use std::{
	sync::{Arc, OnceLock},
	time::Duration,
};

use jiff::{SignedDuration, Timestamp};
use panel::{
	alerts::{Feeds, Hub},
	telegram::{Account, Bot, Chat, ChatInfo, InlineButton, Target, Update},
};
use panel_core::notify::{Failure, Rendered};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::concierge::Concierge;

/// The Bot API's root.
pub const API_BASE: &str = "https://api.telegram.org";

/// How long `getUpdates` holds a poll open when nothing arrives.
const POLL_SECONDS: u64 = 25;

/// The poller's lease: renewed every poll, so it lapses only when its holder is gone.
const POLL_LEASE: SignedDuration = SignedDuration::from_secs(60);

/// A replica without the poller's lease asks for it again this often.
const STANDBY_EVERY: Duration = Duration::from_secs(10);

/// The Bot API, over HTTPS.
#[derive(Clone)]
pub struct BotApi {
	http: reqwest::Client,
	base: String,
	token: Arc<Zeroizing<String>>,
}

impl std::fmt::Debug for BotApi {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("BotApi").field("base", &self.base).finish_non_exhaustive()
	}
}

#[derive(Deserialize)]
struct Answer {
	ok: bool,
	result: Option<Value>,
	description: Option<String>,
	parameters: Option<Parameters>,
}

#[derive(Deserialize)]
struct Parameters {
	retry_after: Option<i64>,
}

impl BotApi {
	/// `base`: [`API_BASE`], or a test's mock.
	pub fn new(base: &str, token: &str) -> eyre::Result<Self> {
		let http = reqwest::Client::builder()
			.connect_timeout(Duration::from_secs(5))
			.timeout(Duration::from_secs(15))
			.redirect(reqwest::redirect::Policy::none())
			.build()?;
		Ok(Self {
			http,
			base: base.trim_end_matches('/').to_owned(),
			token: Arc::new(Zeroizing::new(token.to_owned())),
		})
	}

	fn request(&self, method: &str) -> reqwest::RequestBuilder {
		self.http.post(format!("{}/bot{}/{method}", self.base, self.token.as_str()))
	}

	/// Calls a method with a JSON body; its `result`, or how it failed. `timeout` overrides the
	/// client's, for the long poll.
	async fn call(&self, method: &str, body: &Value, timeout: Option<Duration>) -> Result<Value, Failure> {
		let mut req = self.request(method).json(body);
		if let Some(t) = timeout {
			req = req.timeout(t);
		}
		answer(method, req).await
	}

	/// The bot's username (`getMe`), for the `t.me/<bot>` links.
	pub async fn username(&self) -> Result<String, Failure> {
		let me = self.call("getMe", &json!({}), None).await?;
		me.get("username")
			.and_then(Value::as_str)
			.map(str::to_owned)
			.ok_or_else(|| Failure::Refused("getMe: no username".into()))
	}

	/// The updates from `offset` on, waiting up to `wait` for one: `(update_id, raw)`.
	pub async fn updates(&self, offset: i64, wait: Duration) -> Result<Vec<(i64, Value)>, Failure> {
		let body = json!({"offset": offset, "timeout": wait.as_secs(), "allowed_updates": ["message", "callback_query"]});
		let result = self.call("getUpdates", &body, Some(wait + Duration::from_secs(10))).await?;
		let Value::Array(all) = result else {
			return Err(Failure::Refused("getUpdates: not a list".into()));
		};
		Ok(all.into_iter().filter_map(|u| Some((u.get("update_id")?.as_i64()?, u))).collect())
	}
}

/// What Telegram answered `method`: its `result`, or how it failed.
async fn answer(method: &str, req: reqwest::RequestBuilder) -> Result<Value, Failure> {
	// The URL carries the token: reqwest's error would print it.
	let res = req.send().await.map_err(|e| Failure::Transient(format!("{method}: {}", e.without_url())))?;
	let status = res.status();
	// An answer that is not the Bot API's shape is judged by its status alone.
	let answer: Option<Answer> = res.json().await.ok();
	if status.is_success()
		&& let Some(Answer { ok: true, result, .. }) = answer
	{
		return Ok(result.unwrap_or(Value::Null));
	}
	let why = answer.as_ref().and_then(|a| a.description.clone()).unwrap_or_default();
	Err(match status.as_u16() {
		429 => {
			let secs = answer.and_then(|a| a.parameters).and_then(|p| p.retry_after).unwrap_or(1);
			Failure::RetryAfter(SignedDuration::from_secs(secs.clamp(1, 3600)))
		}
		403 => Failure::Blocked,
		// The chat is gone as surely as a blocked one: no try will reach it.
		400 if why.contains("chat not found") || why.contains("user is deactivated") => Failure::Blocked,
		500..=599 => Failure::Transient(format!("{method}: {status} {why}")),
		_ => Failure::Refused(format!("{method}: {status} {why}")),
	})
}

fn keyboard(buttons: &[InlineButton]) -> Value {
	let row = |b: &InlineButton| match b {
		InlineButton::Callback { label, data } => json!([{"text": label, "callback_data": data}]),
		InlineButton::Url { label, url } => json!([{"text": label, "url": url}]),
	};
	json!({"inline_keyboard": buttons.iter().map(row).collect::<Vec<_>>()})
}

impl Bot for BotApi {
	async fn send(&self, to: Target, message: &Rendered, buttons: &[InlineButton], photo: Option<Vec<u8>>) -> Result<i64, Failure> {
		let (method, sent) = match photo {
			None => {
				let mut body = json!({"chat_id": to.chat_id, "text": message.text, "entities": message.entities, "link_preview_options": {"is_disabled": true}});
				if let Some(topic) = to.topic_id {
					body["message_thread_id"] = json!(topic);
				}
				if !buttons.is_empty() {
					body["reply_markup"] = keyboard(buttons);
				}
				("sendMessage", self.call("sendMessage", &body, None).await)
			}
			Some(photo) => {
				let part = reqwest::multipart::Part::bytes(photo).file_name("photo.png").mime_str("image/png").expect("a valid MIME type");
				let mut form = reqwest::multipart::Form::new()
					.text("chat_id", to.chat_id.to_string())
					.text("caption", message.text.clone())
					.text("caption_entities", Value::from(message.entities.clone()).to_string())
					.part("photo", part);
				if let Some(topic) = to.topic_id {
					form = form.text("message_thread_id", topic.to_string());
				}
				if !buttons.is_empty() {
					form = form.text("reply_markup", keyboard(buttons).to_string());
				}
				("sendPhoto", answer("sendPhoto", self.request("sendPhoto").multipart(form)).await)
			}
		};
		sent?
			.get("message_id")
			.and_then(Value::as_i64)
			.ok_or_else(|| Failure::Refused(format!("{method}: no message_id")))
	}

	async fn edit(&self, chat_id: i64, message_id: i64, message: &Rendered, buttons: &[InlineButton]) -> Result<(), Failure> {
		// An empty keyboard, not none: that is what removes the buttons.
		let body = json!({
			"chat_id": chat_id,
			"message_id": message_id,
			"text": message.text,
			"entities": message.entities,
			"link_preview_options": {"is_disabled": true},
			"reply_markup": keyboard(buttons),
		});
		self.call("editMessageText", &body, None).await.map(drop)
	}

	async fn answer(&self, callback_id: &str, text: &str) -> Result<(), Failure> {
		self.call("answerCallbackQuery", &json!({"callback_query_id": callback_id, "text": text}), None).await.map(drop)
	}

	async fn chat(&self, chat: &str) -> Result<ChatInfo, Failure> {
		let got = self.call("getChat", &json!({"chat_id": chat}), None).await?;
		let field = |k: &str| got.get(k).and_then(Value::as_str).map(str::to_owned);
		Ok(ChatInfo {
			id: got.get("id").and_then(Value::as_i64).ok_or_else(|| Failure::Refused("getChat: no id".into()))?,
			kind: field("type").ok_or_else(|| Failure::Refused("getChat: no type".into()))?,
			title: field("title"),
			forum: got.get("is_forum").and_then(Value::as_bool) == Some(true),
		})
	}

	async fn is_admin(&self, chat_id: i64, user_id: i64) -> Result<bool, Failure> {
		let member = self.call("getChatMember", &json!({"chat_id": chat_id, "user_id": user_id}), None).await?;
		let status = member.get("status").and_then(Value::as_str).ok_or_else(|| Failure::Refused("getChatMember: no status".into()))?;
		Ok(matches!(status, "creator" | "administrator"))
	}
}

/// One update as the engine takes it; `None` for what the panel ignores (edits, joins, …).
pub fn parse_update(raw: &Value) -> Option<Update> {
	let chat_of = |m: &Value| -> Option<Chat> {
		let chat = m.get("chat")?;
		Some(Chat {
			id: chat.get("id")?.as_i64()?,
			private: chat.get("type").and_then(Value::as_str) == Some("private"),
		})
	};
	if let Some(q) = raw.get("callback_query") {
		let message = q.get("message")?;
		// Only the formatting is kept: Telegram adds a link of its own over a URL it sees, and a
		// customer's words must not become one when the message is edited.
		const FORMATTING: [&str; 9] = ["code", "pre", "bold", "italic", "underline", "strikethrough", "spoiler", "blockquote", "expandable_blockquote"];
		let entities = message
			.get("entities")
			.and_then(Value::as_array)
			.map(|all| {
				all.iter()
					.filter(|e| e.get("type").and_then(Value::as_str).is_some_and(|t| FORMATTING.contains(&t)))
					.cloned()
					.collect()
			})
			.unwrap_or_default();
		return Some(Update::Callback {
			id: q.get("id")?.as_str()?.to_owned(),
			chat_id: chat_of(message)?.id,
			message_id: message.get("message_id")?.as_i64()?,
			data: q.get("data").and_then(Value::as_str).unwrap_or_default().to_owned(),
			message: Rendered {
				text: message.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
				entities,
			},
		});
	}
	let message = raw.get("message")?;
	let chat = chat_of(message)?;
	// A forwarded `/start <token>` is someone else's link, passed on: never a link here.
	if message.get("forward_origin").is_some() {
		return Some(Update::Other { chat });
	}
	let from = message.get("from");
	let field = |k: &str| from.and_then(|f| f.get(k)).and_then(Value::as_str).map(str::to_owned);
	let account = Account {
		username: field("username"),
		first_name: field("first_name"),
	};
	let text = message.get("text").and_then(Value::as_str).unwrap_or_default().trim();
	let (command, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
	// `/start@evinvest_sa_bot` in a group names the bot; in a private chat it is the same.
	let command = command.split('@').next().unwrap_or_default();
	Some(match command {
		"/start" => Update::Start {
			chat,
			payload: Some(rest.trim()).filter(|p| !p.is_empty()).map(str::to_owned),
			from: account,
		},
		"/stop" => Update::Stop { chat },
		_ => Update::Other { chat },
	})
}

/// The bot's username, once known (from `TELEGRAM_BOT_USERNAME`, or `getMe` at start).
#[derive(Clone, Debug, Default)]
pub struct BotName(Arc<OnceLock<String>>);

impl BotName {
	/// Its name is `known`, or learned later by [`Self::learn`].
	pub fn new(known: Option<String>) -> Self {
		let cell = OnceLock::new();
		if let Some(name) = known {
			cell.set(name.trim_start_matches('@').to_owned()).expect("a fresh cell");
		}
		Self(Arc::new(cell))
	}

	pub fn learn(&self, name: String) {
		// A name already set (from the environment) wins over getMe's.
		let _kept = self.0.set(name);
	}

	pub fn get(&self) -> Option<&str> {
		self.0.get().map(String::as_str)
	}
}

/// Polls for updates and handles them until `shutdown` turns true, on the replica holding the
/// poller's lease; first learns the bot's name, which the link waits for.
pub async fn poll<F: Feeds>(hub: &Hub<BotApi, Concierge, F>, name: &BotName, mut shutdown: watch::Receiver<bool>) {
	let holder = Uuid::now_v7();
	let stopped = |s: &watch::Receiver<bool>| *s.borrow();
	while name.get().is_none() && !stopped(&shutdown) {
		match hub.bot.username().await {
			Ok(username) => {
				tracing::info!(username, "telegram: the bot is up");
				name.learn(username);
			}
			Err(f) => {
				tracing::warn!(failure = ?f, "telegram: getMe failed; linking waits for it");
				tokio::select! {
					() = tokio::time::sleep(STANDBY_EVERY) => {}
					_ = shutdown.changed() => {}
				}
			}
		}
	}
	while !stopped(&shutdown) {
		let now = Timestamp::now();
		let offset = match hub.panel.telegram_poll_lease(holder, now, now + POLL_LEASE).await {
			Ok(Some(offset)) => offset,
			Ok(None) => {
				tokio::select! {
					() = tokio::time::sleep(STANDBY_EVERY) => {}
					_ = shutdown.changed() => {}
				}
				continue;
			}
			Err(e) => {
				crate::report(&e, "leasing the telegram poller");
				tokio::time::sleep(STANDBY_EVERY).await;
				continue;
			}
		};
		let updates = tokio::select! {
			u = hub.bot.updates(offset, Duration::from_secs(POLL_SECONDS)) => u,
			_ = shutdown.changed() => break,
		};
		let updates = match updates {
			Ok(u) => u,
			Err(f) => {
				tracing::warn!(failure = ?f, "telegram: getUpdates failed");
				tokio::time::sleep(Duration::from_secs(3)).await;
				continue;
			}
		};
		for (id, raw) in updates {
			if let Some(update) = parse_update(&raw)
				&& let Err(e) = hub.handle(update, Timestamp::now()).await
			{
				// Past it anyway: an update that fails every time must not stop the bot; the
				// user presses again.
				crate::report(&e, "handling a telegram update");
			}
			match hub.panel.telegram_poll_advance(holder, id + 1).await {
				Ok(true) => {}
				Ok(false) => break,
				Err(e) => {
					crate::report(&e, "advancing the telegram poller");
					break;
				}
			}
		}
	}
	if let Err(e) = hub.panel.telegram_poll_release(holder).await {
		crate::report(&e, "releasing the telegram poller");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn message(chat_type: &str, text: &str) -> Value {
		json!({"update_id": 1, "message": {"message_id": 5, "chat": {"id": 42, "type": chat_type}, "text": text}})
	}

	#[test]
	fn updates_are_read() {
		let private = Chat { id: 42, private: true };
		assert_eq!(
			parse_update(&message("private", "/start abc_-9")),
			Some(Update::Start {
				chat: private,
				payload: Some("abc_-9".into()),
				from: Account::default()
			})
		);
		let from = Account { username: None, first_name: None };
		assert_eq!(
			parse_update(&message("private", "/start")),
			Some(Update::Start {
				chat: private,
				payload: None,
				from: from.clone()
			})
		);
		let mut forwarded = message("private", "/start tok");
		forwarded["message"]["forward_origin"] = json!({"type": "user"});
		assert_eq!(parse_update(&forwarded), Some(Update::Other { chat: private }), "a forwarded link is not one");
		let mut named = message("private", "/start tok");
		named["message"]["from"] = json!({"id": 5, "username": "olga", "first_name": "Olga"});
		assert!(matches!(parse_update(&named), Some(Update::Start { from: Account { username: Some(u), .. }, .. }) if u == "olga"));
		assert_eq!(parse_update(&message("private", "/stop")), Some(Update::Stop { chat: private }));
		assert_eq!(
			parse_update(&message("group", "/start@evinvest_sa_bot tok")),
			Some(Update::Start {
				chat: Chat { id: 42, private: false },
				payload: Some("tok".into()),
				from: Account::default()
			})
		);
		assert_eq!(parse_update(&message("private", "hello")), Some(Update::Other { chat: private }));
		let cb = json!({"update_id": 2, "callback_query": {"id": "cb1", "data": "t.1.aa", "message": {"message_id": 9, "chat": {"id": 42, "type": "private"}, "text": "New lead",
			"entities": [{"type": "code", "offset": 2, "length": 3}, {"type": "url", "offset": 0, "length": 1}]}}});
		assert_eq!(
			parse_update(&cb),
			Some(Update::Callback {
				id: "cb1".into(),
				chat_id: 42,
				message_id: 9,
				data: "t.1.aa".into(),
				message: Rendered {
					text: "New lead".into(),
					entities: vec![json!({"type": "code", "offset": 2, "length": 3})]
				}
			})
		);
		assert_eq!(parse_update(&json!({"update_id": 3, "edited_message": {}})), None);
	}
}

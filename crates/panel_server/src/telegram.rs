//! The conductor's Telegram bot: the Bot API over HTTPS, sites as the hub reads it, the
//! background work of `serve`, and the profile's `/api/v1/telegram` routes. What a message says
//! and to whom, and what a button does, is the engine's (`panel::telegram`).
//!
//! **Updates arrive by long polling, not a webhook.** A webhook would need a public route and a
//! secret header check on it — surface for anyone on the internet — while `getUpdates` needs
//! only egress to `api.telegram.org`. Telegram answers a second poller 409, so one replica
//! polls, under a lease in the database that another takes over when it lapses; the offset lives
//! there too, so a new holder resumes where the last one stopped. Every update is handled
//! idempotently (a link token redeems once, a button's press carries the same Idempotency-Key),
//! so an update handled twice across a takeover does nothing twice.
//!
//! Without `TELEGRAM_BOT_TOKEN` none of this runs: `serve` warns, and the profile answers
//! that Telegram is not configured.

use std::{
	sync::{Arc, OnceLock},
	time::Duration,
};

use axum::{
	Extension, Json, Router,
	body::Body,
	extract::{State, rejection::JsonRejection},
	http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
	response::{IntoResponse, Response},
	routing::{get, put},
};
use jiff::{SignedDuration, Timestamp};
use panel::{
	Panel,
	telegram::{Account, Bot, Call, Chat, Directory, Identity, InlineButton, NotePage, Notifier, Sites, Update},
};
use panel_core::notify::{Failure, Rendered, Rule};
use sa_auth::{Principal, Service, User};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::watch, task::JoinSet};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
	api::ApiError,
	concierge::Concierge,
	forward::{SITES_TIMEOUT, Services},
	signin::Caller,
};

/// The Bot API's root.
pub const API_BASE: &str = "https://api.telegram.org";

/// How long `getUpdates` holds a poll open when nothing arrives.
const POLL_SECONDS: u64 = 25;

/// The poller's lease: renewed every poll, so it lapses only when its holder is gone.
const POLL_LEASE: SignedDuration = SignedDuration::from_secs(60);

/// How often each part of the work runs.
const DELIVER_EVERY: Duration = Duration::from_millis(500);
const FAN_OUT_EVERY: Duration = Duration::from_secs(2);
const RECHECK_EVERY: Duration = Duration::from_secs(60);
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

	/// Calls a method; its `result`, or how it failed. `timeout` overrides the client's, for
	/// the long poll.
	async fn call(&self, method: &str, body: &Value, timeout: Option<Duration>) -> Result<Value, Failure> {
		let url = format!("{}/bot{}/{method}", self.base, self.token.as_str());
		let mut req = self.http.post(url).json(body);
		if let Some(t) = timeout {
			req = req.timeout(t);
		}
		// The URL carries the token: reqwest's error would print it.
		let res = req.send().await.map_err(|e| Failure::Transient(format!("{method}: {}", e.without_url())))?;
		let status = res.status();
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
		Ok(result
			.as_array()
			.map(|all| all.iter().filter_map(|u| Some((u.get("update_id")?.as_i64()?, u.clone()))).collect())
			.unwrap_or_default())
	}
}

fn keyboard(buttons: &[InlineButton]) -> Value {
	let row = |b: &InlineButton| match b {
		InlineButton::Callback { label, data } => json!([{"text": label, "callback_data": data}]),
		InlineButton::Url { label, url } => json!([{"text": label, "url": url}]),
	};
	json!({"inline_keyboard": buttons.iter().map(row).collect::<Vec<_>>()})
}

impl Bot for BotApi {
	async fn send(&self, chat_id: i64, message: &Rendered, buttons: &[InlineButton]) -> Result<i64, Failure> {
		let mut body = json!({"chat_id": chat_id, "text": message.text, "entities": message.entities, "link_preview_options": {"is_disabled": true}});
		if !buttons.is_empty() {
			body["reply_markup"] = keyboard(buttons);
		}
		let sent = self.call("sendMessage", &body, None).await?;
		sent.get("message_id")
			.and_then(Value::as_i64)
			.ok_or_else(|| Failure::Refused("sendMessage: no message_id".into()))
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

// ── /api/v1/telegram ────────────────────────────────────────────────────────────────────

/// The bot's username, once known (from `TELEGRAM_BOT_USERNAME`, or `getMe` at start);
/// `None` inside when the bot is not configured.
#[derive(Clone, Debug, Default)]
pub struct BotName(Option<Arc<OnceLock<String>>>);

impl BotName {
	/// The bot is not configured.
	pub fn off() -> Self {
		Self(None)
	}

	/// The bot is configured; its name is `known`, or learned later by [`Self::learn`].
	pub fn on(known: Option<String>) -> Self {
		let cell = OnceLock::new();
		if let Some(name) = known {
			let _set_once = cell.set(name.trim_start_matches('@').to_owned());
		}
		Self(Some(Arc::new(cell)))
	}

	pub fn learn(&self, name: String) {
		if let Some(cell) = &self.0 {
			// A name already set (from the environment) wins over getMe's.
			let _kept = cell.set(name);
		}
	}

	fn configured(&self) -> bool {
		self.0.is_some()
	}

	fn get(&self) -> Option<&str> {
		self.0.as_ref().and_then(|c| c.get()).map(String::as_str)
	}

	fn known(&self) -> bool {
		self.get().is_some()
	}
}

#[derive(Clone, Debug)]
pub struct TelegramState {
	pub panel: Panel,
	pub bot: BotName,
}

/// `GET /telegram`, `POST|DELETE /telegram/link`, `PUT /telegram/rules`: behind the gate, open
/// to every signed-in user, so a [`Caller`] is there and CSRF was checked on the writes.
pub fn routes() -> Router<TelegramState> {
	Router::new()
		.route("/telegram", get(settings))
		.route("/telegram/link", axum::routing::delete(unlink))
		.route("/telegram/rules", put(set_rules))
}

/// `POST /telegram/link`: asks concierge afresh — the permissions a link token carries are
/// concierge's answer of this moment, not the cache's.
pub fn link_routes() -> Router<TelegramState> {
	Router::new().route("/telegram/link", axum::routing::post(link))
}

fn display(caller: &Caller) -> String {
	if caller.preferred_name.trim().is_empty() {
		caller.email.clone()
	} else {
		caller.preferred_name.clone()
	}
}

async fn settings(State(s): State<TelegramState>, Extension(caller): Extension<Caller>) -> Result<Json<Value>, ApiError> {
	let settings = s.panel.telegram_settings(caller.user_id, &caller.permissions).await?;
	let rules: serde_json::Map<String, Value> = settings.rules.iter().map(|(r, on)| (r.as_str().to_owned(), json!(on))).collect();
	Ok(Json(json!({
		"enabled": s.bot.configured(),
		"linked": settings.linked,
		"blocked": settings.blocked,
		"account": settings.account,
		"rules": rules,
	})))
}

async fn link(State(s): State<TelegramState>, Extension(caller): Extension<Caller>) -> Result<Response, ApiError> {
	let Some(bot) = s.bot.get() else {
		let why = if s.bot.configured() {
			"the bot is starting, try again in a minute"
		} else {
			"Telegram is not configured"
		};
		return Ok((StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": why }))).into_response());
	};
	let token = s.panel.telegram_link_token(caller.user_id, &caller.permissions, &display(&caller), Timestamp::now()).await?;
	tracing::info!(user_id = %caller.user_id, "telegram: link token issued");
	Ok((StatusCode::CREATED, Json(json!({ "url": format!("https://t.me/{bot}?start={}", token.as_str()) }))).into_response())
}

async fn unlink(State(s): State<TelegramState>, Extension(caller): Extension<Caller>) -> Result<StatusCode, ApiError> {
	if !s.panel.telegram_unlink(caller.user_id).await? {
		return Err(ApiError::NotFound);
	}
	tracing::info!(user_id = %caller.user_id, "telegram: unlinked from the panel");
	Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesBody {
	/// `{"new_lead": true, "contact_overdue": false, …}`: the rules to change.
	rules: std::collections::BTreeMap<String, bool>,
}

async fn set_rules(State(s): State<TelegramState>, Extension(caller): Extension<Caller>, b: Result<Json<RulesBody>, JsonRejection>) -> Result<Json<Value>, ApiError> {
	let Json(b) = b.map_err(|e| ApiError::BadRequest(e.body_text()))?;
	let rules = b.rules.iter().map(|(r, on)| Ok((r.parse::<Rule>()?, *on))).collect::<Result<Vec<_>, panel_core::Invalid>>()?;
	s.panel.telegram_set_rules(caller.user_id, &caller.permissions, &rules).await??;
	settings(State(s), Extension(caller)).await
}

// ── the background work of `serve` ──────────────────────────────────────────────────────

/// Runs the bot until `shutdown` turns true: the poller (on the replica holding its lease),
/// the fan-out, the delivery and the access checks, each on its own schedule. A failing pass
/// is reported and tried again at the next tick; nothing here ends `serve`.
pub async fn run(notifier: Notifier<BotApi, Concierge, Services>, name: BotName, shutdown: watch::Receiver<bool>) {
	let notifier = Arc::new(notifier);
	let mut work = JoinSet::new();
	work.spawn(poll(notifier.clone(), name, shutdown.clone()));
	let n = notifier.clone();
	work.spawn(crate::every(FAN_OUT_EVERY, shutdown.clone(), "telegram fan-out", move || {
		let n = n.clone();
		async move { n.fan_out(Timestamp::now()).await.map(drop) }
	}));
	let n = notifier.clone();
	work.spawn(crate::every(DELIVER_EVERY, shutdown.clone(), "telegram delivery", move || {
		let n = n.clone();
		async move { n.deliver(Timestamp::now()).await.map(drop) }
	}));
	let n = notifier;
	work.spawn(crate::every(RECHECK_EVERY, shutdown, "telegram access check", move || {
		let n = n.clone();
		async move { n.recheck_access(Timestamp::now()).await.map(drop) }
	}));
	while let Some(done) = work.join_next().await {
		if let Err(e) = done {
			crate::report(&eyre::eyre!(e), "a telegram task panicked");
		}
	}
}

async fn poll(n: Arc<Notifier<BotApi, Concierge, Services>>, name: BotName, mut shutdown: watch::Receiver<bool>) {
	let holder = Uuid::now_v7();
	let stopped = |s: &watch::Receiver<bool>| *s.borrow();
	while !name.known() && !stopped(&shutdown) {
		match n.bot.username().await {
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
		let offset = match n.panel.telegram_poll_lease(holder, now, now + POLL_LEASE).await {
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
			u = n.bot.updates(offset, Duration::from_secs(POLL_SECONDS)) => u,
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
				&& let Err(e) = n.handle(update, Timestamp::now()).await
			{
				// Past it anyway: an update that fails every time must not stop the bot; the
				// user presses again.
				crate::report(&e, "handling a telegram update");
			}
			match n.panel.telegram_poll_advance(holder, id + 1).await {
				Ok(true) => {}
				Ok(false) => break,
				Err(e) => {
					crate::report(&e, "advancing the telegram poller");
					break;
				}
			}
		}
	}
	if let Err(e) = n.panel.telegram_poll_release(holder).await {
		crate::report(&e, "releasing the telegram poller");
	}
}

impl Directory for Concierge {
	async fn me(&self, access: &str) -> Result<Identity, panel::telegram::DirectoryError> {
		use panel::telegram::DirectoryError;

		use crate::concierge::ConciergeError;
		match Concierge::me(self, access).await {
			Ok(me) => Ok(Identity {
				user_id: me.user_id,
				email: me.email,
				email_verified: me.email_verified,
				preferred_name: me.preferred_name,
				permissions: me.permissions,
			}),
			Err(ConciergeError::Refused(_)) => Err(DirectoryError::Refused),
			Err(ConciergeError::Unavailable(why)) => Err(DirectoryError::Unavailable(why)),
			Err(ConciergeError::Failed(why)) => Err(DirectoryError::Failed(why)),
		}
	}
}

impl Sites for Services {
	async fn notes(&self, after: i64, limit: usize) -> eyre::Result<NotePage> {
		self.conductor_get(&format!("/api/conductor/notifications?after={after}&limit={limit}")).await
	}

	async fn received(&self) -> eyre::Result<std::collections::BTreeMap<String, Timestamp>> {
		self.conductor_get("/api/conductor/sources/received").await
	}

	async fn press(&self, who: &Identity, call: &Call, idempotency_key: &str) -> eyre::Result<u16> {
		let method: Method = call.method.parse().map_err(|_| eyre::eyre!("a button's method {}", call.method))?;
		let mut headers = HeaderMap::new();
		headers.insert(
			HeaderName::from_static("idempotency-key"),
			HeaderValue::from_str(idempotency_key).expect("tg/<outbox>/<button id> is visible ASCII"),
		);
		let body = match &call.body {
			Value::Null => Body::empty(),
			body => {
				headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
				Body::from(serde_json::to_vec(body).expect("a JSON value serializes"))
			}
		};
		let user = User {
			sub: who.user_id.to_string(),
			email: who.email.clone(),
			email_verified: who.email_verified,
			name: who.preferred_name.clone(),
			permissions: who.permissions.iter().filter(|p| p.starts_with(Service::Sites.prefix())).collect(),
		};
		let sent = self.send(Service::Sites, method, &call.path, headers, Some(Principal::User(user)), body);
		let res = tokio::time::timeout(SITES_TIMEOUT, sent)
			.await
			.map_err(|_| eyre::eyre!("sites did not answer a press in time"))??;
		Ok(res.status().as_u16())
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

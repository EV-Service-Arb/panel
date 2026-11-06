//! Alerts without the I/O: what the conductor's own messages say, what a button's data carries,
//! and how a failed Telegram delivery is retried. The rules are `sa_alerts`'; the services render
//! their own notes.

use std::str::FromStr;

use hmac::{Hmac, KeyInit, Mac};
use jiff::{SignedDuration, Timestamp};
use sa_alerts::{Rule, is_button_id};
use serde_json::{Value, json};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::Invalid;

/// How long a source may send nothing before the admins hear of it.
pub const SOURCE_SILENCE: SignedDuration = SignedDuration::from_hours(24);

/// Longest a name may run in a message, in characters.
pub const MAX_NAME: usize = 80;

/// At most one message a second to a private chat (Telegram's limit for one).
pub const PER_CHAT_GAP: SignedDuration = SignedDuration::from_secs(1);

/// At most twenty a minute to a channel or a group (Telegram's limit for one).
pub const PER_GROUP_GAP: SignedDuration = SignedDuration::from_secs(3);

/// At most this many messages in any second, across every chat and every replica (under the
/// Bot API's 30).
pub const GLOBAL_PER_SECOND: i64 = 25;

/// Tries before a message is given up on: with [`backoff`], about an hour — past that a lead
/// notice is news nobody needs.
pub const MAX_ATTEMPTS: i32 = 10;

/// Bytes of HMAC in a button's data: 80 bits, far past guessing within Telegram's rate of
/// callbacks, and short enough for its 64-byte limit.
const CALLBACK_MAC_LEN: usize = 10;

/// The language of the bot's messages (one per deployment, `TELEGRAM_LOCALE`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Locale {
	#[default]
	Ru,
	En,
}

impl FromStr for Locale {
	type Err = Invalid;

	fn from_str(s: &str) -> Result<Self, Invalid> {
		match s.trim() {
			"ru" => Ok(Self::Ru),
			"en" => Ok(Self::En),
			other => Err(Invalid::new(format!("TELEGRAM_LOCALE {other:?} is not one of ru, en"))),
		}
	}
}

/// A button's `callback_data`: `<button>.<outbox message id>.<mac>`. The message id is all it
/// names; the MAC, under a key derived from `PANEL_DATA_KEY` and bound to the chat, is what
/// makes it the panel's — Telegram hands the data back verbatim, and anyone who can talk to
/// the bot can send any data they like.
pub fn callback_data(key: &[u8; 32], chat_id: i64, message: i64, button: &str) -> String {
	assert!(is_button_id(button), "button ids are checked when a note is taken");
	format!("{button}.{message}.{}", hex::encode(callback_mac(key, chat_id, message, button)))
}

/// The message and button id a callback names, if its data is the panel's for this chat.
pub fn parse_callback(key: &[u8; 32], chat_id: i64, data: &str) -> Option<(i64, String)> {
	let mut parts = data.splitn(3, '.');
	let (button, message, mac) = (parts.next()?, parts.next()?, parts.next()?);
	// Messages sent before the buttons came from sites name theirs by one letter.
	let legacy = match button {
		"t" => Some("take"),
		"n" => Some("no_answer"),
		_ => None,
	};
	if legacy.is_none() && !is_button_id(button) {
		return None;
	}
	let message: i64 = message.parse().ok()?;
	let mac = hex::decode(mac).ok()?;
	let want = callback_mac(key, chat_id, message, button);
	if mac.len() != want.len() || !bool::from(mac.as_slice().ct_eq(&want)) {
		return None;
	}
	Some((message, legacy.unwrap_or(button).to_owned()))
}

/// A message the conductor writes itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Note {
	SourceSilent {
		key_id: String,
		kind: String,
		last: Option<Timestamp>,
	},
	/// Too many of one rule's messages queued for one destination to send each: how many, and
	/// nothing about them.
	Backlog {
		rule: Rule,
		count: u64,
	},
	/// A signed-in user asked for `need`, a permission or an alias of the `sa` catalog.
	AccessRequested {
		email: String,
		/// Their preferred name; empty when they have none.
		name: String,
		need: String,
	},
}
impl Note {
	pub fn rule(&self) -> Rule {
		match self {
			Self::Backlog { rule, .. } => *rule,
			Self::SourceSilent { .. } => Rule::SourceSilent,
			Self::AccessRequested { .. } => Rule::AccessRequested,
		}
	}

	/// A link under it, `(label, url)`: for an access request, the admin console's grant form
	/// for the `sa` scope at `cabinet` (concierge's origin), prefilled with the email and the
	/// need.
	pub fn link(&self, locale: Locale, cabinet: &url::Url) -> Option<(&'static str, String)> {
		let Self::AccessRequested { email, need, .. } = self else { return None };
		let mut url = cabinet.clone();
		url.set_path("/admin/scopes/sa");
		url.query_pairs_mut().clear().append_pair("email", email).append_pair("target", need);
		let label = match locale {
			Locale::Ru => "Выдать доступ",
			Locale::En => "Grant access",
		};
		Some((label, url.into()))
	}

	/// Plain text (sent without a parse mode, so nothing a user typed is markup).
	pub fn text(&self, locale: Locale) -> String {
		self.render(locale).text
	}

	/// The text and its `code` entities: what a user typed goes inside a `code` entity, on one
	/// line ([`one_line`]) and bounded, so it can neither fake a line of ours nor turn into a link.
	pub fn render(&self, locale: Locale) -> Rendered {
		let ru = locale == Locale::Ru;
		let mut out = Rendered::default();
		match self {
			Self::SourceSilent { key_id, kind, last } => {
				let since = match (last, ru) {
					(Some(at), true) => format!("последнее событие {}", minute(*at)),
					(Some(at), false) => format!("last event {}", minute(*at)),
					(None, true) => "событий не было".to_owned(),
					(None, false) => "no events yet".to_owned(),
				};
				out.push(if ru {
					"Источник молчит больше суток\n"
				} else {
					"Source silent for over a day\n"
				});
				out.code(key_id);
				out.push(&format!(" ({kind}): {since}"));
			}
			Self::AccessRequested { email, name, need } => {
				let playbook = need == sa_auth::Mcp::Use.as_str();
				out.push(match (playbook, ru) {
					(true, true) => "🔑 Запрос доступа к плейбуку\n",
					(true, false) => "🔑 Playbook access request\n",
					(false, true) => "🔑 Запрос доступа\n",
					(false, false) => "🔑 Access request\n",
				});
				out.code(&one_line(email, MAX_NAME));
				out.push(&match (playbook, ru) {
					(true, true) => " хочет подключить Claude Code к плейбуку.".to_owned(),
					(true, false) => " wants to connect Claude Code to the playbook.".to_owned(),
					(false, true) => format!(" просит {need}."),
					(false, false) => format!(" asks for {need}."),
				});
				let name = one_line(name, MAX_NAME);
				if !name.is_empty() {
					out.push(if ru { "\nИмя: " } else { "\nName: " });
					out.code(&name);
				}
			}
			Self::Backlog {
				rule: Rule::NewLead | Rule::ContactOverdue,
				count,
			} => out.push(&if ru {
				format!("{count} новых заявок ждут звонка. Откройте панель.")
			} else {
				format!("{count} new leads are waiting for a call. Open the panel.")
			}),
			Self::Backlog { rule, count } => out.push(&if ru {
				format!("Ещё {count} уведомлений «{}» не отправлены. Откройте панель.", rule_name(*rule, locale))
			} else {
				format!("{count} more \"{}\" alerts were not sent. Open the panel.", rule_name(*rule, locale))
			}),
		}
		out
	}
}

/// What a destination is sent when it is added or tested.
pub fn test_text(locale: Locale) -> &'static str {
	match locale {
		Locale::Ru => "Сюда будут приходить оповещения панели Service-Arb.",
		Locale::En => "Alerts of the Service-Arb panel will arrive here.",
	}
}

/// A message: its text and its Telegram `MessageEntity` objects (offsets in UTF-16 code units,
/// as the Bot API counts), sent with no parse mode.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rendered {
	pub text: String,
	pub entities: Vec<Value>,
}
impl Rendered {
	pub fn push(&mut self, s: &str) {
		self.text.push_str(s);
	}

	/// `s` as `code`; nothing when it is empty.
	pub fn code(&mut self, s: &str) {
		let length = s.encode_utf16().count();
		if length > 0 {
			self.entities.push(json!({"type": "code", "offset": self.text.encode_utf16().count(), "length": length}));
		}
		self.text.push_str(s);
	}
}

/// A user's text on one line: the characters `hidden` names made spaces, runs of spaces one, at
/// most `max` characters (the last one "…" when cut).
pub fn one_line(s: &str, max: usize) -> String {
	let spaced: String = s.chars().map(|c| if hidden(c) { ' ' } else { c }).collect();
	let joined = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
	match joined.char_indices().nth(max.saturating_sub(1)) {
		Some((i, _)) if joined.chars().count() > max => format!("{}…", &joined[..i]),
		_ => joined,
	}
}

/// What the bot says once a chat is linked: to which panel account, so a link opened from
/// someone else's hands is seen at once.
pub fn linked_text(locale: Locale, account: &str) -> String {
	let account = one_line(account, MAX_NAME);
	match locale {
		Locale::Ru => format!("Telegram подключён к панели Service-Arb, аккаунт: {account}. Оповещения настраиваются на странице «Оповещения» панели. Не вы? Отправьте /stop."),
		Locale::En => format!("Telegram is linked to the Service-Arb panel, account: {account}. Choose your alerts on the panel's Alerts page. Not you? Send /stop."),
	}
}

/// What the bot says back, by occasion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reply {
	LinkInvalid,
	/// The chat is linked to another panel account already.
	ChatTaken,
	Help,
	Unlinked,
	NotLinked,
	/// The button is not one the panel made for this chat.
	BadButton,
	/// The button's user has no live panel session to ask concierge with.
	OpenPanel,
	NoAccess,
	/// concierge could not be asked.
	TryLater,
	/// Recorded.
	Done,
	/// Pressed before; nothing more recorded.
	AlreadyDone,
	/// What the button acts on is gone.
	LeadGone,
}
impl Reply {
	pub fn text(self, locale: Locale) -> &'static str {
		let ru = locale == Locale::Ru;
		match self {
			Self::ChatTaken if ru => "Этот чат привязан к другому аккаунту панели. Сначала отправьте /stop.",
			Self::ChatTaken => "This chat is linked to another panel account. Send /stop first.",
			Self::LinkInvalid if ru => "Ссылка недействительна или устарела. Откройте «Оповещения» в панели и подключите Telegram заново.",
			Self::LinkInvalid => "This link is invalid or has expired. Open Alerts in the panel and link Telegram again.",
			Self::Help if ru => "Это бот панели Service-Arb. Подключение — кнопкой «Подключить Telegram» на странице «Оповещения» панели. /stop отключает этот чат.",
			Self::Help => "This is the Service-Arb panel's bot. Link it with \"Connect Telegram\" on the panel's Alerts page. /stop unlinks this chat.",
			Self::Unlinked if ru => "Этот чат отключён. Подключить снова можно на странице «Оповещения» панели.",
			Self::Unlinked => "This chat is unlinked. You can link it again on the panel's Alerts page.",
			Self::NotLinked if ru => "Этот чат не подключён к панели.",
			Self::NotLinked => "This chat is not linked to the panel.",
			Self::BadButton if ru => "Кнопка недействительна.",
			Self::BadButton => "This button is not valid.",
			Self::OpenPanel if ru => "Откройте панель, чтобы подтвердить доступ, и нажмите снова.",
			Self::OpenPanel => "Open the panel to confirm your access, then press again.",
			Self::NoAccess if ru => "Нет доступа к панели.",
			Self::NoAccess => "No access to the panel.",
			Self::TryLater if ru => "Не удалось проверить доступ. Попробуйте через минуту.",
			Self::TryLater => "Could not check your access. Try again in a minute.",
			Self::Done if ru => "Записано.",
			Self::Done => "Recorded.",
			Self::AlreadyDone if ru => "Уже записано.",
			Self::AlreadyDone => "Already recorded.",
			Self::LeadGone if ru => "Заявка не найдена.",
			Self::LeadGone => "Lead not found.",
		}
	}
}

/// How a try to send ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
	/// 429: Telegram says when to try again.
	RetryAfter(SignedDuration),
	/// 5xx, a timeout, no connection: tried again later.
	Transient(String),
	/// 403: the user blocked the bot, or it is no longer in the channel or group. The chat is dead
	/// until a send there succeeds again.
	Blocked,
	/// Any other refusal (400: chat not found, text too long): trying again changes nothing.
	Refused(String),
}

/// What becomes of a message after a failed try.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Next {
	Retry {
		at: Timestamp,
	},
	/// Given up on: the tries ran out, or no try can succeed.
	Dead,
	/// Given up on, and the chat with it.
	ChatDead,
}

/// The wait after the `n`th failed try (1-based): 10 s, doubling, at most 15 minutes.
pub fn backoff(n: i32) -> SignedDuration {
	let doublings = u32::try_from(n.saturating_sub(1)).unwrap_or(0).min(16);
	SignedDuration::from_secs(10)
		.checked_mul(1 << doublings)
		.unwrap_or(SignedDuration::MAX)
		.min(SignedDuration::from_mins(15))
}

/// The fate of a message whose `attempts`th try (this one included) failed with `failure`.
pub fn after_failure(attempts: i32, failure: &Failure, now: Timestamp) -> Next {
	match failure {
		Failure::Blocked => Next::ChatDead,
		Failure::Refused(_) => Next::Dead,
		Failure::RetryAfter(_) | Failure::Transient(_) if attempts >= MAX_ATTEMPTS => Next::Dead,
		Failure::RetryAfter(wait) => Next::Retry {
			at: now.saturating_add((*wait).max(PER_CHAT_GAP)).unwrap_or(Timestamp::MAX),
		},
		Failure::Transient(_) => Next::Retry {
			at: now.saturating_add(backoff(attempts)).unwrap_or(Timestamp::MAX),
		},
	}
}

fn callback_mac(key: &[u8; 32], chat_id: i64, message: i64, button: &str) -> [u8; CALLBACK_MAC_LEN] {
	// HMAC takes a key of any length, so this cannot fail.
	let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
	mac.update(b"sa-panel/tg-callback/v1/");
	mac.update(&chat_id.to_be_bytes());
	mac.update(&message.to_be_bytes());
	mac.update(button.as_bytes());
	let full = mac.finalize().into_bytes();
	let mut out = [0u8; CALLBACK_MAC_LEN];
	out.copy_from_slice(&full[..CALLBACK_MAC_LEN]);
	out
}

fn rule_name(rule: Rule, locale: Locale) -> &'static str {
	let ru = locale == Locale::Ru;
	match rule {
		Rule::NewLead if ru => "новая заявка",
		Rule::NewLead => "new lead",
		Rule::ContactOverdue if ru => "заявка ждёт звонка",
		Rule::ContactOverdue => "contact overdue",
		Rule::PaymentReceived if ru => "оплата получена",
		Rule::PaymentReceived => "payment received",
		Rule::SourceSilent if ru => "источник молчит",
		Rule::SourceSilent => "source silent",
		Rule::Booked if ru => "запись",
		Rule::Booked => "booking",
		Rule::AccessRequested if ru => "запрос доступа",
		Rule::AccessRequested => "access request",
		Rule::ReviewNew if ru => "новый отзыв",
		Rule::ReviewNew => "new review",
		Rule::ReviewChanged if ru => "отзыв изменён",
		Rule::ReviewChanged => "review changed",
		Rule::ReviewGone if ru => "отзыв пропал",
		Rule::ReviewGone => "review gone",
		Rule::ReviewReappeared if ru => "отзыв вернулся",
		Rule::ReviewReappeared => "review reappeared",
		Rule::ScanFailed if ru => "проверка не удалась",
		Rule::ScanFailed => "scan failed",
	}
}

/// Whether a character is one a user's text may not carry into a message as it is: any control
/// character (a line break could start a line that reads as ours), the line and paragraph
/// separators, and the marks that reorder text (bidi overrides and isolates).
fn hidden(c: char) -> bool {
	c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// UTC, to the minute.
fn minute(at: Timestamp) -> String {
	at.strftime("%Y-%m-%d %H:%M UTC").to_string()
}

// ── delivery ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	const KEY: [u8; 32] = [7; 32];

	#[test]
	fn access_request_links_to_the_sa_scope_grant_form() {
		let origin: url::Url = "https://evinvest.ltd/".parse().unwrap();
		let note = Note::AccessRequested {
			email: "a+b@x.io".into(),
			name: String::new(),
			need: "sa:operator".into(),
		};
		let (label, url) = note.link(Locale::En, &origin).unwrap();
		assert_eq!(label, "Grant access");
		assert_eq!(url, "https://evinvest.ltd/admin/scopes/sa?email=a%2Bb%40x.io&target=sa%3Aoperator");
		assert_eq!(Note::Backlog { rule: Rule::NewLead, count: 1 }.link(Locale::En, &origin), None);
	}

	#[test]
	fn callbacks_are_bound_to_chat_message_and_button() {
		let data = callback_data(&KEY, 42, 1001, "take");
		assert_eq!(parse_callback(&KEY, 42, &data), Some((1001, "take".to_owned())));
		assert_eq!(parse_callback(&KEY, 43, &data), None, "another chat");
		assert_eq!(parse_callback(&[8; 32], 42, &data), None, "another key");
		assert_eq!(parse_callback(&KEY, 42, &data.replacen("take.1001", "take.1002", 1)), None, "another message");
		assert_eq!(parse_callback(&KEY, 42, &data.replacen("take.", "no_answer.", 1)), None, "another button");
		assert_eq!(parse_callback(&KEY, 42, "take.1001"), None);
		assert_eq!(parse_callback(&KEY, 42, "take.1001.zz"), None);
		assert_eq!(parse_callback(&KEY, 42, ""), None);
		let big = callback_data(&KEY, i64::MIN, i64::MAX, "abcdefghijklmnop");
		assert!(big.len() <= 64, "{big}");
		let one_letter = format!("t.1001.{}", hex::encode(callback_mac(&KEY, 42, 1001, "t")));
		assert_eq!(parse_callback(&KEY, 42, &one_letter), Some((1001, "take".to_owned())), "a message sent with one-letter buttons");
		assert!(!is_button_id("t") && !is_button_id("Take") && is_button_id("no_answer"));
	}

	#[test]
	fn texts() {
		let silent = Note::SourceSilent {
			key_id: "aquafix-site".into(),
			kind: "site".into(),
			last: Some("2026-09-29T08:15:30Z".parse().unwrap()),
		};
		assert_eq!(
			silent.text(Locale::Ru),
			"Источник молчит больше суток\naquafix-site (site): последнее событие 2026-09-29 08:15 UTC"
		);
		let asked = Note::AccessRequested {
			email: "eve@x.io\nВзял: Mallory".into(),
			name: "Eve".into(),
			need: "sa:operator".into(),
		}
		.render(Locale::En);
		assert_eq!(asked.text, "🔑 Access request\neve@x.io Взял: Mallory asks for sa:operator.\nName: Eve");
		let units: Vec<u16> = asked.text.encode_utf16().collect();
		let spans: Vec<String> = asked
			.entities
			.iter()
			.map(|e| {
				let (o, l) = (e["offset"].as_u64().unwrap() as usize, e["length"].as_u64().unwrap() as usize);
				String::from_utf16(&units[o..o + l]).unwrap()
			})
			.collect();
		assert_eq!(spans, ["eve@x.io Взял: Mallory", "Eve"], "what a user typed is code, on one line");
		assert_eq!(one_line("  a \u{2066}b\u{2069}  c ", 80), "a b c");
		assert_eq!(one_line("abcdef", 4), "abc…");
	}

	#[test]
	fn failures() {
		let now: Timestamp = "2026-09-30T10:00:00Z".parse().unwrap();
		let s = SignedDuration::from_secs;
		assert_eq!(after_failure(1, &Failure::RetryAfter(s(7)), now), Next::Retry { at: now + s(7) });
		assert_eq!(
			after_failure(1, &Failure::RetryAfter(s(0)), now),
			Next::Retry { at: now + s(1) },
			"never sooner than the chat's gap"
		);
		assert_eq!(after_failure(1, &Failure::Transient("502".into()), now), Next::Retry { at: now + s(10) });
		assert_eq!(after_failure(3, &Failure::Transient("502".into()), now), Next::Retry { at: now + s(40) });
		assert_eq!(after_failure(MAX_ATTEMPTS, &Failure::Transient("502".into()), now), Next::Dead);
		assert_eq!(after_failure(MAX_ATTEMPTS, &Failure::RetryAfter(s(1)), now), Next::Dead);
		assert_eq!(after_failure(1, &Failure::Blocked, now), Next::ChatDead);
		assert_eq!(after_failure(1, &Failure::Refused("chat not found".into()), now), Next::Dead);
		assert_eq!(backoff(100), SignedDuration::from_mins(15));
		let total: i64 = (1..MAX_ATTEMPTS).map(|n| backoff(n).as_secs()).sum();
		assert!((1800..=2 * 3600).contains(&total), "{total}");
	}
}

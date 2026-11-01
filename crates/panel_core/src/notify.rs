//! Telegram notifications without the I/O: which rules there are and who may get each, what the
//! conductor's own messages say, what a button's data carries, and how a failed delivery is
//! retried. The lead rules' messages are rendered by sites.

use std::{fmt, str::FromStr};

use hmac::{Hmac, KeyInit, Mac};
use jiff::{SignedDuration, Timestamp};
use sa_auth::{Leads, PermissionSet, SA_ADMIN, Sources};
use serde_json::{Value, json};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::Invalid;

/// What a user may be notified of. Each is on or off per user; [`Rule::on_by_default`] is
/// what a user who never chose gets. The lead rules come from sites' notes; `source_silent` and
/// `access_requested` are the conductor's own.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Rule {
	/// A lead came in: brand, location, what is needed and the phone, with buttons.
	NewLead,
	/// A lead has waited past its contact SLA: once.
	ContactOverdue,
	/// A payment was recorded.
	PaymentReceived,
	/// A source sent nothing for [`SOURCE_SILENCE`]: at most once a day.
	SourceSilent,
	/// A slot was booked, moved or canceled — by the customer through a provider, or by an
	/// operator.
	Booked,
	/// A signed-in user asked for a permission: at most once a day per user and need.
	AccessRequested,
}

impl Rule {
	pub const ALL: [Self; 6] = [
		Self::NewLead,
		Self::ContactOverdue,
		Self::PaymentReceived,
		Self::SourceSilent,
		Self::Booked,
		Self::AccessRequested,
	];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::NewLead => "new_lead",
			Self::ContactOverdue => "contact_overdue",
			Self::PaymentReceived => "payment_received",
			Self::SourceSilent => "source_silent",
			Self::Booked => "booked",
			Self::AccessRequested => "access_requested",
		}
	}

	/// On for a user who never chose.
	pub fn on_by_default(self) -> bool {
		match self {
			Self::NewLead | Self::ContactOverdue | Self::Booked | Self::AccessRequested => true,
			Self::PaymentReceived | Self::SourceSilent => false,
		}
	}

	/// Whether someone holding `permissions` gets this rule's messages at all: leads are
	/// whoever works them; money and the sources are whoever manages the sources; access
	/// requests are whoever holds all of `sa:admin`.
	pub fn open_to(self, permissions: &PermissionSet) -> bool {
		match self {
			Self::NewLead | Self::ContactOverdue | Self::Booked => permissions.may(Leads::Edit),
			Self::PaymentReceived | Self::SourceSilent => permissions.may(Sources::Manage),
			Self::AccessRequested => SA_ADMIN.members.iter().all(|m| permissions.iter().any(|p| p == *m)),
		}
	}
}

impl fmt::Display for Rule {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl FromStr for Rule {
	type Err = Invalid;

	fn from_str(s: &str) -> Result<Self, Invalid> {
		Self::ALL.into_iter().find(|r| r.as_str() == s).ok_or_else(|| {
			Invalid::new(format!(
				"{s:?} is not one of new_lead, contact_overdue, payment_received, source_silent, booked, access_requested"
			))
		})
	}
}

/// How long a source may send nothing before the admins hear of it.
pub const SOURCE_SILENCE: SignedDuration = SignedDuration::from_hours(24);

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

/// Bytes of HMAC in a button's data: 80 bits, far past guessing within Telegram's rate of
/// callbacks, and short enough for its 64-byte limit.
const CALLBACK_MAC_LEN: usize = 10;

/// What a button is called in its data: 2–16 of `[a-z0-9_]`, so the data stays within Telegram's
/// 64 bytes and never reads as a one-letter code of [`parse_callback`].
pub fn is_button_id(s: &str) -> bool {
	(2..=16).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
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
	/// Too many leads queued for one chat to send each: how many, and nothing about them.
	Backlog {
		count: i64,
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
			Self::Backlog { .. } => Rule::NewLead,
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
			Self::Backlog { count } => out.push(&if ru {
				format!("{count} новых заявок ждут звонка. Откройте панель.")
			} else {
				format!("{count} new leads are waiting for a call. Open the panel.")
			}),
		}
		out
	}
}

/// Longest a name may run in a message, in characters.
pub const MAX_NAME: usize = 80;

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

/// Whether a character is one a user's text may not carry into a message as it is: any control
/// character (a line break could start a line that reads as ours), the line and paragraph
/// separators, and the marks that reorder text (bidi overrides and isolates).
fn hidden(c: char) -> bool {
	c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
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

/// UTC, to the minute.
fn minute(at: Timestamp) -> String {
	at.strftime("%Y-%m-%d %H:%M UTC").to_string()
}

/// What the bot says once a chat is linked: to which panel account, so a link opened from
/// someone else's hands is seen at once.
pub fn linked_text(locale: Locale, account: &str) -> String {
	let account = one_line(account, MAX_NAME);
	match locale {
		Locale::Ru => format!("Telegram подключён к панели Service-Arb, аккаунт: {account}. Уведомления настраиваются в профиле панели. Не вы? Отправьте /stop."),
		Locale::En => format!("Telegram is linked to the Service-Arb panel, account: {account}. Choose notifications in your panel profile. Not you? Send /stop."),
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
			Self::LinkInvalid if ru => "Ссылка недействительна или устарела. Откройте профиль в панели и подключите Telegram заново.",
			Self::LinkInvalid => "This link is invalid or has expired. Open your panel profile and link Telegram again.",
			Self::Help if ru => "Это бот панели Service-Arb. Подключение — кнопкой «Подключить Telegram» в профиле панели. /stop отключает уведомления.",
			Self::Help => "This is the Service-Arb panel's bot. Link it with \"Connect Telegram\" in your panel profile. /stop turns notifications off.",
			Self::Unlinked if ru => "Уведомления отключены. Подключить снова можно в профиле панели.",
			Self::Unlinked => "Notifications are off. You can link again from your panel profile.",
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

// ── delivery ────────────────────────────────────────────────────────────────────────────

/// At most one message a second to a chat (Telegram's limit for a private chat).
pub const PER_CHAT_GAP: SignedDuration = SignedDuration::from_secs(1);

/// At most this many messages in any second, across every chat and every replica (under the
/// Bot API's 30).
pub const GLOBAL_PER_SECOND: i64 = 25;

/// Tries before a message is given up on: with [`backoff`], about an hour — past that a lead
/// notice is news nobody needs.
pub const MAX_ATTEMPTS: i32 = 10;

/// How a try to send ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
	/// 429: Telegram says when to try again.
	RetryAfter(SignedDuration),
	/// 5xx, a timeout, no connection: tried again later.
	Transient(String),
	/// 403: the user blocked the bot. The chat is dead until they link again.
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

#[cfg(test)]
mod tests {
	use super::*;

	const KEY: [u8; 32] = [7; 32];

	#[test]
	fn rules_round_trip_and_are_scoped() {
		for r in Rule::ALL {
			assert_eq!(r.as_str().parse::<Rule>().unwrap(), r);
		}
		assert!("review_low".parse::<Rule>().is_err(), "not yet");
		assert_eq!(Rule::ALL.iter().filter(|r| r.on_by_default()).count(), 4);
		let operator: PermissionSet = sa_auth::SA_OPERATOR.members.iter().copied().collect();
		let admin: PermissionSet = sa_auth::SA_ADMIN.members.iter().copied().collect();
		assert!(Rule::Booked.on_by_default() && Rule::Booked.open_to(&operator));
		assert!(Rule::NewLead.open_to(&operator) && Rule::ContactOverdue.open_to(&operator));
		assert!(!Rule::PaymentReceived.open_to(&operator) && !Rule::SourceSilent.open_to(&operator) && !Rule::AccessRequested.open_to(&operator));
		assert!(Rule::ALL.iter().all(|r| r.open_to(&admin)));
		assert!(Rule::ALL.iter().all(|r| !r.open_to(&std::iter::empty::<&str>().collect())), "no permission, no rule");
	}

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
		assert_eq!(Note::Backlog { count: 1 }.link(Locale::En, &origin), None);
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

//! Telegram notifications (spec §8) without the I/O: which rules there are and who may get
//! each, what a message says, what its buttons carry, and how a failed delivery is retried.
//!
//! The database and the Bot API are the engine's and the server's; this decides.

use std::{fmt, str::FromStr};

use hmac::{Hmac, KeyInit, Mac};
use jiff::{SignedDuration, Timestamp};
use sa_auth::{Leads, PermissionSet, SA_ADMIN, Sources};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::{Invalid, fact::LeadSuspect};

/// What a user may be notified of. Each is on or off per user; [`Rule::on_by_default`] is
/// what a user who never chose gets.
///
/// Not yet, for want of a source: a review of 3★ or less (review_archive's events), a stage
/// of the funnel dropping more than X % below its 4-week mean (the GBP and PostHog imports),
/// a Grafana alert. Each will be a variant here, a fan-out in the engine, and a text below;
/// the outbox, the pacing and the linking do not change.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Rule {
	/// A lead came in: brand, location, what is needed and the phone, with buttons.
	NewLead,
	/// A lead has waited past [`crate::funnel::CONTACT_SLA`] for its first contact: once.
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

/// A button under a lead's message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Button {
	/// "Взял": the lead is contacted (`lead.contacted`).
	Take,
	/// "Не дозвонился": a call attempted and not answered (`call.attempted` + `call.logged`).
	NoAnswer,
}

impl Button {
	fn code(self) -> &'static str {
		match self {
			Self::Take => "t",
			Self::NoAnswer => "n",
		}
	}

	fn from_code(c: &str) -> Option<Self> {
		match c {
			"t" => Some(Self::Take),
			"n" => Some(Self::NoAnswer),
			_ => None,
		}
	}

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Take => "take",
			Self::NoAnswer => "no_answer",
		}
	}

	/// The button [`Self::as_str`] names.
	pub fn from_name(name: &str) -> Option<Self> {
		[Self::Take, Self::NoAnswer].into_iter().find(|b| b.as_str() == name)
	}

	pub fn label(self, locale: Locale) -> &'static str {
		match (self, locale) {
			(Self::Take, Locale::Ru) => "Взял",
			(Self::Take, Locale::En) => "Taken",
			(Self::NoAnswer, Locale::Ru) => "Не дозвонился",
			(Self::NoAnswer, Locale::En) => "No answer",
		}
	}
}

/// Bytes of HMAC in a button's data: 80 bits, far past guessing within Telegram's rate of
/// callbacks, and short enough for its 64-byte limit.
const CALLBACK_MAC_LEN: usize = 10;

fn callback_mac(key: &[u8; 32], chat_id: i64, message: i64, button: Button) -> [u8; CALLBACK_MAC_LEN] {
	// HMAC takes a key of any length, so this cannot fail.
	let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
	mac.update(b"sa-panel/tg-callback/v1/");
	mac.update(&chat_id.to_be_bytes());
	mac.update(&message.to_be_bytes());
	mac.update(button.code().as_bytes());
	let full = mac.finalize().into_bytes();
	let mut out = [0u8; CALLBACK_MAC_LEN];
	out.copy_from_slice(&full[..CALLBACK_MAC_LEN]);
	out
}

/// A button's `callback_data`: `<button>.<outbox message id>.<mac>`. The message id is all it
/// names; the MAC, under a key derived from `PANEL_DATA_KEY` and bound to the chat, is what
/// makes it the panel's — Telegram hands the data back verbatim, and anyone who can talk to
/// the bot can send any data they like.
pub fn callback_data(key: &[u8; 32], chat_id: i64, message: i64, button: Button) -> String {
	format!("{}.{message}.{}", button.code(), hex::encode(callback_mac(key, chat_id, message, button)))
}

/// The message and button a callback names, if its data is the panel's for this chat.
pub fn parse_callback(key: &[u8; 32], chat_id: i64, data: &str) -> Option<(i64, Button)> {
	let mut parts = data.splitn(3, '.');
	let (button, message, mac) = (parts.next()?, parts.next()?, parts.next()?);
	let button = Button::from_code(button)?;
	let message: i64 = message.parse().ok()?;
	let mac = hex::decode(mac).ok()?;
	let want = callback_mac(key, chat_id, message, button);
	(mac.len() == want.len() && bool::from(mac.as_slice().ct_eq(&want))).then_some((message, button))
}

/// A lead as a message shows it. The PII fields are set only for a role that sees PII.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LeadNote {
	pub brand: String,
	pub location: Option<String>,
	pub name: Option<String>,
	pub need: Option<String>,
	pub phone: Option<String>,
}

/// What a message is about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Note {
	NewLead(LeadNote),
	/// A new lead the landing's antispam doubted: told under [`Rule::NewLead`] too, since it
	/// may be a person, but headed so nobody takes it for an ordinary one.
	SuspectLead {
		lead: LeadNote,
		suspect: LeadSuspect,
	},
	ContactOverdue {
		lead: LeadNote,
		waiting: SignedDuration,
	},
	PaymentReceived {
		brand: String,
		lead: String,
		billed: i64,
		commission: i64,
		currency: String,
	},
	SourceSilent {
		key_id: String,
		kind: String,
		last: Option<Timestamp>,
	},
	/// Too many leads queued for one chat to send each: how many, and nothing about them.
	Backlog {
		count: i64,
	},
	/// A slot booked, moved or canceled. `lead` is `None` for a provider's booking no lead was
	/// found for ("without a lead"); `slot` is already in the place's time.
	Booking {
		change: BookingChange,
		brand: String,
		lead: Option<LeadNote>,
		slot: Option<String>,
		provider: String,
	},
	/// A signed-in user asked for `need`, a permission or an alias of the `sa` catalog.
	AccessRequested {
		email: String,
		/// Their preferred name; empty when they have none.
		name: String,
		need: String,
	},
}

/// What became of a slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BookingChange {
	Booked,
	Moved,
	Canceled,
}

impl Note {
	pub fn rule(&self) -> Rule {
		match self {
			Self::NewLead(_) | Self::SuspectLead { .. } | Self::Backlog { .. } => Rule::NewLead,
			Self::ContactOverdue { .. } => Rule::ContactOverdue,
			Self::PaymentReceived { .. } => Rule::PaymentReceived,
			Self::SourceSilent { .. } => Rule::SourceSilent,
			Self::Booking { .. } => Rule::Booked,
			Self::AccessRequested { .. } => Rule::AccessRequested,
		}
	}

	/// The buttons under it: the lead's two actions for the lead rules, none otherwise.
	pub fn buttons(&self) -> &'static [Button] {
		match self {
			Self::NewLead(_) | Self::SuspectLead { .. } | Self::ContactOverdue { .. } => &[Button::Take, Button::NoAnswer],
			Self::PaymentReceived { .. } | Self::SourceSilent { .. } | Self::Backlog { .. } | Self::Booking { .. } | Self::AccessRequested { .. } => &[],
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

	/// Plain text (sent without a parse mode, so nothing a customer typed is markup).
	pub fn text(&self, locale: Locale) -> String {
		self.render(locale).text
	}

	/// The text and its `code` entities: what a customer typed goes on one line of its own
	/// field, cleaned ([`one_line`]) and bounded, inside a `code` entity, so it can neither
	/// fake a line of ours ("Взял: …") nor turn into a link.
	pub fn render(&self, locale: Locale) -> Rendered {
		let ru = locale == Locale::Ru;
		let mut out = Rendered::default();
		match self {
			Self::NewLead(lead) => lead_text(&mut out, if ru { "Новая заявка" } else { "New lead" }, lead, locale),
			Self::SuspectLead { lead, suspect } => {
				let head = match (suspect, ru) {
					(LeadSuspect::RateLimited, true) => "Подозрительная заявка (антиспам: слишком много отправок)",
					(LeadSuspect::RateLimited, false) => "Suspect lead (antispam: too many submissions)",
					(LeadSuspect::TooFast, true) => "Подозрительная заявка (антиспам: форма заполнена слишком быстро)",
					(LeadSuspect::TooFast, false) => "Suspect lead (antispam: form filled too fast)",
				};
				lead_text(&mut out, head, lead, locale);
			}
			Self::ContactOverdue { lead, waiting } => {
				let mins = waiting.as_mins().max(0);
				let head = if ru {
					format!("Заявка ждёт звонка {mins} мин")
				} else {
					format!("Lead waiting for a call for {mins} min")
				};
				lead_text(&mut out, &head, lead, locale);
			}
			Self::PaymentReceived {
				brand,
				lead,
				billed,
				commission,
				currency,
			} => {
				let (billed, commission) = (money(*billed), money(*commission));
				out.push(if ru { "Оплата получена\n" } else { "Payment received\n" });
				out.push(&format!("{brand} · "));
				out.code(lead);
				if ru {
					out.push(&format!("\nСумма: {billed} {currency}\nКомиссия: {commission} {currency}"));
				} else {
					out.push(&format!("\nBilled: {billed} {currency}\nCommission: {commission} {currency}"));
				}
			}
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
			Self::Booking {
				change,
				brand,
				lead,
				slot,
				provider,
			} => {
				let head = match (change, ru) {
					(BookingChange::Booked, true) => "Бронь",
					(BookingChange::Booked, false) => "Slot booked",
					(BookingChange::Moved, true) => "Бронь перенесена",
					(BookingChange::Moved, false) => "Booking moved",
					(BookingChange::Canceled, true) => "Бронь отменена",
					(BookingChange::Canceled, false) => "Booking canceled",
				};
				let head = match (slot, lead.is_none(), ru) {
					(Some(slot), false, _) => format!("{head}: {slot}"),
					(Some(slot), true, true) => format!("{head} без заявки: {slot}"),
					(Some(slot), true, false) => format!("{head}, no lead: {slot}"),
					(None, false, _) => head.to_owned(),
					(None, true, true) => format!("{head} без заявки"),
					(None, true, false) => format!("{head}, no lead"),
				};
				match lead {
					Some(lead) => lead_text(&mut out, &head, lead, locale),
					None => out.push(&format!("{head}\n{brand}")),
				}
				out.push(&format!("\n{} {provider}", if ru { "Через:" } else { "Via:" }));
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
		out.clamp(MAX_MESSAGE);
		out
	}
}

/// A message's length, in characters, whatever went into it: well under Telegram's 4096.
pub const MAX_MESSAGE: usize = 3500;

/// Longest a customer's field may run in a message, in characters.
pub const MAX_NAME: usize = 80;
pub const MAX_PHONE: usize = 40;
pub const MAX_NEED: usize = 500;

/// A span of a message shown as `code`, in UTF-16 code units (what the Bot API counts).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entity {
	pub offset: usize,
	pub length: usize,
}

/// A message: its text and the spans of it that are `code`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rendered {
	pub text: String,
	pub code: Vec<Entity>,
}

impl Rendered {
	fn units(&self) -> usize {
		self.text.encode_utf16().count()
	}

	pub fn push(&mut self, s: &str) {
		self.text.push_str(s);
	}

	/// `s` as `code`; nothing when it is empty.
	pub fn code(&mut self, s: &str) {
		let length = s.encode_utf16().count();
		if length > 0 {
			self.code.push(Entity { offset: self.units(), length });
		}
		self.text.push_str(s);
	}

	/// At most `max` characters, the entities cut to what is left.
	fn clamp(&mut self, max: usize) {
		let Some((cut, _)) = self.text.char_indices().nth(max) else { return };
		self.text.truncate(cut);
		let units = self.units();
		self.code.retain_mut(|e| {
			e.length = e.length.min(units.saturating_sub(e.offset));
			e.offset < units && e.length > 0
		});
	}
}

/// Whether a character is one a customer's text may not carry into a message as it is: any
/// control character (a line break could start a line that reads as ours), the line and
/// paragraph separators, and the marks that reorder text (bidi overrides and isolates).
fn hidden(c: char) -> bool {
	c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// A customer's text on one line: the characters `hidden` names made spaces, runs of spaces one,
/// at most `max` characters (the last one "…" when cut).
pub fn one_line(s: &str, max: usize) -> String {
	let spaced: String = s.chars().map(|c| if hidden(c) { ' ' } else { c }).collect();
	let joined = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
	match joined.char_indices().nth(max.saturating_sub(1)) {
		Some((i, _)) if joined.chars().count() > max => format!("{}…", &joined[..i]),
		_ => joined,
	}
}

/// A phone number as a message shows it: digits, `+`, `(`, `)`, `-` and spaces only.
pub fn phone(s: &str) -> String {
	let kept: String = s
		.chars()
		.map(|c| if hidden(c) { ' ' } else { c })
		.filter(|c| c.is_ascii_digit() || matches!(c, '+' | '(' | ')' | '-' | ' '))
		.collect();
	one_line(&kept, MAX_PHONE)
}

fn lead_text(out: &mut Rendered, head: &str, lead: &LeadNote, locale: Locale) {
	let ru = locale == Locale::Ru;
	out.push(head);
	out.push("\n");
	out.push(&lead.brand);
	if let Some(location) = &lead.location {
		out.push(&format!(" · {location}"));
	}
	let clean = |v: &Option<String>, max| v.as_deref().map(|v| one_line(v, max)).filter(|v| !v.is_empty());
	if let Some(name) = clean(&lead.name, MAX_NAME) {
		out.push(if ru { "\nИмя: " } else { "\nName: " });
		out.code(&name);
	}
	if let Some(need) = clean(&lead.need, MAX_NEED) {
		out.push(if ru { "\nНужно: " } else { "\nNeeds: " });
		out.code(&need);
	}
	if let Some(p) = lead.phone.as_deref().map(phone).filter(|p| !p.is_empty()) {
		out.push(if ru { "\nТелефон: " } else { "\nPhone: " });
		out.push(&p);
	}
}

/// Minor units as the amount a person reads: whole when it is whole, else with its cents.
/// Every currency the panel takes has two decimals.
fn money(minor: i64) -> String {
	let sign = if minor < 0 { "-" } else { "" };
	let abs = minor.unsigned_abs();
	if abs.is_multiple_of(100) {
		format!("{sign}{}", abs / 100)
	} else {
		format!("{sign}{}.{:02}", abs / 100, abs % 100)
	}
}

/// UTC, to the minute.
fn minute(at: Timestamp) -> String {
	at.strftime("%Y-%m-%d %H:%M UTC").to_string()
}

/// The line added to a lead's message once a button was pressed; the buttons left under it
/// are [`after_press`].
pub fn pressed_line(button: Button, who: &str, locale: Locale) -> String {
	let who = one_line(who, MAX_NAME);
	match (button, locale) {
		(Button::Take, Locale::Ru) => format!("Взял: {who}"),
		(Button::Take, Locale::En) => format!("Taken by: {who}"),
		(Button::NoAnswer, Locale::Ru) => format!("Не дозвонился: {who}"),
		(Button::NoAnswer, Locale::En) => format!("No answer: {who}"),
	}
}

/// The buttons a message keeps after one was pressed: none once taken; "Take" still, after
/// a call nobody answered.
pub fn after_press(button: Button) -> &'static [Button] {
	match button {
		Button::Take => &[],
		Button::NoAnswer => &[Button::Take],
	}
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
	/// The lead is gone from the projection (a rebuild dropped it).
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
		let data = callback_data(&KEY, 42, 1001, Button::Take);
		assert!(data.len() <= 64, "{data}");
		assert_eq!(parse_callback(&KEY, 42, &data), Some((1001, Button::Take)));
		assert_eq!(parse_callback(&KEY, 43, &data), None, "another chat");
		assert_eq!(parse_callback(&[8; 32], 42, &data), None, "another key");
		assert_eq!(parse_callback(&KEY, 42, &data.replacen("t.1001", "t.1002", 1)), None, "another message");
		assert_eq!(parse_callback(&KEY, 42, &data.replacen("t.", "n.", 1)), None, "another button");
		assert_eq!(parse_callback(&KEY, 42, "t.1001"), None);
		assert_eq!(parse_callback(&KEY, 42, "t.1001.zz"), None);
		assert_eq!(parse_callback(&KEY, 42, ""), None);
		let big = callback_data(&KEY, i64::MIN, i64::MAX, Button::NoAnswer);
		assert!(big.len() <= 64, "{big}");
	}

	#[test]
	fn texts() {
		let lead = LeadNote {
			brand: "aquafix".into(),
			location: Some("paris-11".into()),
			name: None,
			need: Some("leaking tap".into()),
			phone: Some("+33 6 00 00 00 00".into()),
		};
		assert_eq!(
			Note::NewLead(lead.clone()).text(Locale::Ru),
			"Новая заявка\naquafix · paris-11\nНужно: leaking tap\nТелефон: +33 6 00 00 00 00"
		);
		let withheld = LeadNote {
			need: None,
			phone: None,
			..lead.clone()
		};
		assert_eq!(Note::NewLead(withheld.clone()).text(Locale::En), "New lead\naquafix · paris-11");
		let doubted = Note::SuspectLead {
			lead: withheld,
			suspect: LeadSuspect::TooFast,
		};
		assert_eq!(doubted.text(Locale::En), "Suspect lead (antispam: form filled too fast)\naquafix · paris-11");
		assert_eq!((doubted.rule(), doubted.buttons()), (Rule::NewLead, [Button::Take, Button::NoAnswer].as_slice()));
		let overdue = Note::ContactOverdue {
			lead,
			waiting: SignedDuration::from_mins(47),
		};
		assert!(overdue.text(Locale::Ru).starts_with("Заявка ждёт звонка 47 мин\n"));
		assert_eq!(overdue.buttons(), [Button::Take, Button::NoAnswer]);
		let paid = Note::PaymentReceived {
			brand: "aquafix".into(),
			lead: "L-1".into(),
			billed: 12050,
			commission: 3000,
			currency: "EUR".into(),
		};
		assert_eq!(paid.text(Locale::En), "Payment received\naquafix · L-1\nBilled: 120.50 EUR\nCommission: 30 EUR");
		assert!(paid.buttons().is_empty());
		let silent = Note::SourceSilent {
			key_id: "aquafix-site".into(),
			kind: "site".into(),
			last: Some("2026-09-29T08:15:30Z".parse().unwrap()),
		};
		assert_eq!(
			silent.text(Locale::Ru),
			"Источник молчит больше суток\naquafix-site (site): последнее событие 2026-09-29 08:15 UTC"
		);
		assert_eq!(money(-5), "-0.05");
		let booked = Note::Booking {
			change: BookingChange::Booked,
			brand: "aquafix".into(),
			lead: Some(LeadNote {
				brand: "aquafix".into(),
				location: Some("royat".into()),
				phone: Some("+33612345678".into()),
				..LeadNote::default()
			}),
			slot: Some("2026-10-06 14:30 (Paris)".into()),
			provider: "google_calendar".into(),
		};
		assert_eq!(
			booked.text(Locale::Ru),
			"Бронь: 2026-10-06 14:30 (Paris)\naquafix · royat\nТелефон: +33612345678\nЧерез: google_calendar"
		);
		assert_eq!((booked.rule(), booked.buttons()), (Rule::Booked, [].as_slice()));
		let orphan = Note::Booking {
			change: BookingChange::Canceled,
			brand: "vifnet".into(),
			lead: None,
			slot: None,
			provider: "google_calendar".into(),
		};
		assert_eq!(orphan.text(Locale::En), "Booking canceled, no lead\nvifnet\nVia: google_calendar");
	}

	#[test]
	fn a_customer_cannot_forge_our_lines_or_links() {
		let lead = LeadNote {
			brand: "aquafix".into(),
			location: None,
			name: Some("Eve\u{202e}\t".into()),
			need: Some(format!("tap\nВзял: Mallory\r\n\u{2028}see https://evil.example {}", "x".repeat(10 * 1024))),
			phone: Some("+33 6 <a>00\n00 https://x".into()),
		};
		let r = Note::NewLead(lead).render(Locale::Ru);
		let lines: Vec<&str> = r.text.lines().collect();
		assert_eq!(lines.len(), 5, "{lines:?}");
		assert!(!lines.iter().any(|l| l.starts_with("Взял")), "no line of ours from the customer");
		assert_eq!(lines[2], "Имя: Eve");
		assert_eq!(lines[4], "Телефон: +33 6 00 00", "digits and phone punctuation only");
		let need = lines[3].strip_prefix("Нужно: ").unwrap();
		assert_eq!(need.chars().count(), MAX_NEED);
		assert!(need.starts_with("tap Взял: Mallory see https://evil.example x") && need.ends_with('…'));
		let units: Vec<u16> = r.text.encode_utf16().collect();
		let spans: Vec<String> = r.code.iter().map(|e| String::from_utf16(&units[e.offset..e.offset + e.length]).unwrap()).collect();
		assert_eq!(spans, ["Eve", need], "the typed fields, exactly, are code");
		assert!(r.text.chars().count() <= MAX_MESSAGE);
		assert_eq!(one_line("  a \u{2066}b\u{2069}  c ", 80), "a b c");
		assert_eq!(one_line("abcdef", 4), "abc…");
		let mut long = Rendered::default();
		long.push("ab");
		long.code("cdef");
		long.clamp(4);
		assert_eq!((long.text.as_str(), long.code.as_slice()), ("abcd", [Entity { offset: 2, length: 2 }].as_slice()));
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

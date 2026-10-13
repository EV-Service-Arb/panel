//! Booking without the I/O (FORM-VARIANTS-SPEC, "Booking providers contract", 2026-10-04):
//! the providers and how a place configures them, the statuses a lead's booking goes
//! through, and how its booking is folded from the facts about it.
//!
//! Provider-agnostic on purpose: a provider is a word of a closed vocabulary with its own URL
//! rule, and its adapter (a webhook it pushes, or a calendar the panel pulls) only ever hands
//! the engine a normalized [`BookingEvent`].

use std::fmt;

use jiff::{Timestamp, civil::Date};
use serde_json::{Map, Value};

use crate::{Invalid, ids::EventId};

/// Where a slot is booked.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Provider {
	/// No external page: the site promises a call, and an operator records the slot.
	Manual,
	/// An external booking page on any https host; no adapter reads it back.
	Link,
	/// A Google Calendar appointment schedule; its bookings are pulled from the calendar.
	GoogleCalendar,
	/// Cal.com, ours (`cal.evinvest.ltd`) or the hosted cloud.
	CalCom,
}

impl Provider {
	pub const ALL: [Self; 4] = [Self::Manual, Self::Link, Self::GoogleCalendar, Self::CalCom];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Manual => "manual",
			Self::Link => "link",
			Self::GoogleCalendar => "google_calendar",
			Self::CalCom => "cal_com",
		}
	}

	pub fn parse(raw: &str) -> Result<Self, Invalid> {
		Self::ALL
			.into_iter()
			.find(|p| p.as_str() == raw)
			.ok_or_else(|| Invalid::new("provider is not one of manual, link, google_calendar, cal_com"))
	}

	/// Whether a place configures it with a booking page of its own. `manual` is always
	/// available, so never configured.
	pub fn has_page(self) -> bool {
		self != Self::Manual
	}

	/// Whether an adapter reports this provider's bookings (`booking.created`/`canceled`).
	pub fn has_adapter(self) -> bool {
		match self {
			Self::GoogleCalendar | Self::CalCom => true,
			Self::Manual | Self::Link => false,
		}
	}
}

impl fmt::Display for Provider {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

// ── a place's configuration ─────────────────────────────────────────────────────────────

/// The longest booking URL.
pub const MAX_URL: usize = 2048;

/// The Cal.com hosts a place's `cal_com` page may be on (kitstart's `calComHosts` default,
/// `rules.json` of the fixtures).
pub const CAL_COM_HOSTS: [&str; 2] = ["cal.evinvest.ltd", "cal.com"];

/// `[A-Za-z0-9][A-Za-z0-9._-]{0,99}`: a Cal.com user or event slug.
fn is_cal_segment(s: &str) -> bool {
	(1..=100).contains(&s.len()) && s.starts_with(|c: char| c.is_ascii_alphanumeric()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `url` may be a `provider`'s booking page: kitstart's URL rule (the fixtures'
/// README, normative). Every provider with a page: at most [`MAX_URL`] printable ASCII; the
/// literal `https://`; no `#` and no `\`; an authority without `@`, `:` or `[`; a dotted DNS
/// name of two labels at least, whose last is not a number (`\d+`, `0x…`: an IPv4 address to a
/// WHATWG parser); a query allowed. Hosts compare lowercase. Then by provider:
/// `google_calendar` — `calendar.app.google/…` (a short link) or
/// `calendar.google.com/calendar/appointments/…`; `cal_com` — a host of [`CAL_COM_HOSTS`]
/// exactly, the path exactly `/<user>/<event>`; `link` — any host. `manual` has no page.
pub fn check_url(provider: Provider, raw: &str) -> Result<(), String> {
	if !provider.has_page() {
		return Err(format!("{provider} takes no url"));
	}
	if raw.is_empty() || raw.len() > MAX_URL || !raw.bytes().all(|b| b.is_ascii_graphic()) {
		return Err(format!("must be an https:// URL of at most {MAX_URL} printable ASCII characters, without spaces"));
	}
	let rest = raw.strip_prefix("https://").ok_or("must start with https://")?;
	if raw.contains('#') {
		return Err("must have no fragment (#…)".into());
	}
	if raw.contains('\\') {
		return Err("must have no backslash".into());
	}
	let split = rest.find(['/', '?']).unwrap_or(rest.len());
	let (host, tail) = rest.split_at(split);
	if host.contains('@') {
		return Err("must have no user or password".into());
	}
	if host.starts_with('[') {
		return Err("host must be a name, not an IP address".into());
	}
	if host.contains(':') {
		return Err("must have no port".into());
	}
	let labels: Vec<&str> = host.split('.').collect();
	let label_ok = |l: &&str| (1..=63).contains(&l.len()) && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
	if host.len() > 253 || labels.len() < 2 || !labels.iter().all(label_ok) {
		return Err("host must be a domain name".into());
	}
	let last = labels.last().map(|l| l.to_ascii_lowercase()).unwrap_or_default();
	if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
		return Err("host must be a name, not an IP address".into());
	}
	let host = host.to_ascii_lowercase();
	let path = tail.split('?').next().unwrap_or_default();
	match provider {
		Provider::Manual => Err("manual takes no url".into()),
		Provider::Link => Ok(()),
		Provider::GoogleCalendar => {
			const APPOINTMENTS: &str = "/calendar/appointments/";
			let ok = (host == "calendar.app.google" && path.len() > 1) || (host == "calendar.google.com" && path.len() > APPOINTMENTS.len() && path.starts_with(APPOINTMENTS));
			if ok {
				Ok(())
			} else {
				Err("must be a Google appointment schedule: https://calendar.app.google/… or https://calendar.google.com/calendar/appointments/…".into())
			}
		}
		Provider::CalCom => {
			if !CAL_COM_HOSTS.contains(&host.as_str()) {
				return Err(format!("must be a Cal.com page on {}", CAL_COM_HOSTS.join(" or ")));
			}
			let segments: Vec<&str> = path.strip_prefix('/').unwrap_or_default().split('/').collect();
			if segments.len() == 2 && segments.iter().all(|s| is_cal_segment(s)) {
				Ok(())
			} else {
				Err("must be a Cal.com page /<user>/<event>".into())
			}
		}
	}
}

/// kitstart's `leadRef`, the panel's lead id for a site's lead: `lead-<row>-<8 hex>`, the row
/// 1 or more, the hex lowercase.
pub fn is_lead_ref(s: &str) -> bool {
	let Some(rest) = s.strip_prefix("lead-") else { return false };
	let Some((row, hex)) = rest.split_once('-') else { return false };
	let row_ok = (1..=19).contains(&row.len()) && row.bytes().all(|b| b.is_ascii_digit()) && !row.starts_with('0');
	row_ok && hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `booking.requested@1`'s properties, checked (the fixtures' README): a lead ref, a provider,
/// and a wish — date, part of the day — for `manual` only.
pub fn requested(lead_ref: &str, provider: &str, date: Option<Date>, part: Option<&str>) -> Result<(Provider, Option<Date>, Option<DayPart>), Invalid> {
	if !is_lead_ref(lead_ref) {
		return Err(Invalid::new("properties.lead_ref is not lead-<row>-<8 lowercase hex>"));
	}
	let provider = Provider::parse(provider).map_err(|e| Invalid::new(format!("properties.{e}")))?;
	let part = part.map(DayPart::parse).transpose()?;
	if provider != Provider::Manual && (date.is_some() || part.is_some()) {
		return Err(Invalid::new("properties.preferred_date and preferred_part are for provider manual only"));
	}
	Ok((provider, date, part))
}

/// How far from the day it arrives a visitor's preferred day may be: two days back (clocks and
/// an outbox's delay), a year and a day ahead.
pub fn preferred_date_in_window(date: Date, today: Date) -> bool {
	let days = (date - today).get_days();
	(-2..=366).contains(&days)
}

/// What was wrong inside a place's `booking`: `(path below the field, why)`, every one found
/// — `.default`, `.providers.google_calendar.url`.
pub type ConfigProblems = Vec<(String, String)>;

/// A place's `booking` setting: `{default: <provider>, providers: {<provider>: {url}}}` —
/// `manual` never among `providers`, it is always there — checked whole, every problem named by its path. `default` is `manual`
/// or one of `providers`. Answers the setting as stored: the same object, its keys sorted.
pub fn check_config(v: &Value) -> Result<Value, ConfigProblems> {
	let mut bad = ConfigProblems::new();
	let Value::Object(m) = v else {
		return Err(vec![(String::new(), "must be {\"default\": …, \"providers\": {…}}".into())]);
	};
	for k in m.keys().filter(|k| !matches!(k.as_str(), "default" | "providers")) {
		bad.push((String::new(), format!("{k} is not one of default, providers")));
	}
	let default = match m.get("default") {
		None => {
			bad.push((".default".into(), "is required".into()));
			None
		}
		Some(Value::String(s)) => match Provider::parse(s) {
			Ok(p) => Some(p),
			Err(e) => {
				bad.push((".default".into(), e.0));
				None
			}
		},
		Some(_) => {
			bad.push((".default".into(), "must be a provider's name".into()));
			None
		}
	};
	let mut providers = Map::new();
	let mut named = Vec::new();
	match m.get("providers") {
		None => bad.push((".providers".into(), "is required; {} for none".into())),
		Some(Value::Object(ps)) =>
			for (name, conf) in ps {
				let path = format!(".providers.{name}");
				let Ok(provider) = Provider::parse(name) else {
					bad.push((path, "is not one of manual, link, google_calendar, cal_com".into()));
					continue;
				};
				let Value::Object(conf) = conf else {
					bad.push((path, "must be an object".into()));
					continue;
				};
				if let Some(k) = conf.keys().find(|k| *k != "url") {
					bad.push((path.clone(), format!("{k} is not a setting; only url")));
					continue;
				}
				match (provider, conf.get("url")) {
					// Always there, so never configured: the README's rule.
					(p, _) if !p.has_page() => bad.push((path, "manual is always available and is never a key of providers".into())),
					(_, None) => bad.push((format!("{path}.url"), "is required".into())),
					(_, Some(Value::String(url))) => match check_url(provider, url) {
						Ok(()) => {
							providers.insert(name.clone(), serde_json::json!({ "url": url }));
							named.push(provider);
						}
						Err(why) => bad.push((format!("{path}.url"), why)),
					},
					(_, Some(_)) => bad.push((format!("{path}.url"), "must be a string".into())),
				}
			},
		Some(_) => bad.push((".providers".into(), "must be an object of a provider's settings by its name".into())),
	}
	if let Some(d) = default
		&& d.has_page()
		&& !named.contains(&d)
		&& !bad.iter().any(|(p, _)| p.starts_with(&format!(".providers.{d}")))
	{
		bad.push((".default".into(), "must be manual or one of the providers set".into()));
	}
	match (bad.is_empty(), default) {
		(true, Some(d)) => Ok(serde_json::json!({ "default": d.as_str(), "providers": providers })),
		_ => Err(bad),
	}
}

// ── a lead's booking ────────────────────────────────────────────────────────────────────

/// Where a lead's booking stands.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum BookingStatus {
	#[default]
	None,
	/// The site asked for a slot (`booking.requested`); nobody has one yet.
	Requested,
	Booked,
	Canceled,
	Done,
	NoShow,
}

impl BookingStatus {
	pub const ALL: [Self; 6] = [Self::None, Self::Requested, Self::Booked, Self::Canceled, Self::Done, Self::NoShow];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::None => "none",
			Self::Requested => "requested",
			Self::Booked => "booked",
			Self::Canceled => "canceled",
			Self::Done => "done",
			Self::NoShow => "no_show",
		}
	}

	pub fn parse(raw: &str) -> Result<Self, Invalid> {
		Self::ALL
			.into_iter()
			.find(|s| s.as_str() == raw)
			.ok_or_else(|| Invalid::new("booking status is not one of none, requested, booked, canceled, done, no_show"))
	}

	/// Whether an operator may do `action` to a booking that stands here: set a slot unless
	/// it is done; clear one that is there to clear (not from none, not once done); close a
	/// booked one only.
	pub fn allows(self, action: OperatorAction) -> bool {
		match action {
			OperatorAction::Set => self != Self::Done,
			OperatorAction::Clear => matches!(self, Self::Requested | Self::Booked | Self::Canceled | Self::NoShow),
			OperatorAction::Close(_) => self == Self::Booked,
		}
	}
}

/// How a booked slot ended, as an operator says (`booking.status_changed`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Closed {
	Done,
	NoShow,
	Canceled,
}

impl Closed {
	pub fn as_str(self) -> &'static str {
		self.status().as_str()
	}

	pub fn status(self) -> BookingStatus {
		match self {
			Self::Done => BookingStatus::Done,
			Self::NoShow => BookingStatus::NoShow,
			Self::Canceled => BookingStatus::Canceled,
		}
	}

	pub fn parse(raw: &str) -> Result<Self, Invalid> {
		[Self::Done, Self::NoShow, Self::Canceled]
			.into_iter()
			.find(|c| c.as_str() == raw)
			.ok_or_else(|| Invalid::new("status is not one of done, no_show, canceled"))
	}
}

/// What an operator does to a lead's booking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorAction {
	Set,
	Clear,
	Close(Closed),
}

/// `as_str` and `parse` for a fieldless enum spelled on the wire as snake-case words.
macro_rules! wire_enum {
	($ty:ident, $refused:literal, { $($variant:ident => $wire:literal),+ $(,)? }) => {
		impl $ty {
			pub const ALL: [Self; [$(stringify!($variant)),+].len()] = [$(Self::$variant),+];

			pub fn as_str(self) -> &'static str {
				match self {
					$(Self::$variant => $wire,)+
				}
			}

			pub fn parse(raw: &str) -> Result<Self, Invalid> {
				Self::ALL.into_iter().find(|v| v.as_str() == raw).ok_or_else(|| Invalid::new($refused))
			}
		}
	};
}

/// When in the day the visitor would like their slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayPart {
	Morning,
	Afternoon,
	Evening,
}

wire_enum!(DayPart, "properties.preferred_part is not one of morning, afternoon, evening", {
	Morning => "morning",
	Afternoon => "afternoon",
	Evening => "evening",
});

/// How a provider's booking was joined to its lead.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BookingMatch {
	/// By the ref the booking page carried (the lead's id).
	Ref,
	/// By the attendee's phone or email, one lead of the brand within the window.
	Contact,
	/// By an operator: a slot typed in, or a provider's booking attached by hand.
	Manual,
}

wire_enum!(BookingMatch, "match is not one of ref, contact, manual", {
	Ref => "ref",
	Contact => "contact",
	Manual => "manual",
});

/// A lead's booking now.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BookingState {
	pub status: BookingStatus,
	pub provider: Option<Provider>,
	pub start_at: Option<Timestamp>,
	pub end_at: Option<Timestamp>,
	/// The provider's booking, when the slot is one.
	pub external_ref: Option<String>,
	pub matched: Option<BookingMatch>,
	/// The visitor's wish, from the latest `booking.requested`.
	pub preferred_date: Option<Date>,
	pub preferred_part: Option<DayPart>,
}

/// One fact about a lead's booking, as its fold reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BookingItem {
	Requested {
		provider: Provider,
		date: Option<Date>,
		part: Option<DayPart>,
	},
	/// An operator's slot.
	Set {
		start: Timestamp,
		end: Option<Timestamp>,
	},
	Closed(Closed),
	Cleared,
	/// A provider's booking joined to the lead: booked (or moved) with its slot, or canceled.
	External {
		provider: Provider,
		external_ref: String,
		slot: Option<(Timestamp, Option<Timestamp>)>,
		matched: BookingMatch,
	},
}

/// Folds a lead's booking facts, in `(occurred_at, id)` order whatever order they come in.
/// What an operator could not have done from where the booking stood (closing one not
/// booked, clearing none) is passed over, so a race between two operators lands where the
/// first left it.
pub fn fold(items: &mut [(Timestamp, EventId, BookingItem)]) -> BookingState {
	items.sort_by_key(|(at, id, _)| (*at, id.raw()));
	let mut s = BookingState::default();
	for (_, _, item) in items.iter() {
		match item {
			BookingItem::Requested { provider, date, part } => {
				s.preferred_date = *date;
				s.preferred_part = *part;
				if matches!(s.status, BookingStatus::None | BookingStatus::Requested | BookingStatus::Canceled) {
					s = BookingState {
						status: BookingStatus::Requested,
						provider: Some(*provider),
						preferred_date: s.preferred_date,
						preferred_part: s.preferred_part,
						..BookingState::default()
					};
				}
			}
			BookingItem::Set { start, end } =>
				if s.status.allows(OperatorAction::Set) {
					s.status = BookingStatus::Booked;
					s.provider = Some(Provider::Manual);
					s.start_at = Some(*start);
					s.end_at = *end;
					s.external_ref = None;
					s.matched = Some(BookingMatch::Manual);
				},
			BookingItem::Closed(c) =>
				if s.status.allows(OperatorAction::Close(*c)) {
					s.status = c.status();
				},
			BookingItem::Cleared =>
				if s.status.allows(OperatorAction::Clear) {
					s = BookingState {
						preferred_date: s.preferred_date,
						preferred_part: s.preferred_part,
						..BookingState::default()
					};
				},
			BookingItem::External {
				provider,
				external_ref,
				slot,
				matched,
			} => {
				let same = s.external_ref.as_deref() == Some(external_ref.as_str());
				match slot {
					// A provider's change to a booking an operator already closed does not
					// reopen it.
					Some(_) if same && matches!(s.status, BookingStatus::Done | BookingStatus::NoShow) => {}
					Some((start, end)) => {
						s.status = BookingStatus::Booked;
						s.provider = Some(*provider);
						s.start_at = Some(*start);
						s.end_at = *end;
						s.external_ref = Some(external_ref.clone());
						s.matched = Some(*matched);
					}
					None =>
						if same && s.status == BookingStatus::Booked {
							s.status = BookingStatus::Canceled;
						},
				}
			}
		}
	}
	s
}

// ── a provider's bookings ───────────────────────────────────────────────────────────────

/// 1–1024 of `[A-Za-z0-9._:-]`: a provider's id of a booking (Google's event ids are
/// base32hex, Cal.com's uids alphanumeric with dashes). It sits in the clear.
pub fn is_external_ref(s: &str) -> bool {
	(1..=1024).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// 1–256 printable ASCII: a provider's revision of a booking (an etag, an update time).
pub fn is_version(s: &str) -> bool {
	(1..=256).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic())
}

/// What a provider says changed, normalized: every adapter, push or pull, answers this.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
	/// Booked, or moved to this slot.
	Booked {
		start: Timestamp,
		end: Option<Timestamp>,
		/// When the customer booked, if the provider says.
		booked_at: Option<Timestamp>,
	},
	Canceled,
}

/// Who booked, as the provider tells it: PII, sealed in the journal, compared with the
/// leads' to match, never in the clear.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Contact {
	pub name: Option<String>,
	/// Lowercase.
	pub email: Option<String>,
	/// E.164 ([`crate::phone::normalize`]).
	pub phone: Option<String>,
}

impl Contact {
	pub fn is_empty(&self) -> bool {
		self.name.is_none() && self.email.is_none() && self.phone.is_none()
	}
}

/// One provider's word about one booking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookingEvent {
	pub provider: Provider,
	pub external_ref: String,
	/// The provider's revision: the same revision seen twice is one journal event.
	pub version: String,
	/// When the provider says it changed.
	pub at: Timestamp,
	pub change: Change,
	/// The lead's id, when the booking page carried it back (a push provider's metadata).
	pub lead_ref: Option<String>,
	pub contact: Contact,
}

/// A provider's booking, folded from its own events and the operators' attachments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalBooking {
	/// Its latest slot, kept once canceled: what it was for.
	pub start: Timestamp,
	pub end: Option<Timestamp>,
	pub canceled: bool,
	pub booked_at: Option<Timestamp>,
	/// The lead and how it was found: the latest attachment, else the match the first event
	/// that named a lead was journaled with.
	pub lead: Option<(String, BookingMatch)>,
	/// The latest `booking.created`: what the attendee's PII is read from.
	pub created_event: EventId,
	pub first_event: EventId,
	pub last_event: EventId,
	pub last_event_at: Timestamp,
}

/// One event of a provider's booking, as [`fold_external`] reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExternalItem {
	Created {
		start: Timestamp,
		end: Option<Timestamp>,
		booked_at: Option<Timestamp>,
		lead: Option<(String, BookingMatch)>,
	},
	Canceled,
	Attached {
		lead: String,
	},
}

/// A provider's booking from its events, in `(occurred_at, id)` order; `None` while no
/// `booking.created` names it (a cancellation or an attachment alone is not a booking).
pub fn fold_external(items: &mut [(Timestamp, EventId, ExternalItem)]) -> Option<ExternalBooking> {
	items.sort_by_key(|(at, id, _)| (*at, id.raw()));
	let (first_created, start, end) = items.iter().enumerate().find_map(|(i, (_, _, item))| match item {
		ExternalItem::Created { start, end, .. } => Some((i, *start, *end)),
		ExternalItem::Canceled | ExternalItem::Attached { .. } => None,
	})?;
	let (first_at, first_id, _) = &items[0];
	let mut b = ExternalBooking {
		start,
		end,
		canceled: false,
		booked_at: None,
		lead: None,
		created_event: items[first_created].1,
		first_event: *first_id,
		last_event: *first_id,
		last_event_at: *first_at,
	};
	let mut attached = None;
	for (at, id, item) in items.iter() {
		match item {
			ExternalItem::Created { start, end, booked_at, lead } => {
				(b.start, b.end, b.canceled) = (*start, *end, false);
				b.booked_at = booked_at.or(b.booked_at);
				b.created_event = *id;
				if b.lead.is_none() {
					b.lead.clone_from(lead);
				}
			}
			ExternalItem::Canceled => b.canceled = true,
			ExternalItem::Attached { lead } => attached = Some(lead.clone()),
		}
		b.last_event = *id;
		b.last_event_at = *at;
	}
	if let Some(lead) = attached {
		b.lead = Some((lead, BookingMatch::Manual));
	}
	Some(b)
}

#[cfg(test)]
mod tests {
	use jiff::SignedDuration;
	use serde_json::json;
	use uuid::Uuid;

	use super::*;

	/// The contract's URL rules, as a table: (provider, url, accepted).
	const URLS: &[(Provider, &str, bool)] = &[
		(Provider::GoogleCalendar, "https://calendar.app.google/AbCd1234XyZ", true),
		(Provider::GoogleCalendar, "https://calendar.google.com/calendar/appointments/schedules/AcZssZ3x", true),
		(Provider::GoogleCalendar, "https://calendar.google.com/calendar/appointments/AcZ?gv=true", true),
		(Provider::GoogleCalendar, "https://calendar.app.google/", false),
		(Provider::GoogleCalendar, "https://calendar.google.com/calendar/r", false),
		(Provider::GoogleCalendar, "https://calendar.google.com/calendar/appointments/", false),
		(Provider::GoogleCalendar, "https://evil.calendar.app.google.example/x", false),
		(Provider::GoogleCalendar, "https://calendly.com/aquafix/30min", false),
		(Provider::CalCom, "https://cal.evinvest.ltd/aquafix/devis", true),
		(Provider::CalCom, "https://cal.com/vifnet/menage", true),
		(Provider::CalCom, "https://app.cal.com/vifnet/menage", false),
		(Provider::CalCom, "https://cal.com/", false),
		(Provider::Link, "https://booking.example.fr/rdv?x=1", true),
		(Provider::Link, "https://example.fr", true),
		(Provider::Link, "http://example.fr/rdv", false),
		(Provider::Link, "ftp://example.fr/rdv", false),
		(Provider::Link, "javascript:alert(1)", false),
		(Provider::Link, "https://user:pw@example.fr/rdv", false),
		(Provider::Link, "https://user@example.fr/rdv", false),
		(Provider::Link, "https://example.fr:8443/rdv", false),
		(Provider::Link, "https://example.fr/rdv#ref", false),
		(Provider::Link, "https://127.0.0.1/rdv", false),
		(Provider::Link, "https://127.1/rdv", false),
		(Provider::Link, "https://0x7f.0.0.1/rdv", false),
		(Provider::Link, "https://[::1]/rdv", false),
		(Provider::Link, "https://localhost/rdv", false),
		(Provider::Link, "https://Example.fr/rdv", true),
		(Provider::Link, "https://example.fr\\rdv", false),
		(Provider::CalCom, "https://cal.com/vifnet/menage/", false),
		(Provider::CalCom, "https://cal.com/vifnet", false),
		(Provider::CalCom, "https://Cal.Com/vifnet/menage", true),
		(Provider::Link, "https://exa mple.fr/rdv", false),
		(Provider::Link, "https://exämple.fr/rdv", false),
		(Provider::Link, "https://-a.fr/rdv", false),
		(Provider::Link, "https://", false),
		(Provider::Link, "", false),
		(Provider::Manual, "https://example.fr", false),
	];

	#[test]
	fn urls_by_provider() {
		for (provider, url, ok) in URLS {
			assert_eq!(check_url(*provider, url).is_ok(), *ok, "{provider} {url}: {:?}", check_url(*provider, url));
		}
		let long = format!("https://example.fr/{}", "a".repeat(MAX_URL));
		assert!(check_url(Provider::Link, &long).is_err());
		let fits = format!("https://example.fr/{}", "a".repeat(MAX_URL - "https://example.fr/".len()));
		assert!(check_url(Provider::Link, &fits).is_ok());
	}

	#[test]
	fn configs() {
		let good = json!({
			"default": "google_calendar",
			"providers": {"google_calendar": {"url": "https://calendar.app.google/AbCd"}},
		});
		assert_eq!(check_config(&good).unwrap(), good);
		assert!(check_config(&json!({"default": "manual", "providers": {}})).is_ok(), "manual needs no entry");
		for (body, path) in [
			(json!({"default": "calendly", "providers": {}}), ".default"),
			(json!({"default": "link", "providers": {}}), ".default"),
			(json!({"providers": {}}), ".default"),
			(json!({"default": "manual"}), ".providers"),
			(json!({"default": "manual", "providers": {"manual": {"url": "https://x.fr"}}}), ".providers.manual"),
			(json!({"default": "manual", "providers": {"manual": {}}}), ".providers.manual"),
			(json!({"default": "manual", "providers": {"link": {}}}), ".providers.link.url"),
			(json!({"default": "manual", "providers": {"link": {"url": "http://x.fr"}}}), ".providers.link.url"),
			(json!({"default": "manual", "providers": {"link": {"url": "https://x.fr", "label": "x"}}}), ".providers.link"),
			(json!({"default": "manual", "providers": {"calendly": {"url": "https://calendly.com/x"}}}), ".providers.calendly"),
			(json!({"default": "manual", "providers": {}, "ab": true}), ""),
			(json!("manual"), ""),
		] {
			let problems = check_config(&body).unwrap_err();
			assert!(problems.iter().any(|(p, _)| p == path), "{body}: {problems:?}");
		}
		let bad_default_and_url = check_config(&json!({"default": "cal_com", "providers": {"cal_com": {"url": "https://x.fr/a"}}})).unwrap_err();
		assert_eq!(bad_default_and_url.len(), 1, "the default is not blamed for its provider's url: {bad_default_and_url:?}");
	}

	fn at(m: i64) -> Timestamp {
		"2026-10-05T08:00:00Z".parse::<Timestamp>().unwrap() + SignedDuration::from_mins(m)
	}

	fn id(n: u128) -> EventId {
		EventId::from_raw(Uuid::from_u128(n))
	}

	fn ext(slot: Option<i64>) -> BookingItem {
		BookingItem::External {
			provider: Provider::GoogleCalendar,
			external_ref: "ev1".into(),
			slot: slot.map(|m| (at(m), Some(at(m + 60)))),
			matched: BookingMatch::Contact,
		}
	}

	#[test]
	fn a_leads_booking() {
		let requested = BookingItem::Requested {
			provider: Provider::GoogleCalendar,
			date: Some("2026-10-07".parse().unwrap()),
			part: Some(DayPart::Morning),
		};
		let mut items = vec![(at(0), id(1), requested), (at(5), id(2), ext(Some(3000)))];
		let s = fold(&mut items);
		assert_eq!(
			(s.status, s.provider, s.matched),
			(BookingStatus::Booked, Some(Provider::GoogleCalendar), Some(BookingMatch::Contact))
		);
		assert_eq!(s.preferred_part, Some(DayPart::Morning), "the wish stays beside the slot");
		items.push((at(6), id(3), ext(Some(4000))));
		assert_eq!(fold(&mut items).start_at, Some(at(4000)), "moved");
		items.push((at(7), id(4), ext(None)));
		assert_eq!(fold(&mut items).status, BookingStatus::Canceled);
		items.reverse();
		assert_eq!(fold(&mut items).status, BookingStatus::Canceled, "whatever the order read in");

		let mut manual = vec![
			(at(0), id(1), BookingItem::Set { start: at(100), end: None }),
			(at(1), id(2), BookingItem::Closed(Closed::Done)),
			(at(2), id(3), BookingItem::Cleared),
			(at(3), id(4), BookingItem::Set { start: at(200), end: None }),
		];
		let s = fold(&mut manual);
		assert_eq!((s.status, s.start_at), (BookingStatus::Done, Some(at(100))), "a done booking is neither cleared nor set again");
		let mut raced = vec![(at(0), id(1), BookingItem::Closed(Closed::NoShow)), (at(1), id(2), BookingItem::Cleared)];
		assert_eq!(fold(&mut raced), BookingState::default(), "nothing booked: nothing to close or clear");
	}

	#[test]
	fn transitions() {
		use BookingStatus::*;
		let close = OperatorAction::Close(Closed::Done);
		for (from, set, clear, closes) in [
			(None, true, false, false),
			(Requested, true, true, false),
			(Booked, true, true, true),
			(Canceled, true, true, false),
			(Done, false, false, false),
			(NoShow, true, true, false),
		] {
			assert_eq!(
				(from.allows(OperatorAction::Set), from.allows(OperatorAction::Clear), from.allows(close)),
				(set, clear, closes),
				"{from:?}"
			);
		}
	}

	#[test]
	fn a_providers_booking() {
		let created = |m: i64, lead: Option<&str>| ExternalItem::Created {
			start: at(m),
			end: None,
			booked_at: Some(at(0)),
			lead: lead.map(|l| (l.to_owned(), BookingMatch::Contact)),
		};
		assert!(fold_external(&mut [(at(0), id(1), ExternalItem::Canceled)]).is_none());
		let mut items = vec![(at(0), id(1), created(100, None)), (at(1), id(2), created(200, Some("L-9")))];
		let b = fold_external(&mut items).unwrap();
		assert_eq!((b.start, b.canceled, b.lead.clone()), (at(200), false, Some(("L-9".into(), BookingMatch::Contact))));
		items.push((at(2), id(3), ExternalItem::Attached { lead: "L-1".into() }));
		items.push((at(3), id(4), ExternalItem::Canceled));
		let b = fold_external(&mut items).unwrap();
		assert_eq!((b.start, b.canceled, b.lead), (at(200), true, Some(("L-1".into(), BookingMatch::Manual))));
		assert_eq!((b.created_event, b.first_event, b.last_event), (id(2), id(1), id(4)));
	}

	#[test]
	fn lead_refs_and_wishes() {
		assert!(is_lead_ref("lead-42-0a1b2c3d"));
		for bad in ["lead-0-0a1b2c3d", "lead-42-0A1B2C3D", "42", "lead-42-0a1b2c3", "lead-042-0a1b2c3d", "lead--0a1b2c3d"] {
			assert!(!is_lead_ref(bad), "{bad}");
		}
		let today: Date = "2026-10-05".parse().unwrap();
		assert!(preferred_date_in_window("2026-10-03".parse().unwrap(), today));
		assert!(!preferred_date_in_window("2026-10-02".parse().unwrap(), today));
		assert!(preferred_date_in_window("2027-10-06".parse().unwrap(), today));
		assert!(!preferred_date_in_window("2027-10-07".parse().unwrap(), today));
	}

	#[test]
	fn refs_and_versions() {
		assert!(is_external_ref("abc_123-x.y:z"));
		assert!(!is_external_ref("a b") && !is_external_ref("") && !is_external_ref(&"a".repeat(1025)));
		assert!(is_version("\"3412345678901234\"") && !is_version("a b"));
	}
}

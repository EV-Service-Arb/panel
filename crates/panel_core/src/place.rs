//! A place's live settings: the half of a landing's place that the panel may change without a
//! release (kitstart's `PlaceLive`, `ts/kitstart/src/core/place/live.ts`). A site fetches them
//! and lays them over its baked config, field by field: whatever is left out, the site's own
//! config supplies.
//!
//! The wire shape is kitstart's, and so are the rules, with one difference: kitstart drops a
//! field that does not validate (a page must not fail on one bad field), the panel refuses it
//! and says why, so whoever edits sees it instead of the site silently keeping its old value.
//! Some rules are stricter than kitstart's, never looser: everything the panel accepts,
//! kitstart keeps.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::fact::Messenger;

/// Why each field was refused, by its wire name — or its path inside a list, `hours[0].opens`,
/// `hours[1].days`, `serviceArea[2]`: what the editor shows beside the field.
pub type FieldErrors = BTreeMap<String, String>;

/// Every field a place's settings may set, by its wire name.
pub const FIELDS: [&str; 12] = [
	"phone", "whatsapp", "telegram", "messengers", "hours", "serviceArea", "address", "geo", "storefrontPhoto", "landmark", "rating", "booking",
];

/// kitstart's `DayOfWeek`, in week order.
pub const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// The short names `panel place set --hours` takes, in [`DAYS`] order.
const SHORT_DAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

/// A line of text: a name, a street, a landmark.
const MAX_TEXT: usize = 200;
const MAX_URL: usize = 2048;
/// Rows of opening hours: one per day twice over (a lunch break) is the most a week needs.
const MAX_HOURS: usize = 14;
const MAX_AREA: usize = 200;
const MAX_LOCALES: usize = 8;

/// A place's live settings, checked: only the fields that are set, each in kitstart's shape.
/// Empty: the site serves its baked config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaceSettings(Map<String, Value>);

impl PlaceSettings {
	/// Checks a whole set of settings (a full replace, not a patch). A field set to `null` is
	/// left out, as if absent. Every bad field is named, not only the first.
	pub fn parse(body: &Value) -> Result<Self, FieldErrors> {
		let Value::Object(fields) = body else {
			return Err(one("settings", "must be an object"));
		};
		let mut out = Map::new();
		let mut errors = FieldErrors::new();
		for (name, value) in fields {
			if value.is_null() {
				continue;
			}
			let whole = |r: Result<Value, String>| r.map_err(|e| vec![(String::new(), e)]);
			let checked = match name.as_str() {
				"phone" | "whatsapp" => whole(phone(value)),
				"telegram" => whole(telegram_bot(value)),
				"messengers" => messengers(value),
				"hours" => hours(value),
				"serviceArea" => service_area(value),
				"address" => whole(address(value)),
				"geo" => whole(geo(value)),
				"storefrontPhoto" => whole(https_url(value)),
				"landmark" => whole(landmark(value)),
				"rating" => whole(rating(value)),
				// The providers the place offers and its default (`crate::booking`).
				"booking" => crate::booking::check_config(value),
				_ => whole(Err(format!("is not a setting; one of {}", FIELDS.join(", ")))),
			};
			match checked {
				Ok(v) => {
					out.insert(name.clone(), v);
				}
				// A part of a list is named by its path: `hours[0].opens`, `serviceArea[2]`.
				Err(bad) => errors.extend(bad.into_iter().map(|(path, why)| (format!("{name}{path}"), why))),
			}
		}
		if errors.is_empty() { Ok(Self(out)) } else { Err(errors) }
	}

	/// Settings as they were stored: an object, not checked again. The rules may have grown
	/// stricter since, and the history must still read.
	pub fn stored(value: Value) -> Option<Self> {
		match value {
			Value::Object(m) => Some(Self(m)),
			_ => None,
		}
	}

	pub fn as_json(&self) -> Value {
		Value::Object(self.0.clone())
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn get(&self, field: &str) -> Option<&Value> {
		self.0.get(field)
	}

	/// These settings with `set` laid over them and `clear` removed, checked whole: what the
	/// CLI does, one field at a time.
	pub fn patched(&self, set: Map<String, Value>, clear: &[String]) -> Result<Self, FieldErrors> {
		let mut next = self.0.clone();
		let mut errors = FieldErrors::new();
		for field in clear {
			if !FIELDS.contains(&field.as_str()) {
				errors.insert(field.clone(), format!("is not a setting; one of {}", FIELDS.join(", ")));
			}
			next.remove(field);
		}
		next.extend(set);
		let parsed = Self::parse(&Value::Object(next));
		match parsed {
			Ok(s) if errors.is_empty() => Ok(s),
			Ok(_) => Err(errors),
			Err(e) => {
				errors.extend(e);
				Err(errors)
			}
		}
	}
}

fn one(field: &str, msg: &str) -> FieldErrors {
	FieldErrors::from([(field.to_owned(), msg.to_owned())])
}

/// A line of text, trimmed: not blank, at most `max` characters, no control characters.
fn text(v: &Value, max: usize) -> Result<String, String> {
	let Value::String(s) = v else { return Err("must be a string".into()) };
	let s = s.trim();
	if s.is_empty() {
		return Err("must not be blank".into());
	}
	if s.chars().count() > max {
		return Err(format!("must be at most {max} characters"));
	}
	if s.chars().any(char::is_control) {
		return Err("must be one line, without control characters".into());
	}
	Ok(s.to_owned())
}

/// `+` and 7 to 15 digits, the first not 0: E.164, which is what `tel:` and `wa.me` links take.
/// kitstart only asks for the `+`.
fn phone(v: &Value) -> Result<Value, String> {
	const MSG: &str = "must be E.164, e.g. +33612345678";
	let s = text(v, 16).map_err(|_| MSG.to_owned())?;
	let digits = s.strip_prefix('+').ok_or(MSG)?;
	let ok = (7..=15).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0');
	if ok { Ok(Value::String(s)) } else { Err(MSG.into()) }
}

/// A Telegram bot's username without the `@`, as `t.me/<bot>` takes it and as Telegram issues it:
/// 5 to 32 of letters, digits and `_`, a letter first, ending in `bot` in any case
/// (`^[A-Za-z][A-Za-z0-9_]{1,28}[Bb][Oo][Tt]$`).
fn telegram_bot(v: &Value) -> Result<Value, String> {
	const MSG: &str = "must be a bot's username without the @: 5 to 32 of letters, digits and _, a letter first, ending in bot";
	let s = text(v, 32).map_err(|_| MSG.to_owned())?;
	let b = s.as_bytes();
	let ok = (5..=32).contains(&b.len()) && b[0].is_ascii_alphabetic() && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_') && b[b.len() - 3..].eq_ignore_ascii_case(b"bot");
	if ok { Ok(Value::String(s)) } else { Err(MSG.into()) }
}

/// `{whatsapp?: bool, telegram?: bool}`: the panel's kill switches for the landing's messenger
/// buttons; a messenger left out is on. Each key is checked on its own, so the editor shows the
/// one that is wrong.
/// The messengers a place may switch off, by their wire names: [`Messenger`]'s, for an error.
fn messenger_names() -> String {
	Messenger::ALL.map(Messenger::as_str).join(", ")
}

fn messengers(v: &Value) -> Result<Value, Bad> {
	let Value::Object(m) = v else {
		return Err(vec![(String::new(), "must be an object like {\"whatsapp\": true, \"telegram\": false}".into())]);
	};
	let mut bad = Bad::new();
	for (k, on) in m {
		if Messenger::parse(k).is_err() {
			bad.push((format!(".{k}"), format!("is not a messenger; one of {}", messenger_names())));
		} else if !on.is_boolean() {
			bad.push((format!(".{k}"), "must be true or false".into()));
		}
	}
	if bad.is_empty() { Ok(v.clone()) } else { Err(bad) }
}

/// `whatsapp=on,telegram=off` → the `messengers` object (`panel place set --messengers`): each
/// messenger once, `on` or `off`. Checked again by [`PlaceSettings::parse`].
pub fn messengers_from_spec(spec: &str) -> Result<Value, String> {
	let mut out = Map::new();
	for part in spec.split(',').map(str::trim) {
		let bad = || format!("{part:?} is not like whatsapp=on or telegram=off");
		let (name, state) = part.split_once('=').ok_or_else(bad)?;
		let (name, state) = (name.trim(), state.trim());
		if Messenger::parse(name).is_err() {
			return Err(format!("{name:?} is not a messenger; one of {}", messenger_names()));
		}
		let on = match state {
			"on" => true,
			"off" => false,
			_ => return Err(bad()),
		};
		if out.insert(name.to_owned(), Value::Bool(on)).is_some() {
			return Err(format!("{name} is named twice"));
		}
	}
	Ok(Value::Object(out))
}

/// An object with exactly `keys`, each checked by `check`; the first problem, named.
fn object<'a>(v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, String> {
	let Value::Object(m) = v else { return Err("must be an object".into()) };
	if let Some(k) = m.keys().find(|k| !keys.contains(&k.as_str())) {
		return Err(format!("{k} is not one of {}", keys.join(", ")));
	}
	if let Some(k) = keys.iter().find(|k| !m.contains_key(**k)) {
		return Err(format!("{k} is required"));
	}
	Ok(m)
}

/// `HH:MM`, 00:00 to 23:59.
fn is_time(s: &str) -> bool {
	let b = s.as_bytes();
	b.len() == 5 && b[2] == b':' && b.iter().enumerate().all(|(i, c)| i == 2 || c.is_ascii_digit()) && matches!(b[0], b'0'..=b'2') && (b[0] != b'2' || b[1] <= b'3') && b[3] <= b'5'
}

/// What was wrong inside a list field: `(path below the field, why)`, every one found.
type Bad = Vec<(String, String)>;

/// `[{days: [Monday, …], opens: "08:00", closes: "19:00"}, …]`: at least one row (an empty
/// list is what kitstart drops); a day once per row; a row closes after it opens (a night
/// across midnight is two rows). Times are the place's local time, Europe/Paris for every
/// place today.
fn hours(v: &Value) -> Result<Value, Bad> {
	let whole = |why: String| vec![(String::new(), why)];
	let Value::Array(rows) = v else {
		return Err(whole("must be a list of {days, opens, closes}".into()));
	};
	if rows.is_empty() {
		return Err(whole("must have a row at least; leave it out to keep the site's own hours".into()));
	}
	if rows.len() > MAX_HOURS {
		return Err(whole(format!("must have at most {MAX_HOURS} rows")));
	}
	let mut out = Vec::with_capacity(rows.len());
	let mut bad = Bad::new();
	for (i, row) in rows.iter().enumerate() {
		let mut row_bad = |part: &str, why: String| bad.push((format!("[{i}]{part}"), why));
		let m = match object(row, &["days", "opens", "closes"]) {
			Ok(m) => m,
			Err(why) => {
				row_bad("", why);
				continue;
			}
		};
		let days = row_days(&m["days"]).map_err(|why| row_bad(".days", why));
		let time = |k: &str| match m[k].as_str() {
			Some(t) if is_time(t) => Ok(t.to_owned()),
			_ => Err("must be HH:MM, 00:00 to 23:59".to_owned()),
		};
		let opens = time("opens").map_err(|why| row_bad(".opens", why));
		let closes = time("closes").map_err(|why| row_bad(".closes", why));
		let (Ok(days), Ok(opens), Ok(closes)) = (days, opens, closes) else { continue };
		// HH:MM sorts as the time it names.
		if closes <= opens {
			row_bad(".closes", "must be after opens; split a night across midnight into two rows".into());
			continue;
		}
		out.push(serde_json::json!({ "days": days, "opens": opens, "closes": closes }));
	}
	if bad.is_empty() { Ok(Value::Array(out)) } else { Err(bad) }
}

/// A row's days: one at least, each once, put in week order.
fn row_days(v: &Value) -> Result<Vec<Value>, String> {
	let Value::Array(days) = v else { return Err("must be a list of day names".into()) };
	if days.is_empty() {
		return Err("must name a day at least".into());
	}
	let mut seen = [false; 7];
	for d in days {
		let Some(n) = d.as_str().and_then(|d| DAYS.iter().position(|day| *day == d)) else {
			return Err(format!("are among {}", DAYS.join(", ")));
		};
		if std::mem::replace(&mut seen[n], true) {
			return Err(format!("name {} twice", DAYS[n]));
		}
	}
	Ok(DAYS.iter().zip(seen).filter(|(_, s)| *s).map(|(d, _)| Value::from(*d)).collect())
}

/// Commune names, at least one, each once.
fn service_area(v: &Value) -> Result<Value, Bad> {
	let whole = |why: String| vec![(String::new(), why)];
	let Value::Array(names) = v else {
		return Err(whole("must be a list of commune names".into()));
	};
	if names.is_empty() {
		return Err(whole("must name a commune at least; leave it out to keep the site's own".into()));
	}
	if names.len() > MAX_AREA {
		return Err(whole(format!("must have at most {MAX_AREA} names")));
	}
	let mut out: Vec<String> = Vec::with_capacity(names.len());
	let mut bad = Bad::new();
	for (i, n) in names.iter().enumerate() {
		match text(n, MAX_TEXT) {
			Ok(name) if out.iter().any(|o| o.to_lowercase() == name.to_lowercase()) => bad.push((format!("[{i}]"), format!("{name} is named twice"))),
			Ok(name) => out.push(name),
			Err(why) => bad.push((format!("[{i}]"), why)),
		}
	}
	if bad.is_empty() { Ok(Value::from(out)) } else { Err(bad) }
}

/// `{street, postalCode, locality}`, a French postal code of five digits.
fn address(v: &Value) -> Result<Value, String> {
	let m = object(v, &["street", "postalCode", "locality"])?;
	let street = text(&m["street"], MAX_TEXT).map_err(|e| format!("street {e}"))?;
	let locality = text(&m["locality"], MAX_TEXT).map_err(|e| format!("locality {e}"))?;
	let postal = text(&m["postalCode"], 5).ok().filter(|p| p.len() == 5 && p.bytes().all(|b| b.is_ascii_digit()));
	let postal = postal.ok_or("postalCode must be five digits")?;
	Ok(serde_json::json!({ "street": street, "postalCode": postal, "locality": locality }))
}

/// `{lat, lng}` in degrees, on the globe.
fn geo(v: &Value) -> Result<Value, String> {
	let m = object(v, &["lat", "lng"])?;
	let coord = |k: &str, limit: f64| match m[k].as_f64() {
		Some(x) if x.is_finite() && x.abs() <= limit => Ok(x),
		_ => Err(format!("{k} must be a number from -{limit} to {limit}")),
	};
	Ok(serde_json::json!({ "lat": coord("lat", 90.0)?, "lng": coord("lng", 180.0)? }))
}

/// An absolute `https://` URL with a host.
fn https_url(v: &Value) -> Result<Value, String> {
	const MSG: &str = "must be an https:// URL";
	let s = text(v, MAX_URL).map_err(|_| MSG.to_owned())?;
	let rest = s.strip_prefix("https://").ok_or(MSG)?;
	let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
	let ok = !host.is_empty() && !host.starts_with([':', '@', '.']) && !s.contains(char::is_whitespace);
	if ok { Ok(Value::String(s)) } else { Err(MSG.into()) }
}

/// `{fr: "…", en: "…"}`: a text per locale (two lowercase letters). kitstart keeps it only
/// when every locale the site speaks is there; which those are, the panel does not know.
fn landmark(v: &Value) -> Result<Value, String> {
	let Value::Object(m) = v else {
		return Err("must be an object of a text per locale, e.g. {\"fr\": \"…\", \"en\": \"…\"}".into());
	};
	if m.is_empty() || m.len() > MAX_LOCALES {
		return Err(format!("must give a text for 1 to {MAX_LOCALES} locales"));
	}
	let mut out = Map::new();
	for (locale, t) in m {
		if !(locale.len() == 2 && locale.bytes().all(|b| b.is_ascii_lowercase())) {
			return Err(format!("{locale} is not a locale like fr or en"));
		}
		out.insert(locale.clone(), Value::String(text(t, MAX_TEXT).map_err(|e| format!("{locale} {e}"))?));
	}
	Ok(Value::Object(out))
}

/// `{value: 1–5, count: a whole number, fetchedAt: an instant}`.
fn rating(v: &Value) -> Result<Value, String> {
	let m = object(v, &["value", "count", "fetchedAt"])?;
	let value = m["value"].as_f64().filter(|x| (1.0..=5.0).contains(x)).ok_or("value must be a number from 1 to 5")?;
	let count = m["count"].as_u64().ok_or("count must be a whole number, 0 or more")?;
	let at: Timestamp = m["fetchedAt"]
		.as_str()
		.and_then(|s| s.parse().ok())
		.ok_or("fetchedAt must be an RFC 3339 instant, e.g. 2026-10-03T12:00:00Z")?;
	Ok(serde_json::json!({ "value": value, "count": count, "fetchedAt": at.to_string() }))
}

/// `Mo-Fr 08:00-19:00,Sa 09:00-12:00` → the `hours` list: rows split by `,`, each a day or a
/// range of days (`Mo`…`Su`, week order) and a span. Checked again by [`PlaceSettings::parse`].
pub fn hours_from_spec(spec: &str) -> Result<Value, String> {
	let bad = |row: &str, why: &str| format!("{row:?}: {why}; rows look like 'Mo-Fr 08:00-19:00,Sa 09:00-12:00'");
	let mut rows = Vec::new();
	for row in spec.split(',').map(str::trim) {
		let (days, span) = row.split_once(char::is_whitespace).ok_or_else(|| bad(row, "a day or days, a space, a span"))?;
		let day = |d: &str| SHORT_DAYS.iter().position(|s| *s == d).ok_or_else(|| bad(row, "days are Mo Tu We Th Fr Sa Su"));
		let (first, last) = match days.split_once('-') {
			Some((a, b)) => (day(a)?, day(b)?),
			None => (day(days)?, day(days)?),
		};
		if last < first {
			return Err(bad(row, "a range of days runs in week order"));
		}
		let (opens, closes) = span.trim().split_once('-').ok_or_else(|| bad(row, "a span is HH:MM-HH:MM"))?;
		rows.push(serde_json::json!({ "days": DAYS[first..=last], "opens": opens, "closes": closes }));
	}
	Ok(Value::Array(rows))
}

/// What a change did to a place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
	/// Registered by hand (it may not have had a lead yet).
	Register,
	/// Its settings replaced.
	Set,
	/// The settings an earlier change found, put back.
	Revert,
	/// Taken off the sites: they answer it as gone.
	Withdraw,
	/// Back on the sites.
	Restore,
}

impl ChangeKind {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Register => "register",
			Self::Set => "set",
			Self::Revert => "revert",
			Self::Withdraw => "withdraw",
			Self::Restore => "restore",
		}
	}

	pub fn parse(raw: &str) -> Option<Self> {
		[Self::Register, Self::Set, Self::Revert, Self::Withdraw, Self::Restore].into_iter().find(|k| k.as_str() == raw)
	}
}

/// Who changed a place: a signed-in user (concierge id, and the email the history shows), or
/// the CLI on the pod.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Editor {
	User { id: Uuid, label: String },
	Cli,
}

impl Editor {
	/// What the history shows as `by`.
	pub fn label(&self) -> &str {
		match self {
			Self::User { label, .. } => label,
			Self::Cli => "cli",
		}
	}

	pub fn user_id(&self) -> Option<Uuid> {
		match self {
			Self::User { id, .. } => Some(*id),
			Self::Cli => None,
		}
	}
}

/// One entry of a place's history: what its settings were before and after, who, when.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
	pub id: Uuid,
	pub at: Timestamp,
	pub kind: ChangeKind,
	/// [`Editor::label`].
	pub by: String,
	pub before: PlaceSettings,
	pub after: PlaceSettings,
	/// The change a [`ChangeKind::Revert`] went back before.
	pub reverts: Option<Uuid>,
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn err(body: Value) -> FieldErrors {
		PlaceSettings::parse(&body).unwrap_err()
	}

	#[test]
	fn the_contracts_example_round_trips() {
		let body = json!({
			"phone": "+33423500640", "whatsapp": "+33612345678",
			"hours": [{ "days": ["Monday", "Tuesday"], "opens": "08:00", "closes": "19:00" }],
			"serviceArea": ["Royat", "Chamalières"],
			"address": { "street": "1 rue X", "postalCode": "63130", "locality": "Royat" },
			"geo": { "lat": 45.76, "lng": 3.05 },
			"storefrontPhoto": "https://example.com/front.jpg",
			"landmark": { "fr": "Face à la mairie", "en": "Opposite the town hall" },
			"rating": { "value": 4.8, "count": 37, "fetchedAt": "2026-10-03T12:00:00Z" },
		});
		assert_eq!(PlaceSettings::parse(&body).unwrap().as_json(), body);
		assert!(PlaceSettings::parse(&json!({})).unwrap().is_empty());
		assert!(PlaceSettings::parse(&json!({"phone": null})).unwrap().is_empty(), "null is left out");
	}

	#[test]
	fn text_is_trimmed_and_days_put_in_week_order() {
		let s = PlaceSettings::parse(&json!({
			"serviceArea": [" Royat "],
			"hours": [{ "days": ["Friday", "Monday"], "opens": "08:00", "closes": "19:00" }],
		}))
		.unwrap();
		assert_eq!(s.get("serviceArea"), Some(&json!(["Royat"])));
		assert_eq!(s.get("hours").unwrap()[0]["days"], json!(["Monday", "Friday"]));
	}

	#[test]
	fn what_kitstart_would_drop_is_refused_by_name() {
		assert_eq!(err(json!({"phone": "0612345678"}))["phone"], "must be E.164, e.g. +33612345678");
		for bad in ["+33 6 12 34 56 78", "+0612345678", "+123", "+1234567890123456", "+33612345678x"] {
			assert!(err(json!({ "whatsapp": bad })).contains_key("whatsapp"), "{bad}");
		}
		let e = err(json!({"fax": "+33612345678", "geo": {"lat": 91, "lng": 0}, "storefrontPhoto": "http://x.fr/a.jpg"}));
		assert_eq!(e.keys().collect::<Vec<_>>(), ["fax", "geo", "storefrontPhoto"], "every bad field, not the first");
		assert!(err(json!([])).contains_key("settings"));
	}

	#[test]
	fn hours() {
		let row = |days: Value, opens: &str, closes: &str| json!({ "hours": [{ "days": days, "opens": opens, "closes": closes }] });
		assert!(PlaceSettings::parse(&row(json!(["Sunday"]), "00:00", "23:59")).is_ok());
		for (body, why) in [
			(json!({"hours": []}), "an empty list"),
			(row(json!([]), "08:00", "19:00"), "no day"),
			(row(json!(["Mon"]), "08:00", "19:00"), "not a day"),
			(row(json!(["Monday", "Monday"]), "08:00", "19:00"), "a day twice"),
			(row(json!(["Monday"]), "8:00", "19:00"), "H:MM"),
			(row(json!(["Monday"]), "08:00", "24:00"), "24:00"),
			(row(json!(["Monday"]), "08:60", "19:00"), "minute 60"),
			(row(json!(["Monday"]), "19:00", "08:00"), "across midnight"),
			(json!({"hours": [{"days": ["Monday"], "opens": "08:00"}]}), "no closes"),
			(json!({"hours": [{"days": ["Monday"], "opens": "08:00", "closes": "19:00", "note": "x"}]}), "a stray key"),
		] {
			assert!(err(body).keys().all(|k| k.starts_with("hours")), "{why}");
		}
	}

	#[test]
	fn a_list_names_the_part_that_is_wrong() {
		let e = err(json!({
			"hours": [
				{"days": ["Monday"], "opens": "08:00", "closes": "19:00"},
				{"days": ["Tuesday", "Tuesday"], "opens": "8h", "closes": "19:00"},
				{"days": ["Friday"], "opens": "19:00", "closes": "08:00"},
			],
			"serviceArea": ["Royat", "", "royat"],
		}));
		let keys: Vec<&str> = e.keys().map(String::as_str).collect();
		assert_eq!(keys, ["hours[1].days", "hours[1].opens", "hours[2].closes", "serviceArea[1]", "serviceArea[2]"]);
		assert_eq!(err(json!({"hours": "Mo-Fr"})).keys().collect::<Vec<_>>(), ["hours"]);
	}

	#[test]
	fn the_other_fields() {
		for body in [
			json!({"serviceArea": []}),
			json!({"serviceArea": ["Royat", "royat"]}),
			json!({"serviceArea": [""]}),
			json!({"address": {"street": "1 rue X", "postalCode": "6313", "locality": "Royat"}}),
			json!({"address": {"street": "1 rue X", "locality": "Royat"}}),
			json!({"geo": {"lat": 45.0}}),
			json!({"storefrontPhoto": "https://"}),
			json!({"storefrontPhoto": "https://a b.fr/x"}),
			json!({"landmark": {}}),
			json!({"landmark": {"french": "x"}}),
			json!({"landmark": {"fr": " "}}),
			json!({"rating": {"value": 0.5, "count": 1, "fetchedAt": "2026-10-03T12:00:00Z"}}),
			json!({"rating": {"value": 4, "count": -1, "fetchedAt": "2026-10-03T12:00:00Z"}}),
			json!({"rating": {"value": 4, "count": 1, "fetchedAt": "yesterday"}}),
		] {
			assert_eq!(err(body.clone()).len(), 1, "{body}");
		}
	}

	#[test]
	fn booking_is_a_setting_named_by_its_path() {
		let booking = json!({"default": "google_calendar", "providers": {"google_calendar": {"url": "https://calendar.app.google/x1"}}});
		assert_eq!(PlaceSettings::parse(&json!({ "booking": booking })).unwrap().get("booking"), Some(&booking));
		let e = err(json!({"booking": {"default": "link", "providers": {"google_calendar": {"url": "https://calendar.google.com/x"}}}}));
		assert_eq!(e.keys().collect::<Vec<_>>(), ["booking.default", "booking.providers.google_calendar.url"]);
	}

	#[test]
	fn the_clis_patch_and_hours() {
		let current = PlaceSettings::parse(&json!({"phone": "+33423500640", "serviceArea": ["Royat"]})).unwrap();
		let hours = hours_from_spec("Mo-Fr 08:00-19:00, Sa 09:00-12:00").unwrap();
		assert_eq!(
			hours,
			json!([
				{"days": ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"], "opens": "08:00", "closes": "19:00"},
				{"days": ["Saturday"], "opens": "09:00", "closes": "12:00"},
			])
		);
		let set = Map::from_iter([("hours".to_owned(), hours)]);
		let next = current.patched(set, &["serviceArea".to_owned()]).unwrap();
		assert_eq!(
			next.as_json(),
			json!({"phone": "+33423500640", "hours": hours_from_spec("Mo-Fr 08:00-19:00,Sa 09:00-12:00").unwrap()})
		);
		assert!(current.patched(Map::new(), &["fax".to_owned()]).unwrap_err().contains_key("fax"));
		for bad in ["Mo-Fr", "Fr-Mo 08:00-19:00", "Mon 08:00-19:00", "Mo 08:00"] {
			assert!(hours_from_spec(bad).is_err(), "{bad}");
		}
		assert!(current.patched(Map::from_iter([("hours".to_owned(), hours_from_spec("Mo 19:00-08:00").unwrap())]), &[]).is_err());
	}

	#[test]
	fn the_messengers_a_place_offers() {
		let s = PlaceSettings::parse(&json!({"telegram": " aquafix_devis_bot ", "messengers": {"whatsapp": true, "telegram": false}})).unwrap();
		assert_eq!(s.get("telegram"), Some(&json!("aquafix_devis_bot")), "trimmed");
		assert_eq!(s.get("messengers"), Some(&json!({"whatsapp": true, "telegram": false})));
		assert_eq!(
			PlaceSettings::parse(&json!({"messengers": {}})).unwrap().get("messengers"),
			Some(&json!({})),
			"every messenger on"
		);
		for good in ["a_bot", "AquafixBOT", "x1Bot", &format!("a{}bot", "b".repeat(28)) as &str] {
			assert!(PlaceSettings::parse(&json!({ "telegram": good })).is_ok(), "{good}");
		}
		for bad in [
			"abot",
			"_xbot",
			"aquafix",
			"aquafix_bo",
			"@aquafix_bot",
			"1aquafix_bot",
			"aqua-fix_bot",
			"aqua fix bot",
			&format!("a{}bot", "b".repeat(29)) as &str,
		] {
			assert_eq!(
				err(json!({ "telegram": bad }))["telegram"],
				"must be a bot's username without the @: 5 to 32 of letters, digits and _, a letter first, ending in bot",
				"{bad}"
			);
		}
		let e = err(json!({"messengers": {"whatsapp": "on", "signal": true, "telegram": false}}));
		assert_eq!(e.keys().collect::<Vec<_>>(), ["messengers.signal", "messengers.whatsapp"]);
		assert_eq!(e["messengers.signal"], "is not a messenger; one of whatsapp, telegram");
		assert_eq!(err(json!({"messengers": [true]})).keys().collect::<Vec<_>>(), ["messengers"]);

		assert_eq!(messengers_from_spec("whatsapp=on, telegram=off").unwrap(), json!({"whatsapp": true, "telegram": false}));
		for bad in ["whatsapp", "whatsapp=yes", "sms=on", "whatsapp=on,whatsapp=off", ""] {
			assert!(messengers_from_spec(bad).is_err(), "{bad:?}");
		}
		let current = PlaceSettings::parse(&json!({"telegram": "aquafix_devis_bot", "messengers": {"telegram": false}})).unwrap();
		let next = current.patched(Map::new(), &["telegram".to_owned(), "messengers".to_owned()]).unwrap();
		assert!(next.is_empty(), "both are fields the CLI clears");
	}

	#[test]
	fn change_kinds_round_trip() {
		for k in [ChangeKind::Register, ChangeKind::Set, ChangeKind::Revert, ChangeKind::Withdraw, ChangeKind::Restore] {
			assert_eq!(ChangeKind::parse(k.as_str()), Some(k));
		}
		assert_eq!(ChangeKind::parse("delete"), None);
	}
}

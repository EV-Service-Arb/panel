//! The Places API keys, in rotation order: a key sits out while Google says it is spent, and the
//! next one is asked. Free keys go first, so billed ones pay only for what the free ones could not.

use std::{
	sync::Mutex,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use zeroize::Zeroizing;

/// Why a key is sitting out. Only a refusal says so: an API key cannot read its quota ahead.
#[derive(Clone, Debug, PartialEq)]
pub enum Spent {
	/// A quota of its project's, as the refusal spelled it.
	Quota {
		/// The quota's name, `SearchTextRequestPerDayPerProject` and the like.
		limit: String,
		/// Unix seconds its window rolls over at.
		reset: u64,
	},
	/// Its project's billing is off, which is what an ended trial looks like: back only at
	/// restart, after a human has fixed it.
	Billing,
}

impl std::fmt::Display for Spent {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Quota { limit, reset } => write!(f, "its project's {limit} is used, until {:02}:{:02} UTC", reset % 86_400 / 3600, reset % 3600 / 60),
			Self::Billing => write!(f, "its project's billing is disabled"),
		}
	}
}

impl Spent {
	/// What a refusal says about the key, if it says the key is done for a window. Anything else —
	/// a malformed query, a key with the wrong referrer — is `None`: rotating on it would spend
	/// the next project's quota on the same mistake.
	pub(crate) fn of(body: &str) -> Option<Self> {
		let v = serde_json::from_str::<serde_json::Value>(body).ok()?; // not JSON: no reason given, the refusal travels as is
		let details = v["error"]["details"].as_array()?;
		if details.iter().any(|d| d["reason"].as_str() == Some("BILLING_DISABLED")) {
			return Some(Self::Billing);
		}
		if v["error"]["status"].as_str() != Some("RESOURCE_EXHAUSTED") {
			return None;
		}
		details.iter().map(|d| &d["metadata"]).find_map(|m| {
			let limit = m["quota_limit"].as_str()?;
			let start: u64 = m["window_start_time"].as_str()?.parse().ok()?;
			let window = if limit.contains("PerMinute") { 60 } else { 86_400 };
			Some(Self::Quota {
				limit: limit.to_owned(),
				reset: start + window,
			})
		})
	}

	fn back_by(&self, now: SystemTime) -> bool {
		match self {
			Self::Quota { reset, .. } => now >= UNIX_EPOCH + Duration::from_secs(*reset),
			Self::Billing => false,
		}
	}
}

/// The keys and which of them sit out.
pub struct Keys(Mutex<Vec<(Zeroizing<String>, Option<Spent>)>>);

impl std::fmt::Debug for Keys {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let keys = self.0.lock().expect("never poisoned: no panic while held");
		f.debug_list().entries(keys.iter().map(|(_, out)| out)).finish()
	}
}

impl Keys {
	/// `keys` in the order they are used in.
	pub fn new(keys: Vec<String>) -> Self {
		for (i, k) in keys.iter().enumerate() {
			assert!(!keys[..i].contains(k), "a Places key is listed twice");
		}
		Self(Mutex::new(keys.into_iter().map(|k| (Zeroizing::new(k), None)).collect()))
	}

	pub(crate) fn is_empty(&self) -> bool {
		self.0.lock().expect("never poisoned: no panic while held").is_empty()
	}

	/// The first key not sitting out and not `tried` (by this request: a window Google says is
	/// over already must not loop it), else why each sits out.
	pub(crate) fn next(&self, tried: &[Zeroizing<String>]) -> Result<Zeroizing<String>, Vec<Spent>> {
		let mut keys = self.0.lock().expect("never poisoned: no panic while held");
		let now = SystemTime::now();
		for (key, out) in keys.iter_mut().filter(|(k, _)| !tried.contains(k)) {
			if out.as_ref().is_some_and(|s| s.back_by(now)) {
				*out = None;
			}
			if out.is_none() {
				return Ok(key.clone());
			}
		}
		Err(keys.iter().filter_map(|(_, out)| out.clone()).collect())
	}

	pub(crate) fn sit_out(&self, key: &str, why: Spent) {
		let mut keys = self.0.lock().expect("never poisoned: no panic while held");
		let (_, out) = keys.iter_mut().find(|(k, _)| k.as_str() == key).expect("handed out by `next`");
		let tail = &key[key.len().saturating_sub(4)..];
		tracing::warn!(key = %format!("…{tail}"), "Places key spent: {why}");
		*out = Some(why);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The body a spent window actually returns, trimmed to the detail it is read for.
	const REFUSAL: &str = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","details":[
		{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"RATE_LIMIT_EXCEEDED","metadata":{
			"window_start_time":"1789369200","quota_limit_value":"100",
			"quota_metric":"places.googleapis.com/SearchTextRequest",
			"quota_limit":"SearchTextRequestPerDayPerProject"}}]}}"#;

	/// What an ended trial returns.
	const NO_BILLING: &str = r#"{"error":{"code":403,"status":"PERMISSION_DENIED","details":[
		{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"BILLING_DISABLED","domain":"googleapis.com",
			"metadata":{"consumer":"projects/1","service":"places.googleapis.com"}}]}}"#;

	#[test]
	fn a_refusal_is_read_for_what_it_says() {
		assert_eq!(
			Spent::of(REFUSAL),
			Some(Spent::Quota {
				limit: "SearchTextRequestPerDayPerProject".into(),
				reset: 1789369200 + 86_400
			})
		);
		assert_eq!(Spent::of(NO_BILLING), Some(Spent::Billing));
		assert_eq!(
			Spent::of(&REFUSAL.replace("SearchTextRequestPerDayPerProject", "GetPlaceRequestPerMinutePerProject")).map(|s| s.to_string().contains("GetPlace")),
			Some(true)
		);
		assert!(Spent::of(r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#).is_none());
		assert!(Spent::of("not json at all").is_none());
		assert!(Spent::of(r#"{"error":{}}"#).is_none());
		assert!(Spent::of(r#"{"error":{"status":"RESOURCE_EXHAUSTED","details":[{"metadata":{"quota_limit":"PlacesPerMinute"}}]}}"#).is_none());
	}
}

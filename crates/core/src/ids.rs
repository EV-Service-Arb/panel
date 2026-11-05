//! A brand a source key writes for, the shape of a key id, and a site key's origin.

use std::fmt;

use crate::Invalid;

/// A brand: aquafix, vifnet, … Lowercase slug, 1–64 of `[a-z0-9_-]`, starting alphanumeric.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BrandId(String);

impl BrandId {
	pub fn parse(raw: &str) -> Result<Self, Invalid> {
		if is_slug(raw) {
			Ok(Self(raw.to_owned()))
		} else {
			Err(Invalid::new("brand is not a valid id"))
		}
	}

	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl fmt::Display for BrandId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.0)
	}
}

/// Where a site key's landing answers: `https://<domain>[:port]`, or plain http on loopback (a
/// landing on this machine), spelled exactly as the URL's own origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiteOrigin(String);

impl SiteOrigin {
	pub fn parse(raw: &str) -> Result<Self, Invalid> {
		let invalid = || Invalid::new(format!("{raw:?} is not a site origin: https://<domain>[:port], nothing after it"));
		let url = url::Url::parse(raw).map_err(|_| invalid())?;
		let shaped = matches!(
			(url.scheme(), url.host()),
			("https", Some(url::Host::Domain(_))) | ("http", Some(url::Host::Domain("localhost") | url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)))
		);
		if shaped && url.origin().ascii_serialization() == raw {
			Ok(Self(raw.to_owned()))
		} else {
			Err(invalid())
		}
	}

	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl fmt::Display for SiteOrigin {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.0)
	}
}

/// 1–64 of `[a-z0-9_-]`, starting alphanumeric.
pub fn is_slug(s: &str) -> bool {
	(1..=64).contains(&s.len())
		&& s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
		&& s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slugs() {
		assert!(BrandId::parse("aquafix").is_ok());
		for bad in ["", "Aquafix", "-x", "a b", &"a".repeat(65)] {
			assert!(BrandId::parse(bad).is_err(), "{bad:?}");
		}
	}

	#[test]
	fn site_origins() {
		for good in ["https://aquafix.top", "https://vifnet.site:8443", "http://localhost:59081", "http://127.0.0.1:9"] {
			assert_eq!(SiteOrigin::parse(good).map(|o| o.to_string()), Ok(good.to_owned()));
		}
		for bad in [
			"https://aquafix.top/",
			"https://aquafix.top/fr",
			"https://aquafix.top?x",
			"https://Aquafix.top",
			"https://aquafix.top:443",
			"https://u@aquafix.top",
			"https://1.2.3.4",
			"http://aquafix.top",
			"ftp://aquafix.top",
			"aquafix.top",
		] {
			assert!(SiteOrigin::parse(bad).is_err(), "{bad:?}");
		}
	}
}

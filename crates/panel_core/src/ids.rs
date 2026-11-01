//! A brand a source key writes for, and the shape of a key id.

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
}

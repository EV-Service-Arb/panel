//! The panel's cookies. `__Host-` names and `Secure` behind https (the prefix pins a cookie
//! to this host and path `/`, so no sibling subdomain can plant one); bare names over plain
//! http, where a browser would refuse either — local development only.

use axum::http::{HeaderMap, HeaderValue, header};
use subtle::ConstantTimeEq;

/// The pre-login: state and PKCE verifier, sealed, for the ten minutes of a sign-in.
pub const PRELOGIN: &str = "sa_prelogin";
/// The session id. HttpOnly: no script ever reads it.
pub const SESSION: &str = "sa_session";
/// The double-submit CSRF token: readable by the page, which echoes it in [`CSRF_HEADER`].
pub const CSRF: &str = "sa_csrf";
pub const CSRF_HEADER: &str = "x-sa-csrf";

#[derive(Clone, Debug)]
pub struct Cookies {
	secure: bool,
}

impl Cookies {
	pub fn new(secure: bool) -> Self {
		Self { secure }
	}

	pub fn name(&self, base: &str) -> String {
		if self.secure { format!("__Host-{base}") } else { base.to_owned() }
	}

	/// A cookie's value from the request; the first one of that name.
	pub fn get<'a>(&self, headers: &'a HeaderMap, base: &str) -> Option<&'a str> {
		let name = self.name(base);
		headers
			.get_all(header::COOKIE)
			.iter()
			.filter_map(|v| v.to_str().ok())
			.flat_map(|v| v.split(';'))
			.filter_map(|pair| pair.trim().split_once('='))
			.find(|(k, _)| *k == name)
			.map(|(_, v)| v)
			.filter(|v| !v.is_empty())
	}

	/// `Set-Cookie` for a value living `max_age` seconds. Every cookie here is `Path=/` and
	/// `SameSite=Lax`: the sign-in comes back by a top-level navigation from concierge,
	/// which `Strict` would strip them from.
	pub fn set(&self, base: &str, value: &str, max_age: i64, http_only: bool) -> HeaderValue {
		let mut c = format!("{}={value}; Path=/; Max-Age={}; SameSite=Lax", self.name(base), max_age.max(0));
		if http_only {
			c.push_str("; HttpOnly");
		}
		if self.secure {
			c.push_str("; Secure");
		}
		// Every value here is hex or base64url, so the header is always valid.
		HeaderValue::from_str(&c).unwrap_or_else(|_| HeaderValue::from_static(""))
	}

	pub fn clear(&self, base: &str) -> HeaderValue {
		self.set(base, "", 0, true)
	}

	/// The double-submit check: the header equals the cookie, compared in constant time.
	pub fn csrf_ok(&self, headers: &HeaderMap) -> bool {
		let cookie = self.get(headers, CSRF);
		let header = headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok());
		matches!((cookie, header), (Some(c), Some(h)) if c.len() == h.len() && bool::from(c.as_bytes().ct_eq(h.as_bytes())))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn headers(cookie: &str, csrf: Option<&str>) -> HeaderMap {
		let mut h = HeaderMap::new();
		h.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
		if let Some(csrf) = csrf {
			h.insert(CSRF_HEADER, HeaderValue::from_str(csrf).unwrap());
		}
		h
	}

	#[test]
	fn names_values_and_attributes() {
		let https = Cookies::new(true);
		let h = headers("a=1; __Host-sa_session=abc; sa_session=evil", None);
		assert_eq!(https.get(&h, SESSION), Some("abc"), "only the __Host- one counts behind https");
		assert_eq!(
			https.set(SESSION, "abc", 60, true).to_str().unwrap(),
			"__Host-sa_session=abc; Path=/; Max-Age=60; SameSite=Lax; HttpOnly; Secure"
		);
		assert_eq!(https.set(CSRF, "t", 60, false).to_str().unwrap(), "__Host-sa_csrf=t; Path=/; Max-Age=60; SameSite=Lax; Secure");
		assert_eq!(
			Cookies::new(false).set(PRELOGIN, "p", 600, true).to_str().unwrap(),
			"sa_prelogin=p; Path=/; Max-Age=600; SameSite=Lax; HttpOnly"
		);
	}

	#[test]
	fn csrf_double_submit() {
		let c = Cookies::new(true);
		assert!(c.csrf_ok(&headers("__Host-sa_csrf=tok", Some("tok"))));
		assert!(!c.csrf_ok(&headers("__Host-sa_csrf=tok", Some("tok2"))));
		assert!(!c.csrf_ok(&headers("__Host-sa_csrf=tok", None)));
		assert!(!c.csrf_ok(&headers("x=1", Some("tok"))));
		assert!(!c.csrf_ok(&headers("__Host-sa_csrf=", Some(""))), "empty is not a token");
	}
}

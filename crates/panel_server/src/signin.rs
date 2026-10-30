//! Signing in through concierge (spec §4), and the gate every `/api/v1` request passes.
//!
//! ```text
//! GET /auth/login      ?return_to=<path on this origin>, else 400; it, the state and the
//!                      PKCE verifier sealed into the pre-login cookie
//!                      → 302 concierge /api/auth/authorize?client_id=sa&…&code_challenge
//!                      [&prompt=select_account, as asked]
//! GET /auth/callback   state from the cookie = state in the URL (constant time), and the
//!                      state not redeemed before, else 400 and the code is never presented;
//!                      ExchangeCode(code, verifier); the browser's previous session closed
//!                      → server-side session, cookies → 303 return_to, else /
//! /api/v1/*            CSRF on anything but GET; session (access token rotated when it is
//!                      about to expire); GetMe (≤ 60 s cache; fresh for minting or revoking
//!                      source keys) → the caller's permissions, else 401/503
//! POST /auth/logout    CSRF; every session of the user is closed
//! ```
//!
//! Signing out of evinvest.ltd revokes the concierge token family: the panel's session ends
//! at its next rotation, within the access token's lifetime. A permission revoked at concierge
//! is seen within [`ME_TTL`].
//!
//! Every signed-in user passes the gate; what they may open is the routers' to say
//! ([`crate::http`]).
//!
//! The browser never holds a concierge token: only a random session id, whose hash names a
//! row holding the tokens sealed.
//!
//! Under `PANEL_DEV_SIGN_IN` (development, loopback only) there is no concierge:
//! `/auth/login` sends the browser straight to `/auth/callback` with [`DEV_CODE`], and
//! [`Concierge::dev`] answers for one made-up user; everything else above runs unchanged.

use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
	time::{Duration, Instant},
};

use axum::{
	Json,
	extract::{Query, Request, State},
	http::{HeaderMap, HeaderValue, Method, StatusCode, header},
	middleware::Next,
	response::{Html, IntoResponse, Response},
};
use jiff::Timestamp;
use panel::{
	Panel,
	session::{PRELOGIN_TTL, SessionError, SessionKey},
};
use sa_auth::PermissionSet;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
	api::ApiError,
	concierge::{CLIENT_ID, Concierge, ConciergeError, DEV_CODE, Me},
	cookies::{self, Cookies},
	http::Section,
};

/// How long `GetMe` is trusted for a session: a grant revoked at concierge shuts the panel
/// at most this much later (at the next token rotation, concierge refuses outright).
pub const ME_TTL: Duration = Duration::from_secs(60);

/// What signing in needs.
#[derive(Clone, Debug)]
pub struct SignInConfig {
	/// e.g. `https://sa.evinvest.ltd`, no trailing slash.
	pub panel_origin: String,
	/// e.g. `https://evinvest.ltd`.
	pub concierge_origin: String,
}

/// The sign-in's state, shared by the routes and the gate.
#[derive(Clone, Debug)]
pub struct SignIn {
	pub panel: Panel,
	pub concierge: Concierge,
	config: Arc<SignInConfig>,
	cookies: Cookies,
	me: Arc<Mutex<HashMap<SessionKey, (Instant, Me)>>>,
}

impl SignIn {
	pub fn new(panel: Panel, concierge: Concierge, mut config: SignInConfig) -> Self {
		config.panel_origin = config.panel_origin.trim_end_matches('/').to_owned();
		config.concierge_origin = config.concierge_origin.trim_end_matches('/').to_owned();
		let cookies = Cookies::new(config.panel_origin.starts_with("https://"));
		Self {
			panel,
			concierge,
			config: Arc::new(config),
			cookies,
			me: Arc::default(),
		}
	}

	pub fn cookies(&self) -> &Cookies {
		&self.cookies
	}

	/// The panel's own origin, e.g. `https://sa.evinvest.ltd`: the one a live socket's
	/// handshake must come from.
	pub fn panel_origin(&self) -> &str {
		&self.config.panel_origin
	}

	fn redirect_uri(&self) -> String {
		format!("{}/auth/callback", self.config.panel_origin)
	}

	fn cached_me(&self, key: &SessionKey) -> Option<Me> {
		let cache = self.me.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		cache.get(key).filter(|(at, _)| at.elapsed() < ME_TTL).map(|(_, me)| me.clone())
	}

	fn remember_me(&self, key: SessionKey, me: Me) {
		let mut cache = self.me.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		// Bounded without a sweeper: stale entries go whenever the map has grown.
		if cache.len() >= 1024 {
			cache.retain(|_, (at, _)| at.elapsed() < ME_TTL);
		}
		cache.insert(key, (Instant::now(), me));
	}

	/// Forgets every cached `GetMe` of the user: their sessions are all closed.
	fn forget_user(&self, user_id: uuid::Uuid) {
		self.me.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|_, (_, me)| me.user_id != user_id);
	}

	fn forget_me(&self, key: &SessionKey) {
		self.me.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(key);
	}
}

/// The signed-in user of an `/api/v1` request, put in its extensions by the gate; `GET /me`.
#[derive(Clone, Debug, Serialize, TS)]
pub struct Caller {
	pub user_id: Uuid,
	pub email: String,
	#[serde(skip)]
	#[ts(skip)]
	pub email_verified: bool,
	pub preferred_name: String,
	/// Concrete `sa` permissions, as concierge resolved them.
	#[ts(type = "Array<Permission>")]
	pub permissions: PermissionSet,
	/// Signed in by `PANEL_DEV_SIGN_IN`, not concierge: `/me` says so, for the UI to show.
	pub dev_sign_in: bool,
}

fn json_error(status: StatusCode, msg: &str) -> Response {
	(status, Json(json!({ "error": msg }))).into_response()
}

/// What a sign-in page may load and do: nothing but this origin, and never inside a frame.
pub const PAGE_CSP: &str = "default-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// A page on this origin with fixed text: nothing from the request is echoed.
fn page(status: StatusCode, title: &str, message: &str) -> Response {
	let body = format!(
		"<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
		 <title>{title}</title></head><body><main><h1>{title}</h1><p>{message}</p><p><a href=\"/auth/login\">Sign in again</a></p></main></body></html>"
	);
	let mut res = (status, Html(body)).into_response();
	res.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(PAGE_CSP));
	res
}

/// The sign-in's answers carry a code or a token in their URLs: kept out of caches and out
/// of the `Referer` of whatever the page loads next.
fn private(mut res: Response) -> Response {
	let h = res.headers_mut();
	h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
	res
}

/// Percent-encodes a query value: everything but RFC 3986's unreserved characters.
pub(crate) fn encode(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for b in value.bytes() {
		if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
			out.push(char::from(b));
		} else {
			out.push_str(&format!("%{b:02X}"));
		}
	}
	out
}

#[derive(Deserialize)]
pub struct Login {
	return_to: Option<String>,
	prompt: Option<Prompt>,
}

/// OIDC `prompt`, passed on to concierge.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Prompt {
	/// Google's account chooser, past the evinvest.ltd session the browser may already have.
	SelectAccount,
}

/// Longest `return_to`: a path the panel links to, with room for its query.
const MAX_RETURN_TO: usize = 512;

/// Whether `/auth/callback` may send the browser to `path`: a path on this origin, and
/// nothing a browser could read as another (`//host`, `/\host`), nor anything but visible
/// ASCII (no control character, no space).
fn same_origin_path(path: &str) -> bool {
	path.len() <= MAX_RETURN_TO && path.starts_with('/') && !path[1..].starts_with('/') && !path.contains('\\') && path.bytes().all(|b| b.is_ascii_graphic())
}

/// `GET /auth/login`.
pub async fn login(State(s): State<SignIn>, Query(q): Query<Login>) -> Response {
	if q.return_to.as_deref().is_some_and(|p| !same_origin_path(p)) {
		return private(page(StatusCode::BAD_REQUEST, "Sign-in failed", "This sign-in link is not valid here. Start again."));
	}
	let begun = match s.panel.begin_sign_in(q.return_to.as_deref(), Timestamp::now()) {
		Ok(b) => b,
		Err(e) => {
			crate::report(&e, "starting a sign-in");
			return private(page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed", "Something went wrong on our side. Try again."));
		}
	};
	let location = if s.concierge.is_dev() {
		// No concierge to send the browser to: straight back to the callback, which then runs
		// as after concierge — pre-login, state redeemed once, the code exchanged, a session.
		format!("{}?code={DEV_CODE}&state={}", s.redirect_uri(), begun.state)
	} else {
		format!(
			"{}/api/auth/authorize?client_id={CLIENT_ID}&redirect_uri={}&response_type=code&state={}&code_challenge={}&code_challenge_method=S256{}",
			s.config.concierge_origin,
			encode(&s.redirect_uri()),
			begun.state,
			begun.challenge,
			match q.prompt {
				Some(Prompt::SelectAccount) => "&prompt=select_account",
				None => "",
			},
		)
	};
	let Ok(location) = HeaderValue::from_str(&location) else {
		return private(page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed", "The sign-in is misconfigured."));
	};
	let prelogin = s.cookies.set(cookies::PRELOGIN, &begun.cookie, PRELOGIN_TTL.as_secs(), true);
	private((StatusCode::FOUND, [(header::LOCATION, location), (header::SET_COOKIE, prelogin)]).into_response())
}

#[derive(Deserialize)]
pub struct Callback {
	code: Option<String>,
	state: Option<String>,
	error: Option<String>,
}

/// `GET /auth/callback`.
pub async fn callback(State(s): State<SignIn>, headers: HeaderMap, Query(q): Query<Callback>) -> Response {
	// The pre-login is single-use whatever happens next.
	let clear = s.cookies.clear(cookies::PRELOGIN);
	let with_clear = |res: Response| {
		let mut res = private(res);
		res.headers_mut().append(header::SET_COOKIE, clear.clone());
		res
	};
	if let Some(error) = q.error.as_deref() {
		// Fixed pages: concierge's error word picks one, and is not shown.
		return with_clear(match error {
			"access_denied" => page(StatusCode::FORBIDDEN, "No access", "Your account has no access to the Service-Arb panel. Ask its admin for one."),
			"temporarily_unavailable" => page(
				StatusCode::SERVICE_UNAVAILABLE,
				"Sign-in unavailable",
				"Signing in is unavailable right now. Try again in a minute.",
			),
			_ => page(StatusCode::BAD_REQUEST, "Sign-in failed", "The sign-in could not be completed. Try again."),
		});
	}
	let now = Timestamp::now();
	let pre = match (s.cookies.get(&headers, cookies::PRELOGIN), q.state.as_deref()) {
		(Some(cookie), Some(state)) => s.panel.finish_sign_in(cookie, state, now),
		_ => None,
	};
	let (Some(pre), Some(code)) = (pre, q.code.as_deref().filter(|c| !c.is_empty())) else {
		tracing::info!("sign-in callback without a matching pre-login; the code is not presented");
		return with_clear(page(
			StatusCode::BAD_REQUEST,
			"Sign-in failed",
			"This sign-in link is not valid here, or has expired. Start again.",
		));
	};
	// A replayed callback (a leaked URL with the pre-login cookie still in the jar) stops
	// here: a state is redeemed once, across replicas.
	match s.panel.consume_state(&pre.state, now).await {
		Ok(true) => {}
		Ok(false) => {
			tracing::info!("sign-in callback with a state redeemed before; the code is not presented");
			return with_clear(page(StatusCode::BAD_REQUEST, "Sign-in failed", "This sign-in was already used. Start again."));
		}
		Err(e) => {
			crate::report(&e, "redeeming a sign-in state");
			return with_clear(page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed", "Something went wrong on our side. Try again."));
		}
	}
	let issued = match s.concierge.exchange_code(code, &s.redirect_uri(), &pre.verifier).await {
		Ok(i) => i,
		Err(ConciergeError::Refused(tonic::Code::PermissionDenied)) =>
			return with_clear(page(
				StatusCode::FORBIDDEN,
				"No access",
				"Your account has no access to the Service-Arb panel. Ask its admin for one.",
			)),
		Err(ConciergeError::Refused(_)) => return with_clear(page(StatusCode::BAD_REQUEST, "Sign-in failed", "This sign-in has expired or was already used. Start again.")),
		Err(ConciergeError::Unavailable(why)) => {
			tracing::warn!(why, "concierge unavailable at the sign-in callback");
			return with_clear(page(
				StatusCode::SERVICE_UNAVAILABLE,
				"Sign-in unavailable",
				"Signing in is unavailable right now. Try again in a minute.",
			));
		}
		Err(e @ ConciergeError::Failed(_)) => {
			crate::report(&eyre::eyre!(e), "redeeming a sign-in code");
			return with_clear(page(StatusCode::BAD_GATEWAY, "Sign-in failed", "Something went wrong. Try again."));
		}
	};
	// Signing in again from a browser with a session: the old one is closed, not orphaned.
	if let Some(old) = s.cookies.get(&headers, cookies::SESSION).and_then(SessionKey::of_cookie) {
		s.forget_me(&old);
		if let Err(e) = s.panel.close_session(&old).await {
			crate::report(&e, "closing the session a new sign-in replaces");
			return with_clear(page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed", "Something went wrong on our side. Try again."));
		}
	}
	let opened = match s.panel.open_session(issued.user_id, &issued.tokens, now).await {
		Ok(o) => o,
		Err(e) => {
			crate::report(&e, "opening a session");
			return with_clear(page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed", "Something went wrong on our side. Try again."));
		}
	};
	let csrf = match panel::session::random_token() {
		Ok(t) => t,
		Err(e) => {
			crate::report(&e, "making a CSRF token");
			return with_clear(page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed", "Something went wrong on our side. Try again."));
		}
	};
	let max_age = opened.expires_at.duration_since(now).as_secs();
	tracing::info!(user_id = %issued.user_id, "signed in");
	let landing = HeaderValue::from_str(pre.return_to.as_deref().unwrap_or("/")).expect("`/auth/login` let in visible ASCII only");
	let mut res = with_clear((StatusCode::SEE_OTHER, [(header::LOCATION, landing)]).into_response());
	let h = res.headers_mut();
	h.append(header::SET_COOKIE, s.cookies.set(cookies::SESSION, &opened.cookie, max_age, true));
	h.append(header::SET_COOKIE, s.cookies.set(cookies::CSRF, &csrf, max_age, false));
	res
}

/// `POST /auth/logout`: CSRF-checked; every session of the user is closed (a sign-out is
/// meant to end access, not one browser's copy of it) and the cookies cleared.
pub async fn logout(State(s): State<SignIn>, headers: HeaderMap) -> Response {
	if !s.cookies.csrf_ok(&headers) {
		return json_error(StatusCode::FORBIDDEN, "csrf");
	}
	if let Some(key) = s.cookies.get(&headers, cookies::SESSION).and_then(SessionKey::of_cookie) {
		s.forget_me(&key);
		match s.panel.close_all_sessions(&key).await {
			Ok(Some(user_id)) => {
				s.forget_user(user_id);
				tracing::info!(%user_id, "signed out everywhere");
			}
			Ok(None) => {}
			Err(e) => {
				crate::report(&e, "closing a user's sessions");
				return json_error(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
			}
		}
	}
	signed_out(&s, StatusCode::NO_CONTENT.into_response())
}

fn signed_out(s: &SignIn, mut res: Response) -> Response {
	let h = res.headers_mut();
	h.append(header::SET_COOKIE, s.cookies.clear(cookies::SESSION));
	h.append(header::SET_COOKIE, s.cookies.clear(cookies::CSRF));
	res
}

/// Whether a route trusts the cached `GetMe`, or asks concierge afresh: for what must not
/// outlive a revoked permission by even [`ME_TTL`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Freshness {
	Cached,
	Fresh,
}

/// What [`gate`] asks of a route's caller.
#[derive(Clone, Debug)]
pub(crate) struct Gate {
	pub sign_in: SignIn,
	pub freshness: Freshness,
	/// `None`: every signed-in user.
	pub section: Option<Section>,
}

/// The gate of `/api/v1`: CSRF for anything that is not a read, then the session and the
/// caller's permissions, into the request's extensions as a [`Caller`]; then the route's
/// section, else 403.
pub(crate) async fn gate(State(g): State<Gate>, mut req: Request, next: Next) -> Response {
	let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
	if !safe && !g.sign_in.cookies.csrf_ok(req.headers()) {
		return json_error(StatusCode::FORBIDDEN, "csrf");
	}
	let caller = match authenticate(&g.sign_in, req.headers(), g.freshness).await {
		Ok(c) => c,
		Err(res) => return *res,
	};
	if g.section.is_some_and(|section| !section.may(&caller.permissions)) {
		return ApiError::Forbidden.into_response();
	}
	req.extensions_mut().insert(caller);
	let mut res = next.run(req).await;
	// PII and secrets pass through here: nothing is cached.
	res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
	res
}

/// `GetMe`, asked once more when concierge did not answer (a timeout included): one slow
/// answer should not be a 503 for the user.
async fn get_me(s: &SignIn, access: &str) -> Result<Me, ConciergeError> {
	match s.concierge.me(access).await {
		Err(ConciergeError::Unavailable(why)) => {
			tracing::debug!(why, "GetMe unavailable; asking once more");
			s.concierge.me(access).await
		}
		answer => answer,
	}
}

/// Why a caller was not let in, before it is an answer: `/api/v1` answers it, a live socket
/// closes on it.
#[derive(Debug)]
pub(crate) enum Denied {
	/// No session, or one closed: sign in again (401, the cookies cleared).
	Unauthenticated,
	/// Concierge cannot be asked now; nothing is closed (503).
	Unavailable,
	/// Our failure, or concierge's: the status and the message the caller sees.
	Failed(StatusCode, &'static str),
}

impl Denied {
	pub(crate) fn into_response(self, s: &SignIn) -> Response {
		match self {
			Self::Unauthenticated => signed_out(s, json_error(StatusCode::UNAUTHORIZED, "sign in")),
			Self::Unavailable => json_error(StatusCode::SERVICE_UNAVAILABLE, "sign-in is unavailable, try again"),
			Self::Failed(status, msg) => json_error(status, msg),
		}
	}
}

/// The refusal is boxed: a `Response` is large, and the happy path should not carry it.
async fn authenticate(s: &SignIn, headers: &HeaderMap, freshness: Freshness) -> Result<Caller, Box<Response>> {
	check(s, headers, freshness).await.map_err(|denied| Box::new(denied.into_response(s)))
}

/// The session behind the request's cookie and what concierge says its user may do: what
/// [`gate`] lets through, and what a live socket asks again while it is open.
pub(crate) async fn check(s: &SignIn, headers: &HeaderMap, freshness: Freshness) -> Result<Caller, Denied> {
	let cookie = s.cookies.get(headers, cookies::SESSION).ok_or(Denied::Unauthenticated)?;
	let session = match s.panel.session(cookie, Timestamp::now(), &s.concierge).await {
		Ok(session) => session,
		Err(SessionError::Missing | SessionError::Rejected) => return Err(Denied::Unauthenticated),
		Err(SessionError::Unavailable) => return Err(Denied::Unavailable),
		Err(SessionError::Internal(e)) => {
			crate::report(&e, "reading a session");
			return Err(Denied::Failed(StatusCode::INTERNAL_SERVER_ERROR, "internal error"));
		}
	};
	// The user is here: the bot may ask concierge with this session for another week.
	if let Err(e) = s.panel.touch_session(&session.key, Timestamp::now()).await {
		crate::report(&e, "marking a session used");
	}
	let cached = if freshness == Freshness::Cached { s.cached_me(&session.key) } else { None };
	let fresh = cached.is_none();
	let me = match cached {
		Some(me) => me,
		None => match get_me(s, &session.access).await {
			Ok(me) => {
				s.remember_me(session.key, me.clone());
				me
			}
			// The token revoked at concierge (signed out of evinvest.ltd, the user held or
			// disabled): the session is over here too.
			Err(ConciergeError::Refused(code)) => {
				tracing::info!(user_id = %session.user_id, ?code, "GetMe refused; session closed");
				s.forget_me(&session.key);
				if let Err(e) = s.panel.close_session(&session.key).await {
					crate::report(&e, "closing a refused session");
				}
				return Err(Denied::Unauthenticated);
			}
			Err(ConciergeError::Unavailable(why)) => {
				tracing::warn!(why, "concierge unavailable for GetMe");
				return Err(Denied::Unavailable);
			}
			Err(e @ ConciergeError::Failed(_)) => {
				crate::report(&eyre::eyre!(e), "GetMe");
				return Err(Denied::Failed(StatusCode::BAD_GATEWAY, "sign-in failed"));
			}
		},
	};
	if me.user_id != session.user_id {
		crate::report(&eyre::eyre!("GetMe answered another user than the session's"), "GetMe");
		return Err(Denied::Failed(StatusCode::BAD_GATEWAY, "sign-in failed"));
	}
	if fresh {
		// A Telegram link sends only on permissions concierge confirmed lately; every answer
		// the gate gets is such a confirmation. A no-op for an unlinked user.
		let display = if me.preferred_name.trim().is_empty() { &me.email } else { &me.preferred_name };
		if let Err(e) = s.panel.telegram_access_seen(me.user_id, &me.permissions, display, Timestamp::now()).await {
			crate::report(&e, "recording a user's access for Telegram");
		}
	}
	Ok(Caller {
		user_id: me.user_id,
		email: me.email,
		email_verified: me.email_verified,
		preferred_name: me.preferred_name,
		permissions: me.permissions,
		dev_sign_in: s.concierge.is_dev(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn query_values_are_encoded() {
		assert_eq!(encode("https://sa.evinvest.ltd/auth/callback"), "https%3A%2F%2Fsa.evinvest.ltd%2Fauth%2Fcallback");
		assert_eq!(encode("aZ09-._~ &="), "aZ09-._~%20%26%3D");
	}
}

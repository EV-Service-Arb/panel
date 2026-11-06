//! `GET /api/v1/live`: a WebSocket that tells a signed-in screen what changed, so it reads
//! again instead of being reloaded.
//!
//! ```text
//! GET /api/v1/live    Origin = PANEL_PUBLIC_ORIGIN, exactly                 else 403
//!                     the session, as the /api/v1 gate                      else 401 / 503
//!                     ≤ 5 sockets per user, ≤ 200 in all                    else 429
//!                     → 101; the bus subscribed before the answer, so nothing committed after
//!                       it is missed
//! server → client     {"type":"hello","at","user_id"}                        at once
//!                     {"type":"changed","topic","brand_id"?,"id"?,"at"}    after each commit the
//!                                                                          user may read
//!                     {"type":"resync"}                                     fell behind; read all
//!                     Ping every 25 s; no Pong for 60 s → dropped
//! closes              4401 the session ended (sign-out, replaced, refused, expired)
//!                     1001 the server is stopping
//! ```
//!
//! What sites changed comes from its feed ([`follow`]): one subscriber per process, each line
//! onto the bus with the permission that reads it.
//!
//! Only the server speaks: the client's frames are read for Pong and Close and otherwise
//! ignored. CSRF has no say over a GET, so the `Origin` header is what keeps another site's
//! page from opening a socket with the user's cookie (cross-site WebSocket hijacking): a
//! browser always sends it on a WebSocket handshake, and a request without one is refused.
//!
//! The session is asked again every [`LiveLimits::recheck_every`] (and the permissions, through the
//! gate's `GetMe` cache), and at once when the engine says a session ended. What a user is told
//! is what they may read ([`sa_panel::live::Change::visible_to`]). A slow reader never makes the
//! server hold more: the bus keeps a bounded backlog per subscriber and says "lagged" past it,
//! which becomes a `resync`; a write that does not go out within
//! [`LiveLimits::send_within`] drops the socket.

use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
	time::Duration,
};

use axum::{
	extract::{
		State,
		ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
	},
	http::{HeaderMap, StatusCode, header},
	response::{IntoResponse, Response},
};
use sa_panel::{
	live::{Change, Ended, Signal},
	session::SessionKey,
};
use serde::Serialize;
use tokio::sync::broadcast::{self, error::RecvError};
use tracing::Instrument;
use uuid::Uuid;

use crate::{
	cookies,
	forward::Services,
	signin::{self, Caller, Denied, Freshness, SignIn},
};

/// The session ended: sign in again.
pub const CLOSE_SESSION_ENDED: u16 = 4401;
/// RFC 6455's "going away": the server is stopping.
pub const CLOSE_GOING_AWAY: u16 = 1001;

/// How long the feed may say nothing (sites pings every 25 s) before it is taken for dead.
const FEED_IDLE: Duration = Duration::from_secs(60);

/// The longest line sites' feed may send.
const FEED_LINE: usize = 64 * 1024;

/// The waits between reconnections: doubling from the first to the last.
const FEED_BACKOFF: (Duration, Duration) = (Duration::from_secs(1), Duration::from_secs(30));

/// What the live sockets may cost.
#[derive(Clone, Copy, Debug)]
pub struct LiveLimits {
	/// Sockets one user may hold open: a few tabs, not a leak.
	pub per_user: usize,
	/// Sockets open in all.
	pub total: usize,
	/// How often the server pings.
	pub ping_every: Duration,
	/// How long without a Pong before the peer is taken for dead.
	pub pong_within: Duration,
	/// How often the session and the permissions are asked again.
	pub recheck_every: Duration,
	/// How long one frame may take to go out before the socket is dropped.
	pub send_within: Duration,
	/// Handshakes at once (each may wait on concierge for `GetMe`), past it `503`.
	pub upgrade_concurrent: usize,
	pub upgrade_timeout: Duration,
}

impl Default for LiveLimits {
	fn default() -> Self {
		Self {
			per_user: 5,
			total: 200,
			ping_every: Duration::from_secs(25),
			pong_within: Duration::from_secs(60),
			recheck_every: Duration::from_secs(60),
			send_within: Duration::from_secs(10),
			upgrade_concurrent: 16,
			upgrade_timeout: Duration::from_secs(15),
		}
	}
}

/// The route's state: the sign-in (the gate's checks), the sockets open, the limits.
#[derive(Clone, Debug)]
pub struct Live {
	sign_in: SignIn,
	slots: Slots,
	limits: LiveLimits,
}

impl Live {
	pub fn new(sign_in: SignIn, limits: LiveLimits) -> Self {
		Self {
			sign_in,
			slots: Slots::default(),
			limits,
		}
	}
}

/// `GET /api/v1/live`. The upgrade is extracted as a `Result` so a request that is not a
/// handshake is still answered 403/401 first, the same as one that is.
pub async fn upgrade(State(live): State<Live>, headers: HeaderMap, ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>) -> Response {
	if !same_origin(&headers, live.sign_in.panel_origin()) {
		tracing::info!(origin = ?headers.get(header::ORIGIN), "live: a handshake from another origin refused");
		return refuse(StatusCode::FORBIDDEN, "origin");
	}
	let caller = match signin::check(&live.sign_in, &headers, Freshness::Cached).await {
		Ok(c) => c,
		Err(denied) => return denied.into_response(&live.sign_in),
	};
	let ws = match ws {
		Ok(ws) => ws,
		Err(rejection) => return rejection.into_response(),
	};
	// The check passed, so the cookie is there and names a session.
	let Some(key) = live.sign_in.cookies().get(&headers, cookies::SESSION).and_then(SessionKey::of_cookie) else {
		return Denied::Unauthenticated.into_response(&live.sign_in);
	};
	let Some(slot) = live.slots.take(caller.user_id, &live.limits) else {
		tracing::info!(user_id = %caller.user_id, "live: too many sockets");
		return refuse(StatusCode::TOO_MANY_REQUESTS, "too many live connections");
	};
	// Only the cookie is kept, to ask for the session again.
	let mut cookie = HeaderMap::new();
	for v in headers.get_all(header::COOKIE) {
		cookie.append(header::COOKIE, v.clone());
	}
	let rx = live.sign_in.panel.bus().subscribe();
	let span = tracing::info_span!("live", user_id = %caller.user_id);
	ws.max_message_size(4096).max_frame_size(4096).max_write_buffer_size(256 * 1024).on_upgrade(move |socket| {
		Connection {
			socket,
			live,
			caller,
			cookie,
			key,
			rx,
			_slot: slot,
		}
		.run()
		.instrument(span)
	})
}

/// Follows sites' change feed until `shutdown`, onto `bus`: reconnected with a backoff whenever
/// it ends, fails, or goes quiet; sites sends `resync` first on every connection, so whatever
/// was missed meanwhile is read again.
pub async fn follow(services: Services, bus: sa_panel::live::Bus, mut shutdown: tokio::sync::watch::Receiver<bool>) {
	let mut wait = FEED_BACKOFF.0;
	while !*shutdown.borrow() {
		let heard = tokio::select! {
			r = feed(&services, &bus) => r,
			_ = shutdown.changed() => return,
		};
		match heard {
			Ok(true) => wait = FEED_BACKOFF.0,
			Ok(false) => {}
			Err(e) => tracing::warn!(error = format!("{e:#}"), "live: sites' feed broke off"),
		}
		tokio::select! {
			() = tokio::time::sleep(wait) => {}
			_ = shutdown.changed() => return,
		}
		wait = (wait * 2).min(FEED_BACKOFF.1);
	}
}

/// Who holds how many sockets.
#[derive(Clone, Debug, Default)]
struct Slots(Arc<Mutex<Held>>);
impl Slots {
	fn take(&self, user: Uuid, limits: &LiveLimits) -> Option<Slot> {
		let mut held = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		let mine = held.by_user.get(&user).copied().unwrap_or(0);
		if held.total >= limits.total || mine >= limits.per_user {
			return None;
		}
		held.total += 1;
		held.by_user.insert(user, mine + 1);
		Some(Slot { slots: self.clone(), user })
	}
}

#[derive(Debug, Default)]
struct Held {
	total: usize,
	by_user: HashMap<Uuid, usize>,
}

/// One socket's place, given back when dropped — however the socket ends, or if the
/// handshake never completes.
#[derive(Debug)]
struct Slot {
	slots: Slots,
	user: Uuid,
}

impl Drop for Slot {
	fn drop(&mut self) {
		let mut held = self.slots.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		held.total = held.total.saturating_sub(1);
		if let Some(n) = held.by_user.get_mut(&self.user) {
			*n = n.saturating_sub(1);
			if *n == 0 {
				held.by_user.remove(&self.user);
			}
		}
	}
}

/// A frame the server sends.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Out<'a> {
	Hello {
		at: String,
		user_id: String,
	},
	Changed {
		topic: &'a str,
		#[serde(skip_serializing_if = "Option::is_none")]
		brand_id: Option<&'a str>,
		#[serde(skip_serializing_if = "Option::is_none")]
		id: Option<&'a str>,
		at: String,
	},
	Resync,
}

impl<'a> Out<'a> {
	fn changed(c: &'a Change) -> Self {
		Self::Changed {
			topic: &c.topic,
			brand_id: c.brand.as_deref(),
			id: c.id.as_deref(),
			at: c.at.to_string(),
		}
	}

	fn message(&self) -> Message {
		// A derived `Serialize` of strings and options cannot fail; were it to, an empty frame
		// says nothing wrong rather than ending the socket.
		Message::Text(serde_json::to_string(self).unwrap_or_default().into())
	}
}

fn refuse(status: StatusCode, msg: &str) -> Response {
	(status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

/// Whether the handshake comes from the panel's own pages. Browsers send `Origin` on every
/// WebSocket handshake and scripts cannot forge it; its absence is something other than our
/// front end, and is refused as well.
fn same_origin(headers: &HeaderMap, ours: &str) -> bool {
	headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).is_some_and(|origin| origin.eq_ignore_ascii_case(ours))
}

struct Connection {
	socket: WebSocket,
	live: Live,
	caller: Caller,
	/// The handshake's `Cookie` header, for [`signin::check`] again.
	cookie: HeaderMap,
	key: SessionKey,
	rx: broadcast::Receiver<Signal>,
	_slot: Slot,
}
impl Connection {
	async fn run(mut self) {
		let limits = self.live.limits;
		tracing::debug!("live: open");
		let hello = Out::Hello {
			at: jiff::Timestamp::now().to_string(),
			user_id: self.caller.user_id.to_string(),
		};
		if !self.send(hello.message()).await {
			return;
		}
		let start = tokio::time::Instant::now();
		let mut ping = tokio::time::interval_at(start + limits.ping_every, limits.ping_every);
		ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
		let mut recheck = tokio::time::interval_at(start + limits.recheck_every, limits.recheck_every);
		recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
		let pong_deadline = tokio::time::sleep(limits.pong_within);
		tokio::pin!(pong_deadline);
		loop {
			let step = tokio::select! {
				signal = self.rx.recv() => self.on_signal(signal),
				incoming = self.socket.recv() => match incoming {
					Some(Ok(Message::Pong(_))) => {
						pong_deadline.as_mut().reset(tokio::time::Instant::now() + limits.pong_within);
						Step::Nothing
					}
					// The socket answers Pings by itself; the client has nothing else to say here.
					Some(Ok(Message::Ping(_) | Message::Text(_) | Message::Binary(_))) => Step::Nothing,
					Some(Ok(Message::Close(_)) | Err(_)) | None => Step::Drop,
				},
				() = &mut pong_deadline => {
					tracing::debug!("live: no pong; dropped");
					Step::Drop
				}
				_ = ping.tick() => Step::Send(Message::Ping(Default::default())),
				_ = recheck.tick() => match signin::check(&self.live.sign_in, &self.cookie, Freshness::Cached).await {
					Ok(caller) => {
						self.caller = caller;
						Step::Nothing
					}
					Err(Denied::Unauthenticated) => Step::Close(CLOSE_SESSION_ENDED, "session ended"),
					// The API would answer 503 and keep the session: so does the socket, and it
					// asks again at the next tick.
					Err(Denied::Unavailable | Denied::Failed(..)) => Step::Nothing,
				},
			};
			match step {
				Step::Nothing => {}
				Step::Send(m) =>
					if !self.send(m).await {
						return;
					},
				Step::Lagged(missed) => {
					tracing::debug!(missed, "live: fell behind; resync");
					self.rx = self.rx.resubscribe();
					if !self.send(Out::Resync.message()).await {
						return;
					}
				}
				Step::Close(code, reason) => {
					tracing::debug!(code, "live: closed");
					let frame = CloseFrame {
						code,
						reason: Utf8Bytes::from_static(reason),
					};
					// The last word: whether it reached the peer changes nothing here.
					let _last = self.send(Message::Close(Some(frame))).await;
					return;
				}
				Step::Drop => return,
			}
		}
	}

	fn on_signal(&self, signal: Result<Signal, RecvError>) -> Step {
		match signal {
			Ok(Signal::Changed(c)) if c.visible_to(self.caller.user_id, &self.caller.permissions) => Step::Send(Out::changed(&c).message()),
			Ok(Signal::Changed(_)) => Step::Nothing,
			Ok(Signal::Resync) => Step::Send(Out::Resync.message()),
			Ok(Signal::SessionsEnded(ended)) => {
				let mine = match ended {
					Ended::User(user) => user == self.caller.user_id,
					Ended::Session(key) => key == self.key,
				};
				if mine { Step::Close(CLOSE_SESSION_ENDED, "session ended") } else { Step::Nothing }
			}
			Err(RecvError::Lagged(missed)) => Step::Lagged(missed),
			Ok(Signal::GoingAway) | Err(RecvError::Closed) => Step::Close(CLOSE_GOING_AWAY, "server stopping"),
		}
	}

	/// Sends one frame within [`LiveLimits::send_within`]; `false`: the socket is done.
	async fn send(&mut self, m: Message) -> bool {
		matches!(tokio::time::timeout(self.live.limits.send_within, self.socket.send(m)).await, Ok(Ok(())))
	}
}

/// What one turn of the loop decided.
enum Step {
	Nothing,
	Send(Message),
	/// Behind by more than the bus holds: skip what is left of it and say so.
	Lagged(u64),
	Close(u16, &'static str),
	/// The peer is gone or broke the protocol: nothing more to say to it.
	Drop,
}

/// A line of sites' feed (`GET /api/conductor/changes`).
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Line {
	Changed {
		topic: String,
		brand_id: Option<String>,
		id: Option<String>,
		at: jiff::Timestamp,
		permission: String,
	},
	Resync,
	Ping,
}

/// One connection to the feed, until it ends: whether a line came through it.
async fn feed(services: &Services, bus: &sa_panel::live::Bus) -> eyre::Result<bool> {
	use http_body_util::BodyExt;

	let res = services
		.send(
			sa_auth::Service::Sites,
			axum::http::Method::GET,
			"/api/conductor/changes",
			HeaderMap::new(),
			Some(sa_auth::Principal::Conductor),
			axum::body::Body::empty(),
		)
		.await?;
	eyre::ensure!(res.status() == StatusCode::OK, "sites answered {}", res.status());
	let mut body = res.into_body();
	let mut pending: Vec<u8> = Vec::new();
	let mut heard = false;
	loop {
		let frame = match tokio::time::timeout(FEED_IDLE, body.frame()).await {
			Ok(Some(frame)) => frame?,
			Ok(None) => return Ok(heard),
			Err(_) => eyre::bail!("nothing for {}s", FEED_IDLE.as_secs()),
		};
		let Ok(data) = frame.into_data() else { continue };
		pending.extend_from_slice(&data);
		while let Some(end) = pending.iter().position(|b| *b == b'\n') {
			let line: Vec<u8> = pending.drain(..=end).collect();
			let line = line.trim_ascii();
			if line.is_empty() {
				continue;
			}
			heard = true;
			match serde_json::from_slice::<Line>(line)? {
				Line::Changed {
					topic,
					brand_id,
					id,
					at,
					permission,
				} => {
					eyre::ensure!(
						permission.starts_with(sa_auth::Service::Sites.prefix()),
						"a change readable by {permission}, outside sites' permissions"
					);
					bus.changed(Change {
						topic,
						brand: brand_id,
						id,
						at,
						audience: sa_panel::live::Audience::Holders(permission),
					});
				}
				Line::Resync => bus.publish(Signal::Resync),
				Line::Ping => {}
			}
		}
		eyre::ensure!(pending.len() <= FEED_LINE, "a line longer than {FEED_LINE} bytes");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slots_are_bounded_and_given_back() {
		let limits = LiveLimits {
			per_user: 2,
			total: 3,
			..Default::default()
		};
		let slots = Slots::default();
		let (ann, bob) = (Uuid::from_u128(1), Uuid::from_u128(2));
		let a1 = slots.take(ann, &limits).unwrap();
		let _a2 = slots.take(ann, &limits).unwrap();
		assert!(slots.take(ann, &limits).is_none(), "per user");
		let _b1 = slots.take(bob, &limits).unwrap();
		assert!(slots.take(bob, &limits).is_none(), "in all");
		drop(a1);
		assert!(slots.take(bob, &limits).is_some(), "given back");
	}

	#[test]
	fn frames_are_the_contract() {
		let c = Change {
			topic: "places".into(),
			brand: Some("aquafix".into()),
			id: Some("royat".into()),
			at: "2026-10-03T10:00:00Z".parse().unwrap(),
			audience: sa_panel::live::Audience::User(Uuid::nil()),
		};
		let Message::Text(t) = Out::changed(&c).message() else { panic!("text") };
		assert_eq!(t.as_str(), r#"{"type":"changed","topic":"places","brand_id":"aquafix","id":"royat","at":"2026-10-03T10:00:00Z"}"#);
		let c = Change { brand: None, id: None, ..c };
		let Message::Text(t) = Out::changed(&c).message() else { panic!("text") };
		assert_eq!(t.as_str(), r#"{"type":"changed","topic":"places","at":"2026-10-03T10:00:00Z"}"#, "no nulls");
		let Message::Text(t) = Out::Resync.message() else { panic!("text") };
		assert_eq!(t.as_str(), r#"{"type":"resync"}"#);
	}

	#[test]
	fn origin_is_exact() {
		let with = |o: Option<&str>| {
			let mut h = HeaderMap::new();
			if let Some(o) = o {
				h.insert(header::ORIGIN, o.parse().unwrap());
			}
			same_origin(&h, "https://sa.evinvest.ltd")
		};
		assert!(with(Some("https://sa.evinvest.ltd")));
		assert!(!with(Some("https://sa.evinvest.ltd.evil.example")));
		assert!(!with(Some("http://sa.evinvest.ltd")));
		assert!(!with(Some("null")));
		assert!(!with(None), "no Origin, no socket");
	}
}

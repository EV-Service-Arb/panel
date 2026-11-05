//! `/api/v1/live` on a real server: a TCP listener, the real router, dev sign-in for the
//! session, and a WebSocket client written out here (the handshake and the frames this
//! contract uses), so the test speaks the wire a browser does.

use std::{collections::HashMap, convert::Infallible, net::SocketAddr, time::Duration};

use axum::{
	Router,
	body::{Body, Bytes},
	http::{Method, Request, StatusCode, header},
	routing::get,
};
use jiff::Timestamp;
use sa_alerts::Rule;
use sa_auth::{PermissionSet, SourceKind};
use sa_core::ids::BrandId;
use sa_panel::{
	Panel,
	live::{Audience, Bus, Change, Signal},
	testing::{TestDb, admin, operator, panel},
};
use sa_server::{
	concierge::{DEV_CODE, DevIdentity},
	http::Limits,
	live::{self, CLOSE_SESSION_ENDED, LiveLimits},
	testing::{app_with, services, upstream},
};
use serde_json::{Value, json};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	net::TcpStream,
	sync::{broadcast, watch},
};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:59120";
/// Long enough for a loaded CI machine; every wait here ends as soon as its frame arrives.
const PATIENCE: Duration = Duration::from_secs(10);

fn email(permissions: &PermissionSet) -> String {
	format!("dev-{}@localhost", permissions.iter().count())
}

fn user_id(permissions: &PermissionSet) -> uuid::Uuid {
	DevIdentity::named(&email(permissions))
}

/// Sites' change feed, as the test feeds it: each connection opens with `resync`, then every
/// line sent on the channel; `None` ends the connection.
async fn sites() -> (String, broadcast::Sender<Option<String>>) {
	let (lines, _) = broadcast::channel::<Option<String>>(64);
	let feed = lines.clone();
	let routes = Router::new().route(
		"/api/conductor/changes",
		get(move || {
			let rx = feed.subscribe();
			let stream = futures::stream::unfold((rx, true), |(mut rx, first)| async move {
				if first {
					return Some((Ok::<_, Infallible>(Bytes::from_static(b"{\"type\":\"resync\"}\n")), (rx, false)));
				}
				match rx.recv().await {
					Ok(Some(line)) => Some((Ok(Bytes::from(format!("{line}\n"))), (rx, false))),
					Ok(None) | Err(_) => None,
				}
			});
			async move { Body::from_stream(stream) }
		}),
	);
	let (base, _) = upstream(routes).await;
	(base, lines)
}

/// The conductor signing everyone in as holding `permissions`, served on a port of its own,
/// every service at `base`.
async fn serve(panel: Panel, permissions: PermissionSet, limits: Limits, base: &str) -> (Router, SocketAddr) {
	let app = app_with(panel, permissions.clone(), &email(&permissions), base, limits);
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	let served = app.clone();
	// Lives as long as the test's runtime; nothing to report but a panic, which fails the test.
	tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
	(app, addr)
}

/// `serve` with no sites to reach.
async fn alone(panel: Panel, permissions: PermissionSet, limits: Limits) -> (Router, SocketAddr) {
	serve(panel, permissions, limits, "http://127.0.0.1:9").await
}

/// The feed followed onto `bus` until the sender is dropped.
fn follow(base: &str, bus: &Bus) -> watch::Sender<bool> {
	let (stop, stopped) = watch::channel(false);
	tokio::spawn(live::follow(services(base), bus.clone(), stopped));
	stop
}

// ── a browser's cookies, through the router in process ───────────────────────────────────

#[derive(Default)]
struct Browser {
	jar: HashMap<String, String>,
}

impl Browser {
	async fn send(&mut self, app: &Router, method: Method, uri: &str) -> (StatusCode, Option<String>) {
		let mut req = Request::builder().method(method).uri(uri);
		if let Some(cookie) = self.cookie() {
			req = req.header(header::COOKIE, cookie);
		}
		if let Some(t) = self.jar.get("sa_csrf") {
			req = req.header("x-sa-csrf", t);
		}
		let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
		for c in res.headers().get_all(header::SET_COOKIE) {
			let c = c.to_str().unwrap();
			let (pair, attrs) = c.split_once(';').unwrap_or((c, ""));
			let (k, v) = pair.split_once('=').unwrap();
			if attrs.contains("Max-Age=0") {
				self.jar.remove(k);
			} else {
				self.jar.insert(k.to_owned(), v.to_owned());
			}
		}
		let location = res.headers().get(header::LOCATION).map(|l| l.to_str().unwrap().to_owned());
		(res.status(), location)
	}

	async fn sign_in(&mut self, app: &Router) {
		let (_, location) = self.send(app, Method::GET, "/auth/login").await;
		let path = location.unwrap().strip_prefix(ORIGIN).unwrap().to_owned();
		assert!(path.starts_with(&format!("/auth/callback?code={DEV_CODE}")), "{path}");
		assert_eq!(self.send(app, Method::GET, &path).await.0, StatusCode::SEE_OTHER);
		assert!(self.jar.contains_key("sa_session"));
	}

	fn cookie(&self) -> Option<String> {
		(!self.jar.is_empty()).then(|| self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "))
	}
}

// ── the WebSocket client ─────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum Frame {
	Text(Value),
	Close(u16),
	Ping,
	/// The server hung up without a close frame.
	Eof,
}

struct Ws {
	stream: TcpStream,
}

impl Ws {
	/// The handshake, as a browser on `origin` with `cookie` sends it: the socket, or the
	/// status the server answered instead of 101.
	async fn connect(addr: SocketAddr, origin: Option<&str>, cookie: Option<&str>) -> Result<Self, u16> {
		let mut stream = TcpStream::connect(addr).await.unwrap();
		let mut req =
			format!("GET /api/v1/live HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n");
		if let Some(o) = origin {
			req.push_str(&format!("Origin: {o}\r\n"));
		}
		if let Some(c) = cookie {
			req.push_str(&format!("Cookie: {c}\r\n"));
		}
		req.push_str("\r\n");
		stream.write_all(req.as_bytes()).await.unwrap();
		let mut head = Vec::new();
		while !head.ends_with(b"\r\n\r\n") {
			let mut b = [0u8; 1];
			assert_eq!(
				tokio::time::timeout(PATIENCE, stream.read(&mut b)).await.expect("the handshake's answer").unwrap(),
				1,
				"closed mid-answer"
			);
			head.push(b[0]);
		}
		let head = String::from_utf8(head).unwrap();
		let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
		if status != 101 {
			return Err(status);
		}
		assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "the accept key of RFC 6455's example: {head}");
		Ok(Self { stream })
	}

	/// One frame as it comes; the server's are never masked.
	async fn raw(&mut self) -> Frame {
		let mut h = [0u8; 2];
		match tokio::time::timeout(PATIENCE, self.stream.read_exact(&mut h)).await.expect("a frame in time") {
			Ok(_) => {}
			Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof || e.kind() == std::io::ErrorKind::ConnectionReset => return Frame::Eof,
			Err(e) => panic!("{e}"),
		}
		assert_eq!(h[1] & 0x80, 0, "a server's frame is not masked");
		let len = match h[1] & 0x7f {
			126 => u64::from(self.stream.read_u16().await.unwrap()),
			127 => self.stream.read_u64().await.unwrap(),
			n => u64::from(n),
		};
		let mut payload = vec![0u8; usize::try_from(len).unwrap()];
		self.stream.read_exact(&mut payload).await.unwrap();
		match h[0] & 0x0f {
			0x1 => Frame::Text(serde_json::from_slice(&payload).unwrap()),
			0x8 => Frame::Close(u16::from_be_bytes([payload[0], payload[1]])),
			0x9 => Frame::Ping,
			op => panic!("unexpected opcode {op:#x}"),
		}
	}

	/// The next frame that is not a Ping, answering Pings as a browser does.
	async fn next(&mut self) -> Frame {
		loop {
			match self.raw().await {
				Frame::Ping => self.pong().await,
				f => return f,
			}
		}
	}

	async fn text(&mut self) -> Value {
		match self.next().await {
			Frame::Text(v) => v,
			other => panic!("expected a text frame, got {other:?}"),
		}
	}

	/// A Close frame with 1000, "normal".
	async fn close(&mut self) {
		let mask = [1u8, 2, 3, 4];
		let code = 1000u16.to_be_bytes();
		self.stream
			.write_all(&[0x88, 0x82, mask[0], mask[1], mask[2], mask[3], code[0] ^ mask[0], code[1] ^ mask[1]])
			.await
			.unwrap();
	}

	async fn pong(&mut self) {
		// A client's frames are masked; an empty Pong has nothing to mask but the key.
		self.stream.write_all(&[0x8a, 0x80, 1, 2, 3, 4]).await.unwrap();
	}
}

async fn signed_in(app: &Router) -> Browser {
	let mut b = Browser::default();
	b.sign_in(app).await;
	b
}

async fn open(addr: SocketAddr, b: &Browser) -> Ws {
	let mut ws = Ws::connect(addr, Some(ORIGIN), b.cookie().as_deref()).await.expect("upgraded");
	assert_eq!(ws.text().await["type"], "hello");
	ws
}

// ── the tests ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn refused_before_the_upgrade_and_open_to_anyone_signed_in() {
	let db = TestDb::create().await;
	let (app, addr) = alone(panel(&db).await, admin(), Limits::default()).await;
	assert_eq!(Ws::connect(addr, Some(ORIGIN), None).await.err(), Some(401), "no session");
	assert_eq!(Ws::connect(addr, Some(ORIGIN), Some("sa_session=nonsense")).await.err(), Some(401), "no such session");

	let b = signed_in(&app).await;
	let cookie = b.cookie();
	assert_eq!(Ws::connect(addr, Some("https://evil.example"), cookie.as_deref()).await.err(), Some(403), "another site's page");
	assert_eq!(Ws::connect(addr, Some("http://127.0.0.1:59120.evil.example"), cookie.as_deref()).await.err(), Some(403));
	assert_eq!(Ws::connect(addr, None, cookie.as_deref()).await.err(), Some(403), "no Origin at all");
	assert!(Ws::connect(addr, Some(ORIGIN), cookie.as_deref()).await.is_ok(), "ours");

	let (app, addr) = alone(panel(&db).await, PermissionSet::from_iter(Vec::<String>::new()), Limits::default()).await;
	let b = signed_in(&app).await;
	let mut ws = Ws::connect(addr, Some(ORIGIN), b.cookie().as_deref()).await.expect("no permission needed: what it is told is");
	let hello = ws.text().await;
	assert_eq!(hello["type"], "hello");
	hello["at"].as_str().unwrap().parse::<Timestamp>().expect("RFC 3339");
}

/// The conductor's own topics: the keys to whoever manages them, a user's alerts to that user.
#[tokio::test]
async fn the_conductors_own_changes_go_to_who_may_read_them() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (admin_app, admin_addr) = alone(panel.clone(), admin(), Limits::default()).await;
	let (op_app, op_addr) = alone(panel.clone(), operator(), Limits::default()).await;
	let mut admin_ws = open(admin_addr, &signed_in(&admin_app).await).await;
	let mut op = open(op_addr, &signed_in(&op_app).await).await;

	panel
		.add_source(
			"vifnet-site",
			SourceKind::Site,
			[BrandId::parse("vifnet").unwrap()].into(),
			Some(sa_core::ids::SiteOrigin::parse("https://vifnet.site").unwrap()),
		)
		.await
		.unwrap()
		.unwrap();
	for (permissions, chat, rule) in [(admin(), 1, Rule::PaymentReceived), (operator(), 2, Rule::NewLead)] {
		let user = user_id(&permissions);
		let destination: i64 = sqlx::query_scalar("INSERT INTO alert_destinations (user_id, method, created_at) VALUES ($1, 'telegram', 0) RETURNING id")
			.bind(user)
			.fetch_one(panel.store().pool())
			.await
			.unwrap();
		sqlx::query("INSERT INTO telegram_destinations (destination_id, kind, chat_id) VALUES ($1, 'private', $2)")
			.bind(destination)
			.bind(chat)
			.execute(panel.store().pool())
			.await
			.unwrap();
		panel.alerts_set_rules(user, &permissions, destination, &[(rule, true)]).await.unwrap().unwrap();
	}

	let admin_saw: Vec<String> = [admin_ws.text().await, admin_ws.text().await].iter().map(|f| f["topic"].as_str().unwrap().to_owned()).collect();
	assert_eq!(admin_saw, ["sources", "alerts"]);
	// Frames keep their order: had the operator been told of the keys, they would come first.
	assert_eq!(op.text().await["topic"], "alerts");
}

/// sites' feed: each change to whoever holds the permission it names, `resync` to everyone, and
/// a feed that broke off is followed again — opening with `resync`, so what was missed is read.
#[tokio::test]
async fn sites_changes_reach_only_their_readers_and_the_feed_comes_back() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (base, lines) = sites().await;
	let analyst: PermissionSet = ["sa:sites:analysis:read"].into_iter().collect();
	let (op_app, op_addr) = serve(panel.clone(), operator(), Limits::default(), &base).await;
	let (an_app, an_addr) = serve(panel.clone(), analyst, Limits::default(), &base).await;
	let mut op = open(op_addr, &signed_in(&op_app).await).await;
	let mut an = open(an_addr, &signed_in(&an_app).await).await;

	let _following = follow(&base, panel.bus());
	assert_eq!(op.text().await, json!({"type": "resync"}), "the feed's first line");
	assert_eq!(an.text().await, json!({"type": "resync"}));
	let line =
		|topic: &str, permission: &str| json!({"type": "changed", "topic": topic, "brand_id": "aquafix", "id": "L-1", "at": "2026-11-02T10:00:00Z", "permission": permission}).to_string();
	lines.send(Some(line("leads", "sa:sites:work:read"))).unwrap();
	lines.send(Some(json!({"type": "ping"}).to_string())).unwrap();
	lines.send(Some(line("experiments", "sa:sites:analysis:read"))).unwrap();
	assert_eq!(
		op.text().await,
		json!({"type": "changed", "topic": "leads", "brand_id": "aquafix", "id": "L-1", "at": "2026-11-02T10:00:00Z"}),
		"the frame as the screens know it, without the permission"
	);
	assert_eq!(op.text().await["topic"], "experiments");
	assert_eq!(an.text().await["topic"], "experiments", "the work topic was never the analyst's");

	lines.send(None).unwrap();
	assert_eq!(op.text().await, json!({"type": "resync"}), "followed again");
	assert_eq!(an.text().await, json!({"type": "resync"}));
	lines.send(Some(line("leads", "sa:sites:work:read"))).unwrap();
	assert_eq!(op.text().await["topic"], "leads");
}

#[tokio::test]
async fn signing_out_closes_a_socket_the_feed_was_telling() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (base, lines) = sites().await;
	let (app, addr) = serve(panel.clone(), admin(), Limits::default(), &base).await;
	let mut b = signed_in(&app).await;
	let mut ws = open(addr, &b).await;
	let _following = follow(&base, panel.bus());
	assert_eq!(ws.text().await, json!({"type": "resync"}));
	lines
		.send(Some(
			json!({"type": "changed", "topic": "leads", "at": "2026-11-02T10:00:00Z", "permission": "sa:sites:work:read"}).to_string(),
		))
		.unwrap();
	assert_eq!(ws.text().await["topic"], "leads");
	assert_eq!(b.send(&app, Method::POST, "/auth/logout").await.0, StatusCode::NO_CONTENT);
	assert_eq!(ws.next().await, Frame::Close(CLOSE_SESSION_ENDED));
}

#[tokio::test]
async fn a_new_sign_in_closes_the_socket_of_the_session_it_replaces() {
	let db = TestDb::create().await;
	let (app, addr) = alone(panel(&db).await, admin(), Limits::default()).await;
	let mut b = signed_in(&app).await;
	let mut ws = open(addr, &b).await;
	b.sign_in(&app).await;
	assert_eq!(ws.next().await, Frame::Close(CLOSE_SESSION_ENDED));
}

/// A session ended with nobody telling the bus (expired, or deleted from the database) is
/// found at the next recheck.
#[tokio::test]
async fn a_session_gone_is_found_at_the_recheck() {
	let db = TestDb::create().await;
	let limits = Limits {
		live: LiveLimits {
			recheck_every: Duration::from_millis(200),
			..Default::default()
		},
		..Default::default()
	};
	let (app, addr) = alone(panel(&db).await, admin(), limits).await;
	let mut ws = open(addr, &signed_in(&app).await).await;
	sqlx::query("DELETE FROM sessions").execute(&db.pool().await).await.unwrap();
	assert_eq!(ws.next().await, Frame::Close(CLOSE_SESSION_ENDED));
}

fn change(topic: &str, id: Option<String>) -> Change {
	Change {
		topic: topic.into(),
		brand: Some("aquafix".into()),
		id,
		at: Timestamp::now(),
		audience: Audience::Holders("sa:sites:work:read".into()),
	}
}

/// The test's runtime has one thread: the ten changes are published before the socket's task
/// runs again, so it is behind by more than the bus of two holds.
#[tokio::test]
async fn a_reader_behind_is_told_to_resync() {
	let db = TestDb::create().await;
	let panel = panel(&db).await.with_live(Bus::new(2));
	let (app, addr) = alone(panel.clone(), admin(), Limits::default()).await;
	let mut ws = open(addr, &signed_in(&app).await).await;
	for i in 0..10 {
		panel.bus().changed(change("lead", Some(format!("L-{i}"))));
	}
	assert_eq!(ws.text().await, json!({"type": "resync"}));
	// What was left of the backlog is skipped: the next frame is what comes after.
	panel.bus().publish(Signal::Resync);
	assert_eq!(ws.text().await, json!({"type": "resync"}));
	panel.bus().changed(change("places", None));
	assert_eq!(ws.text().await["topic"], "places");
}

#[tokio::test]
async fn too_many_sockets_are_refused() {
	let db = TestDb::create().await;
	let limits = Limits {
		live: LiveLimits { per_user: 2, ..Default::default() },
		..Default::default()
	};
	let (app, addr) = alone(panel(&db).await, admin(), limits).await;
	let b = signed_in(&app).await;
	let first = open(addr, &b).await;
	let _second = open(addr, &b).await;
	assert_eq!(Ws::connect(addr, Some(ORIGIN), b.cookie().as_deref()).await.err(), Some(429));
	// Closed by the client: the server lets go of the socket and its slot together, before the
	// client sees the connection end.
	let mut first = first;
	first.close().await;
	while first.raw().await != Frame::Eof {}
	assert!(Ws::connect(addr, Some(ORIGIN), b.cookie().as_deref()).await.is_ok(), "the slot was given back");
}

#[tokio::test]
async fn a_silent_peer_is_dropped() {
	let db = TestDb::create().await;
	let limits = Limits {
		live: LiveLimits {
			ping_every: Duration::from_millis(100),
			pong_within: Duration::from_millis(400),
			..Default::default()
		},
		..Default::default()
	};
	let (app, addr) = alone(panel(&db).await, admin(), limits).await;
	let mut ws = open(addr, &signed_in(&app).await).await;
	// Pings arrive and go unanswered, then the server hangs up.
	assert_eq!(ws.raw().await, Frame::Ping);
	loop {
		match ws.raw().await {
			Frame::Ping => {}
			Frame::Eof => break,
			other => panic!("{other:?}"),
		}
	}

	// Answered, the socket stays.
	let mut ws = open(addr, &signed_in(&app).await).await;
	for _ in 0..8 {
		assert_eq!(ws.raw().await, Frame::Ping);
		ws.pong().await;
	}
}

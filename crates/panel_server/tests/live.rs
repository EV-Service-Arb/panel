//! `/api/v1/live` on a real server: a TCP listener, the real router, dev sign-in for the
//! session, and a WebSocket client written out here (the handshake and the frames this
//! contract uses), so the test speaks the wire a browser does.

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use axum::{
	Router,
	body::{Body, to_bytes},
	http::{Method, Request, StatusCode, header},
};
use jiff::Timestamp;
use panel::{
	Panel,
	live::{Bus, Change, Signal, Topic},
	testing::{TestDb, event, panel, sign},
};
use panel_core::{ids::BrandId, notify::Rule};
use panel_server::{
	concierge::{Concierge, DEV_CODE, DevIdentity},
	http::{self, Limits},
	live::{CLOSE_SESSION_ENDED, LiveLimits},
	signin::{SignIn, SignInConfig},
};
use sa_auth::SourceKind;
use serde_json::{Value, json};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	net::TcpStream,
};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:59120";
/// Long enough for a loaded CI machine; every wait here ends as soon as its frame arrives.
const PATIENCE: Duration = Duration::from_secs(10);

fn who(alias: &str) -> DevIdentity {
	DevIdentity {
		permissions: sa_auth::Catalog::collect("sa", 0).aliases[alias].iter().cloned().collect(),
		email: format!("dev-{}@localhost", alias.trim_start_matches("sa:")),
	}
}

/// The panel's router signing everyone in as `alias` holds, served on a port of its own.
async fn serve(panel: Panel, alias: &str, limits: Limits) -> (Router, SocketAddr) {
	let config = SignInConfig {
		panel_origin: ORIGIN.to_owned(),
		concierge_origin: ORIGIN.to_owned(),
	};
	let app = http::app_with(SignIn::new(panel, Concierge::dev(who(alias)), config), limits);
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	let served = app.clone();
	// Lives as long as the test's runtime; nothing to report but a panic, which fails the test.
	tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
	(app, addr)
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

/// A source for aquafix; its secret.
async fn aquafix_site(panel: &Panel) -> String {
	panel
		.add_source("aquafix-site", SourceKind::Site, [BrandId::parse("aquafix").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string()
}

/// A lead posted by the site through ingest, as the cluster's landings post them.
async fn post_lead(app: &Router, secret: &str, lead_id: &str) {
	let now = Timestamp::now();
	let created = event(
		"lead.created",
		now,
		"site",
		json!({"brandId": "aquafix", "locationId": "royat", "leadId": lead_id}),
		json!({"channel": "form"}),
	);
	let signed = sign("aquafix-site", secret, &[created], now);
	let req = Request::post("/api/ingest/v1/events")
		.header("content-type", "application/json")
		.header("x-sa-key-id", &signed.key_id)
		.header("x-sa-timestamp", &signed.timestamp)
		.header("x-sa-signature", &signed.signature)
		.body(Body::from(signed.body.clone()))
		.unwrap();
	let res = app.clone().oneshot(req).await.unwrap();
	let status = res.status();
	let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
	assert_eq!(status, StatusCode::MULTI_STATUS, "{}", String::from_utf8_lossy(&body));
	assert!(String::from_utf8_lossy(&body).contains("accepted"));
}

// ── the tests ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn refused_before_the_upgrade() {
	let db = TestDb::create().await;
	let (app, addr) = serve(panel(&db).await, "sa:admin", Limits::default()).await;
	assert_eq!(Ws::connect(addr, Some(ORIGIN), None).await.err(), Some(401), "no session");
	assert_eq!(Ws::connect(addr, Some(ORIGIN), Some("sa_session=nonsense")).await.err(), Some(401), "no such session");

	let b = signed_in(&app).await;
	let cookie = b.cookie();
	assert_eq!(Ws::connect(addr, Some("https://evil.example"), cookie.as_deref()).await.err(), Some(403), "another site's page");
	assert_eq!(Ws::connect(addr, Some("http://127.0.0.1:59120.evil.example"), cookie.as_deref()).await.err(), Some(403));
	assert_eq!(Ws::connect(addr, None, cookie.as_deref()).await.err(), Some(403), "no Origin at all");
	assert!(Ws::connect(addr, Some(ORIGIN), cookie.as_deref()).await.is_ok(), "ours");
}

#[tokio::test]
async fn hello_then_a_new_lead() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = aquafix_site(&panel).await;
	let (app, addr) = serve(panel, "sa:operator", Limits::default()).await;
	let b = signed_in(&app).await;

	let mut ws = Ws::connect(addr, Some(ORIGIN), b.cookie().as_deref()).await.expect("upgraded");
	let hello = ws.text().await;
	assert_eq!(hello["type"], "hello");
	assert_eq!(hello["user_id"], who("sa:operator").user_id().to_string());
	hello["at"].as_str().unwrap().parse::<Timestamp>().expect("RFC 3339");

	post_lead(&app, &secret, "L-1").await;
	let changed = ws.text().await;
	assert_eq!(changed["type"], "changed", "{changed}");
	assert_eq!(changed["topic"], "leads");
	assert_eq!(changed["brand_id"], "aquafix");
	assert_eq!(changed["id"], "L-1");
	changed["at"].as_str().unwrap().parse::<Timestamp>().expect("RFC 3339");

	// A stage moved by the operator API: the one lead.
	let mut b = b;
	let (status, _) = b.send(&app, Method::GET, "/api/v1/leads/aquafix/L-1").await;
	assert_eq!(status, StatusCode::OK);
	let req = Request::post("/api/v1/leads/aquafix/L-1/stage")
		.header(header::COOKIE, b.cookie().unwrap())
		.header("x-sa-csrf", b.jar["sa_csrf"].clone())
		.header("content-type", "application/json")
		.body(Body::from(r#"{"stage":"contacted"}"#))
		.unwrap();
	assert_eq!(app.clone().oneshot(req).await.unwrap().status(), StatusCode::CREATED);
	let changed = ws.text().await;
	assert_eq!(
		(changed["topic"].as_str(), changed["id"].as_str(), changed["brand_id"].as_str()),
		(Some("lead"), Some("L-1"), Some("aquafix")),
		"{changed}"
	);
}

/// No permission is narrower than the whole panel: whoever works reads every brand,
/// so what an operator is kept from is what it cannot read — sources, and another user's
/// Telegram link.
#[tokio::test]
async fn told_only_what_one_may_read() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = aquafix_site(&panel).await;
	let (admin_app, admin_addr) = serve(panel.clone(), "sa:admin", Limits::default()).await;
	let (op_app, op_addr) = serve(panel.clone(), "sa:operator", Limits::default()).await;
	let mut admin = open(admin_addr, &signed_in(&admin_app).await).await;
	let mut op = open(op_addr, &signed_in(&op_app).await).await;

	panel
		.add_source("vifnet-site", SourceKind::Site, [BrandId::parse("vifnet").unwrap()].into())
		.await
		.unwrap()
		.unwrap();
	panel
		.telegram_set_rules(who("sa:admin").user_id(), &panel::testing::admin(), &[(Rule::PaymentReceived, true)])
		.await
		.unwrap();
	post_lead(&admin_app, &secret, "L-2").await;

	let admin_saw: Vec<String> = [admin.text().await, admin.text().await, admin.text().await]
		.iter()
		.map(|f| f["topic"].as_str().unwrap().to_owned())
		.collect();
	assert_eq!(admin_saw, ["sources", "telegram", "leads"]);
	// Frames keep their order: had the operator been told of either, it would come first.
	let first = op.text().await;
	assert_eq!((first["topic"].as_str(), first["id"].as_str()), (Some("leads"), Some("L-2")), "{first}");
}

/// A brand's pricing saved by an admin over the API: every reader.s screens are told.
#[tokio::test]
async fn a_pricing_save_is_told_to_every_reader() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let (admin_app, _) = serve(panel.clone(), "sa:admin", Limits::default()).await;
	let (op_app, op_addr) = serve(panel.clone(), "sa:operator", Limits::default()).await;
	let mut op = open(op_addr, &signed_in(&op_app).await).await;
	let admin = signed_in(&admin_app).await;

	let model: Value = serde_json::from_str(include_str!("../../panel_core/tests/fixtures/pricing/valid/cleaning.json")).unwrap();
	let mut req = Request::builder()
		.method(Method::PUT)
		.uri("/api/v1/pricing/vifnet")
		.header(header::CONTENT_TYPE, "application/json")
		.header(header::COOKIE, admin.cookie().unwrap());
	if let Some(t) = admin.jar.get("sa_csrf") {
		req = req.header("x-sa-csrf", t);
	}
	let body = Body::from(json!({"model": model, "expected_updated_at": null}).to_string());
	let res = admin_app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
	assert_eq!(res.status(), StatusCode::OK);
	let saved: Value = serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();

	let told = op.text().await;
	assert_eq!(
		(told["type"].as_str(), told["topic"].as_str(), told["brand_id"].as_str()),
		(Some("changed"), Some("pricing"), Some("vifnet")),
		"{told}"
	);
	assert_eq!(told["at"], saved["updated_at"], "when it committed: the version the item names");
}

#[tokio::test]
async fn signing_out_closes_the_socket() {
	let db = TestDb::create().await;
	let (app, addr) = serve(panel(&db).await, "sa:admin", Limits::default()).await;
	let mut b = signed_in(&app).await;
	let mut ws = open(addr, &b).await;
	assert_eq!(b.send(&app, Method::POST, "/auth/logout").await.0, StatusCode::NO_CONTENT);
	assert_eq!(ws.next().await, Frame::Close(CLOSE_SESSION_ENDED));
}

#[tokio::test]
async fn a_new_sign_in_closes_the_socket_of_the_session_it_replaces() {
	let db = TestDb::create().await;
	let (app, addr) = serve(panel(&db).await, "sa:admin", Limits::default()).await;
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
	let (app, addr) = serve(panel(&db).await, "sa:admin", limits).await;
	let mut ws = open(addr, &signed_in(&app).await).await;
	sqlx::query("DELETE FROM sessions").execute(&db.pool().await).await.unwrap();
	assert_eq!(ws.next().await, Frame::Close(CLOSE_SESSION_ENDED));
}

/// The test's runtime has one thread: the ten changes are published before the socket's task
/// runs again, so it is behind by more than the bus of two holds.
#[tokio::test]
async fn a_reader_behind_is_told_to_resync() {
	let db = TestDb::create().await;
	let panel = panel(&db).await.with_live(Bus::new(2));
	let (app, addr) = serve(panel.clone(), "sa:admin", Limits::default()).await;
	let mut ws = open(addr, &signed_in(&app).await).await;
	for i in 0..10 {
		panel.bus().changed(Change {
			topic: Topic::Lead,
			brand: Some(BrandId::parse("aquafix").unwrap()),
			id: Some(format!("L-{i}")),
			user: None,
			at: Timestamp::now(),
		});
	}
	assert_eq!(ws.text().await, json!({"type": "resync"}));
	// What was left of the backlog is skipped: the next frame is what comes after.
	panel.bus().publish(Signal::Resync);
	assert_eq!(ws.text().await, json!({"type": "resync"}));
	panel.bus().changed(Change {
		topic: Topic::Places,
		brand: None,
		id: None,
		user: None,
		at: Timestamp::now(),
	});
	assert_eq!(ws.text().await["topic"], "places");
}

#[tokio::test]
async fn too_many_sockets_are_refused() {
	let db = TestDb::create().await;
	let limits = Limits {
		live: LiveLimits { per_user: 2, ..Default::default() },
		..Default::default()
	};
	let (app, addr) = serve(panel(&db).await, "sa:admin", limits).await;
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
	let (app, addr) = serve(panel(&db).await, "sa:admin", limits).await;
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

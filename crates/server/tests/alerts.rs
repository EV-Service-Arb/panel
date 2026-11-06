//! The alerts hub end to end on a throwaway database, against a mock Bot API and stand-ins for
//! sites and review_archive, all served in-process (axum on local ports — never the network),
//! and a fake concierge: linking and adding chats, the services' notes and the conductor's own
//! rules into the outbox for their audiences, the outbox's pace and retries, and the buttons
//! calling sites.
//!
//! Time is virtual: every pass is given its `now`, so the pace is checked to the millisecond
//! without sleeping.

use std::{
	collections::{BTreeMap, HashMap, HashSet, VecDeque},
	sync::{Arc, Mutex},
};

use axum::{
	Json, Router,
	extract::{FromRequest, Multipart, Path, Query, Request, State},
	http::{StatusCode, header},
	routing::{get, post},
};
use hmac::{Hmac, KeyInit, Mac};
use jiff::{SignedDuration, Timestamp};
use sa_alerts::{Origin, Rule};
use sa_auth::{PermissionSet, Principal, Service};
use sa_core::notify::{Locale, Rendered};
use sa_panel::{
	Panel, Refused,
	alerts::{Access, Directory, DirectoryError, Hub, Identity},
	import::Export,
	seal::DataKey,
	session::{RefreshError, Refresher, Tokens},
	telegram::{Account, Chat, Update},
	testing::{self as aliases, TestDb},
	tokens::User,
};
use sa_server::{
	forward::{Services, Upstreams},
	telegram::BotApi,
	testing::{Browser, Log, SEED, upstream},
};
use serde_json::{Value, json};
use uuid::Uuid;
use zeroize::Zeroizing;

const TOKEN: &str = "123456:test-bot-token";
const DATA_KEY: &str = "0f0e0d0c0b0a09080706050403020100000102030405060708090a0b0c0d0e0f";

/// The capture review_archive serves for a gone review.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really";

const ROYAT: &str = "ChIJLU7jZClu5kcR4PcOOO6p3I0";

const CHAMALIERES: &str = "ChIJD7fiBh9u5kcRYJSMaMOCCwQ";
fn t0() -> Timestamp {
	"2026-09-30T18:00:00Z".parse().unwrap()
}

fn at(ms: i64) -> Timestamp {
	t0() + SignedDuration::from_millis(ms)
}

// ── the mock Bot API ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Mock {
	/// Every call, in order: `(method, body)`; a photo's bytes as `photo_bytes`.
	calls: Vec<(String, Value)>,
	/// What the next sends answer, in order; then 200.
	send_script: VecDeque<(StatusCode, Value)>,
	next_message_id: i64,
	/// `getChat` by what it is asked with.
	chats: HashMap<String, Value>,
	/// `(chat, user)` pairs `getChatMember` answers as administrators.
	admins: HashSet<(i64, i64)>,
}

#[derive(Clone, Default)]
struct MockBot(Arc<Mutex<Mock>>);

impl MockBot {
	fn with<R>(&self, f: impl FnOnce(&mut Mock) -> R) -> R {
		f(&mut self.0.lock().unwrap())
	}

	fn calls(&self, method: &str) -> Vec<Value> {
		self.with(|m| m.calls.iter().filter(|(k, _)| k == method).map(|(_, b)| b.clone()).collect())
	}

	fn script(&self, status: u16, body: Value) {
		self.with(|m| m.send_script.push_back((StatusCode::from_u16(status).unwrap(), body)));
	}

	fn clear(&self) {
		self.with(|m| m.calls.clear());
	}

	/// A channel or group the bot sees, administered by `admin`.
	fn chat(&self, asked_as: &str, id: i64, kind: &str, forum: bool, admin: i64) {
		self.with(|m| {
			m.chats
				.insert(asked_as.to_owned(), json!({"id": id, "type": kind, "title": format!("chat {id}"), "is_forum": forum}));
			m.admins.insert((id, admin));
		});
	}
}

async fn bot_method(State(mock): State<MockBot>, Path((bot, method)): Path<(String, String)>, req: Request) -> (StatusCode, Json<Value>) {
	assert_eq!(bot, format!("bot{TOKEN}"), "the token is in the path");
	let multipart = req.headers().get(header::CONTENT_TYPE).is_some_and(|c| c.to_str().unwrap().starts_with("multipart/form-data"));
	let body = if multipart {
		let mut form = Multipart::from_request(req, &()).await.unwrap();
		let mut body = json!({});
		while let Some(field) = form.next_field().await.unwrap() {
			let name = field.name().unwrap().to_owned();
			if name == "photo" {
				body["photo_bytes"] = json!(field.bytes().await.unwrap().to_vec());
			} else {
				let text = field.text().await.unwrap();
				body[name] = serde_json::from_str(&text).unwrap_or(Value::String(text));
			}
		}
		body
	} else {
		let Json(body) = Json::<Value>::from_request(req, &()).await.unwrap();
		body
	};
	mock.with(|m| {
		m.calls.push((method.clone(), body.clone()));
		if method.starts_with("send")
			&& let Some((status, answer)) = m.send_script.pop_front()
		{
			return (status, Json(answer));
		}
		m.next_message_id += 1;
		let result = match method.as_str() {
			"sendMessage" | "sendPhoto" => json!({"message_id": m.next_message_id, "chat": {"id": body["chat_id"]}}),
			"getMe" => json!({"id": 1, "is_bot": true, "username": "evinvest_sa_bot"}),
			"getChat" => match m.chats.get(body["chat_id"].as_str().unwrap()) {
				Some(chat) => chat.clone(),
				None =>
					return (
						StatusCode::BAD_REQUEST,
						Json(json!({"ok": false, "error_code": 400, "description": "Bad Request: chat not found"})),
					),
			},
			"getChatMember" => {
				let admin = m.admins.contains(&(body["chat_id"].as_i64().unwrap(), body["user_id"].as_i64().unwrap()));
				json!({"status": if admin { "administrator" } else { "member" }})
			}
			_ => json!(true),
		};
		(StatusCode::OK, Json(json!({"ok": true, "result": result})))
	})
}

async fn mock_bot() -> (MockBot, String) {
	let mock = MockBot::default();
	let app = Router::new().route("/{bot}/{method}", post(bot_method)).with_state(mock.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let base = format!("http://{}", listener.local_addr().unwrap());
	// Lives as long as the test's runtime.
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(mock, base)
}

// ── the stand-in services ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Feed {
	/// Every note ever made, its `seq` its place + 1.
	notes: Vec<Value>,
	/// Answers its notes 404, as a service not cut over.
	unpublished: bool,
	/// `sources/received`.
	received: BTreeMap<String, Timestamp>,
	/// What the next presses answer, in order; then 201.
	presses: VecDeque<u16>,
}

#[derive(Clone, Default)]
struct FakeService(Arc<Mutex<Feed>>);

impl FakeService {
	fn with<R>(&self, f: impl FnOnce(&mut Feed) -> R) -> R {
		f(&mut self.0.lock().unwrap())
	}
}

async fn serve(service: FakeService) -> (String, Log) {
	let notes = service.clone();
	let received = service.clone();
	let routes = Router::new()
		.route(
			"/api/conductor/notifications",
			get(move |Query(q): Query<HashMap<String, i64>>| {
				let service = notes.clone();
				async move {
					let after = q["after"];
					service.with(|s| {
						if s.unpublished {
							return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"})));
						}
						let page: Vec<Value> = s.notes.iter().filter(|n| n["seq"].as_i64().unwrap() > after).take(q["limit"] as usize).cloned().collect();
						let next = page.last().map_or(after, |n| n["seq"].as_i64().unwrap());
						(StatusCode::OK, Json(json!({"notes": page, "next": next})))
					})
				}
			}),
		)
		.route(
			"/api/conductor/sources/received",
			get(move || {
				let service = received.clone();
				async move { Json(service.with(|s| json!(s.received))) }
			}),
		)
		.route("/api/conductor/captures/{sha}", get(|| async { PNG }))
		.route(
			"/api/v1/leads/{brand}/{lead}/{action}",
			post(move || {
				let service = service.clone();
				async move { StatusCode::from_u16(service.with(|s| s.presses.pop_front().unwrap_or(201))).unwrap() }
			}),
		);
	upstream(routes).await
}

// ── the fake concierge ───────────────────────────────────────────────────────────────────

/// access token → who `GetMe` says holds it, or a refusal; and how many `GetMe` to refuse
/// first, whatever the token.
#[derive(Clone, Default)]
struct FakeConcierge(Arc<Mutex<HashMap<String, Result<Identity, ()>>>>, Arc<Mutex<u32>>);

impl Refresher for FakeConcierge {
	async fn refresh(&self, _: &str) -> Result<Tokens, RefreshError> {
		// The tokens the tests open sessions with never come near expiry.
		Err(RefreshError::Rejected)
	}
}

impl Directory for FakeConcierge {
	async fn me(&self, access: &str) -> Result<Identity, DirectoryError> {
		{
			let mut refuse = self.1.lock().unwrap();
			if *refuse > 0 {
				*refuse -= 1;
				return Err(DirectoryError::Refused);
			}
		}
		match self.0.lock().unwrap().get(access).cloned() {
			Some(Ok(id)) => Ok(id),
			Some(Err(())) | None => Err(DirectoryError::Refused),
		}
	}
}

struct Setup {
	_db: TestDb,
	db: sqlx::SqlitePool,
	panel: Panel,
	mock: MockBot,
	concierge: FakeConcierge,
	sites: FakeService,
	archive: FakeService,
	/// What sites was sent.
	seen: Log,
	/// What review_archive was sent.
	archive_seen: Log,
	n: Hub<BotApi, FakeConcierge, Services>,
}
impl Setup {
	/// A user linked to `chat` at `now`, as the Alerts page and `/start` do it.
	async fn link(&self, user: Uuid, permissions: PermissionSet, chat: i64, now: Timestamp) {
		let token = self.panel.telegram_link_token(user, &permissions, &format!("user-{chat}"), now).await.unwrap();
		self.n
			.handle(
				Update::Start {
					chat: private(chat),
					payload: Some(token.to_string()),
					from: Account::default(),
				},
				now,
			)
			.await
			.unwrap();
	}

	/// A live session for `user`, whose access token concierge answers `GetMe` for.
	async fn session(&self, user: Uuid, permissions: PermissionSet, name: &str) {
		self.session_until(user, permissions, name, t0() + SignedDuration::from_hours(24 * 30)).await;
	}

	/// [`Self::session`], its access token expiring at `access_until`.
	async fn session_until(&self, user: Uuid, permissions: PermissionSet, name: &str, access_until: Timestamp) {
		let access = format!("access-{user}");
		let far = t0() + SignedDuration::from_hours(24 * 30);
		let tokens = Tokens {
			access: Zeroizing::new(access.clone()),
			access_expires_at: access_until,
			refresh: Zeroizing::new(format!("refresh-{user}")),
			refresh_expires_at: far,
		};
		self.panel.open_session(user, &tokens, t0()).await.unwrap();
		self.concierge.0.lock().unwrap().insert(
			access,
			Ok(Identity {
				user_id: user,
				email: format!("{}@example.com", name.to_lowercase()),
				email_verified: true,
				preferred_name: name.to_owned(),
				permissions,
			}),
		);
	}

	/// A note `service` made at `now`; its event id.
	fn note_of(&self, service: &FakeService, rule: &str, now: Timestamp, text: Value, text_pii: Value, extra: Value) -> Uuid {
		let event = Uuid::now_v7();
		service.with(|s| {
			let seq = s.notes.len() as i64 + 1;
			let mut note = json!({"seq": seq, "rule": rule, "event_id": event, "created_at": now, "text": text, "text_pii": text_pii, "skip_user": null, "buttons": []});
			for (k, v) in extra.as_object().unwrap() {
				note[k] = v.clone();
			}
			s.notes.push(note);
		});
		event
	}

	/// A note sites made at `now`; its event id.
	fn note(&self, rule: &str, now: Timestamp, text: Value, text_pii: Value, skip_user: Option<Uuid>, buttons: Value) -> Uuid {
		self.note_of(&self.sites, rule, now, text, text_pii, json!({"skip_user": skip_user, "buttons": buttons}))
	}

	/// A note review_archive made at `now` about `gmbs`.
	fn review(&self, rule: &str, now: Timestamp, gmbs: &[i64], photo: Option<&str>) {
		let mut extra = json!({"gmbs": gmbs});
		if let Some(photo) = photo {
			extra["photo"] = json!(photo);
		}
		self.note_of(&self.archive, rule, now, text(&format!("{rule} · Plombier Royat")), Value::Null, extra);
	}

	/// A new lead `lead` noted at `now`: its brand and place for everyone, its need and phone for
	/// whoever sees PII, and its two buttons.
	fn lead(&self, lead: &str, now: Timestamp) {
		self.note(
			"new_lead",
			now,
			text("Новая заявка\naquafix · paris-11"),
			text("Новая заявка\naquafix · paris-11\nНужно: a leaking tap\nТелефон: +33 6 00 00 00 00"),
			None,
			lead_buttons(lead),
		);
	}

	/// One pass of the hub over the conductor's own rules and both feeds: how many were queued.
	async fn fan_out(&self, n: &Hub<BotApi, FakeConcierge, Services>, now: Timestamp) -> eyre::Result<usize> {
		let own = n.fan_out_own(now).await?;
		let sites = n.take_feed(Origin::Sites, now).await?.unwrap();
		let archive = n.take_feed(Origin::ReviewArchive, now).await?.unwrap_or(0);
		Ok(own + sites + archive)
	}

	async fn outbox(&self) -> Vec<(i64, String, String, i32)> {
		sqlx::query_as("SELECT chat_id, rule, state, attempts FROM telegram_outbox ORDER BY id")
			.fetch_all(&self.db)
			.await
			.unwrap()
	}

	/// The user's private destination, if linked.
	async fn private_of(&self, user: Uuid) -> Option<sa_panel::alerts::Destination> {
		self.panel
			.alerts(user, &aliases::admin())
			.await
			.unwrap()
			.destinations
			.into_iter()
			.find(|d| d.kind == sa_panel::telegram::Kind::Private)
	}

	async fn linked(&self, user: Uuid) -> bool {
		self.private_of(user).await.is_some()
	}

	/// Turns rules on or off at the user's private chat.
	async fn set_rules(&self, user: Uuid, permissions: &PermissionSet, rules: &[(Rule, bool)]) -> Result<(), Refused> {
		let destination = self.private_of(user).await.unwrap().id;
		self.panel.alerts_set_rules(user, permissions, destination, rules).await.unwrap()
	}

	async fn unlink(&self, user: Uuid) -> bool {
		match self.private_of(user).await {
			Some(d) => self.panel.alerts_remove(user, d.id).await.unwrap(),
			None => false,
		}
	}

	/// The presses sites was sent.
	fn presses(&self) -> Vec<sa_server::testing::Seen> {
		self.seen.lock().unwrap().iter().filter(|s| s.path_and_query.starts_with("/api/v1/")).cloned().collect()
	}

	/// A user known to the conductor, with one gmail and a GMB under it: `(gmail, gmb)`.
	async fn owner(&self, user: Uuid, gmail: &str, place: &str) -> (i64, i64) {
		self.panel
			.user_seen(
				&User {
					user_id: user,
					email: format!("{user}@example.com"),
					name: "Owner".into(),
				},
				t0(),
			)
			.await
			.unwrap();
		let gmail = self.panel.add_gmail(user, gmail, t0()).await.unwrap().unwrap();
		let gmb = self.panel.add_gmb(Some(user), gmail.id, place, t0()).await.unwrap().unwrap();
		(gmail.id, gmb.id)
	}
}

async fn setup() -> Setup {
	let db = TestDb::create().await;
	let panel = aliases::panel_with(db.store().await, DataKey::from_hex(DATA_KEY).unwrap());
	let (mock, base) = mock_bot().await;
	let (sites, archive) = (FakeService::default(), FakeService::default());
	let (sites_base, seen) = serve(sites.clone()).await;
	let (archive_base, archive_seen) = serve(archive.clone()).await;
	let concierge = FakeConcierge::default();
	let n = Hub {
		panel: panel.clone(),
		bot: BotApi::try_new(&base, TOKEN).unwrap(),
		concierge: concierge.clone(),
		feeds: Services::new(Upstreams {
			review_archive: archive_base.parse().unwrap(),
			playbook: sites_base.parse().unwrap(),
			sites: sites_base.parse().unwrap(),
			signer: SEED.parse().unwrap(),
		}),
		locale: Locale::Ru,
		cabinet: cabinet(),
	};
	panel
		.import_review_archive(Export {
			people: vec![],
			gmails: vec![],
			gmbs: vec![],
			ledger: vec![],
			kickbacks: vec![],
		})
		.await
		.unwrap();
	Setup {
		db: db.pool().await,
		_db: db,
		panel,
		mock,
		concierge,
		sites,
		archive,
		seen,
		archive_seen,
		n,
	}
}

fn private(id: i64) -> Chat {
	Chat { id, private: true }
}

/// A lead's two buttons, as sites describes them.
fn lead_buttons(lead: &str) -> Value {
	json!([
		{"id": "take", "label": "Взял", "pressed": "Взял",
		 "call": {"method": "POST", "path": format!("/api/v1/leads/aquafix/{lead}/stage"), "body": {"stage": "contacted"}}, "leaves": []},
		{"id": "no_answer", "label": "Не дозвонился", "pressed": "Не дозвонился",
		 "call": {"method": "POST", "path": format!("/api/v1/leads/aquafix/{lead}/no_answer"), "body": null}, "leaves": ["take"]},
	])
}

fn text(text: &str) -> Value {
	json!({"text": text, "entities": []})
}

fn texts(calls: &[Value]) -> Vec<String> {
	calls.iter().map(|c| c["text"].as_str().unwrap_or_default().to_owned()).collect()
}

fn cabinet() -> url::Url {
	"https://evinvest.test".parse().unwrap()
}

// ── linking ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_link_token_is_single_use_private_and_expires() {
	let s = setup().await;
	let (alice, bob) = (Uuid::now_v7(), Uuid::now_v7());

	let token = s.panel.telegram_link_token(alice, &aliases::operator(), "Alice", t0()).await.unwrap();
	assert!(token.len() >= 43, "256 bits, base64url");
	let raw: Option<(Vec<u8>,)> = sqlx::query_as("SELECT token_hash FROM telegram_link_tokens").fetch_optional(&s.db).await.unwrap();
	assert_ne!(raw.unwrap().0, token.as_bytes(), "only a hash is stored");

	// From a group: ignored whatever it carries, and the token is not spent.
	let group = Update::Start {
		chat: Chat { id: -100, private: false },
		payload: Some(token.to_string()),
		from: Account::default(),
	};
	s.n.handle(group, t0()).await.unwrap();
	assert!(!s.linked(alice).await);
	assert!(s.mock.calls("sendMessage").is_empty(), "no answer in a group");

	let start = |chat, token: &str| Update::Start {
		chat: private(chat),
		payload: Some(token.to_owned()),
		from: Account::default(),
	};
	s.n.handle(start(7, &token), t0() + SignedDuration::from_mins(9)).await.unwrap();
	assert!(s.linked(alice).await);
	assert!(texts(&s.mock.calls("sendMessage"))[0].starts_with("Telegram подключён"));

	// Spent: a second /start with it links nothing, even from another chat.
	s.mock.clear();
	s.n.handle(start(8, &token), t0() + SignedDuration::from_mins(9)).await.unwrap();
	assert!(texts(&s.mock.calls("sendMessage"))[0].starts_with("Ссылка недействительна"));
	assert_eq!(s.private_of(alice).await.unwrap().chat_id, 7);

	// Expired: past ten minutes it opens nothing.
	let late = s.panel.telegram_link_token(bob, &aliases::operator(), "Bob", t0()).await.unwrap();
	s.n.handle(start(9, &late), t0() + SignedDuration::from_mins(10)).await.unwrap();
	assert!(!s.linked(bob).await);
	let forged = start(9, "A".repeat(43).as_str());
	s.n.handle(forged, t0()).await.unwrap();
	assert!(!s.linked(bob).await);

	// /stop unlinks; the page's remove does too.
	s.n.handle(Update::Stop { chat: private(7) }, t0()).await.unwrap();
	assert!(!s.linked(alice).await);
	s.link(alice, aliases::operator(), 7, t0()).await;
	assert!(s.unlink(alice).await);
	assert!(!s.unlink(alice).await);
}

#[tokio::test]
async fn linking_names_both_accounts_and_never_takes_a_chat_over() {
	let s = setup().await;
	let (alice, bob) = (Uuid::now_v7(), Uuid::now_v7());
	let start = |chat, token: &str, username: &str| Update::Start {
		chat: private(chat),
		payload: Some(token.to_owned()),
		from: Account {
			username: Some(username.to_owned()),
			first_name: Some("A".to_owned()),
		},
	};

	// A second token replaces the first.
	let first = s.panel.telegram_link_token(alice, &aliases::operator(), "Alice Panel", t0()).await.unwrap();
	let second = s.panel.telegram_link_token(alice, &aliases::operator(), "Alice Panel", t0()).await.unwrap();
	s.n.handle(start(1, &first, "alice_tg"), t0()).await.unwrap();
	assert!(!s.linked(alice).await, "the older token is gone");
	s.n.handle(start(1, &second, "alice_tg"), t0() + SignedDuration::from_mins(9)).await.unwrap();
	let replies = texts(&s.mock.calls("sendMessage"));
	assert!(replies.last().unwrap().contains("аккаунт: Alice Panel"), "{replies:?}");
	assert_eq!(s.private_of(alice).await.unwrap().title.as_deref(), Some("@alice_tg"));
	let checked: String = sqlx::query_scalar("SELECT datetime(confirmed_at / 1000000, 'unixepoch') FROM alert_access WHERE user_id = $1")
		.bind(alice)
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert!(checked.starts_with("2026-09-30 18:00:00"), "confirmed as of the token's issue, not the /start: {checked}");

	// Bob's link opened in Alice's chat: refused, and the token not spent.
	let bobs = s.panel.telegram_link_token(bob, &aliases::operator(), "Bob", t0()).await.unwrap();
	s.n.handle(start(1, &bobs, "alice_tg"), t0()).await.unwrap();
	assert!(texts(&s.mock.calls("sendMessage")).last().unwrap().starts_with("Этот чат привязан к другому аккаунту"));
	assert!(!s.linked(bob).await);
	assert_eq!(s.private_of(alice).await.unwrap().chat_id, 1, "Alice keeps her chat");
	s.n.handle(start(2, &bobs, "bob_tg"), t0()).await.unwrap();
	assert!(s.linked(bob).await, "the token still links Bob's own chat");

	// Alice linking another chat keeps her rules there.
	s.set_rules(alice, &aliases::operator(), &[(Rule::NewLead, false)]).await.unwrap();
	s.link(alice, aliases::operator(), 3, t0()).await;
	let moved = s.private_of(alice).await.unwrap();
	assert_eq!((moved.chat_id, moved.chosen), (3, vec![(Rule::NewLead, false)]));
}

// ── sites' notes ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_note_goes_to_linked_users_with_access_only_its_pii_to_who_sees_it() {
	let s = setup().await;
	let operator = Uuid::now_v7(); // linked, on by default, sees PII: gets it whole
	let no_pii = Uuid::now_v7(); // works the leads without seeing PII: gets it without
	let muted = Uuid::now_v7(); // turned the rule off
	let revoked = Uuid::now_v7(); // concierge said no since
	let stale = Uuid::now_v7(); // not confirmed for over an hour
	let worker: PermissionSet = ["sa:sites:work:read", "sa:sites:work:leads:edit"].into_iter().collect();
	s.link(operator, aliases::operator(), 1, t0()).await;
	s.link(muted, aliases::admin(), 2, t0()).await;
	s.link(revoked, aliases::operator(), 3, t0()).await;
	s.link(stale, aliases::operator(), 4, t0() - SignedDuration::from_mins(61)).await;
	s.link(no_pii, worker, 5, t0()).await;
	s.set_rules(muted, &aliases::admin(), &[(Rule::NewLead, false)]).await.unwrap();
	s.panel.alerts_access_seen(revoked, &PermissionSet::from_iter(Vec::<String>::new()), "Rex", t0()).await.unwrap();
	assert!(
		matches!(s.set_rules(operator, &aliases::operator(), &[(Rule::PaymentReceived, true)]).await, Err(Refused::Invalid(_))),
		"payments are the admins'"
	);
	assert!(
		matches!(s.set_rules(operator, &aliases::operator(), &[(Rule::ReviewGone, true)]).await, Err(Refused::Invalid(_))),
		"reviews are their GMBs' owners'"
	);
	s.mock.clear();

	s.lead("L-1", t0());
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 2);
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 0, "told once");
	assert_eq!(
		s.outbox().await,
		[(1, "new_lead".to_owned(), "pending".to_owned(), 0), (5, "new_lead".to_owned(), "pending".to_owned(), 0)]
	);
	let sealed: Vec<Vec<u8>> = sqlx::query_scalar("SELECT text_sealed FROM telegram_outbox").fetch_all(&s.db).await.unwrap();
	assert!(sealed.iter().all(|t| !String::from_utf8_lossy(t).contains("+33")), "the phone is sealed in the outbox");

	assert_eq!(s.n.deliver(t0()).await.unwrap().sent, 2);
	let sent = s.mock.calls("sendMessage");
	let by_chat: HashMap<i64, &str> = sent.iter().map(|m| (m["chat_id"].as_i64().unwrap(), m["text"].as_str().unwrap())).collect();
	assert_eq!(
		by_chat[&1], "Новая заявка\naquafix · paris-11\nНужно: a leaking tap\nТелефон: +33 6 00 00 00 00",
		"PII for whoever sees it"
	);
	assert_eq!(by_chat[&5], "Новая заявка\naquafix · paris-11", "and not for whoever does not");
	let labels: Vec<&str> = sent[0]["reply_markup"]["inline_keyboard"]
		.as_array()
		.unwrap()
		.iter()
		.map(|row| row[0]["text"].as_str().unwrap())
		.collect();
	assert_eq!(labels, ["Взял", "Не дозвонился"]);
	let sealed: Vec<Option<Vec<u8>>> = sqlx::query_scalar("SELECT text_sealed FROM telegram_outbox").fetch_all(&s.db).await.unwrap();
	assert!(sealed.iter().all(Option::is_none), "the text is dropped once sent");

	// A payment goes to the admins who asked for it.
	s.panel.alerts_access_seen(muted, &aliases::admin(), "Mia", t0()).await.unwrap();
	s.set_rules(muted, &aliases::admin(), &[(Rule::PaymentReceived, true)]).await.unwrap();
	s.note("payment_received", t0(), text("Оплата получена\naquafix · L-1"), Value::Null, None, json!([]));
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 1);
	s.n.deliver(at(2_000)).await.unwrap();
	let last = s.mock.calls("sendMessage").pop().unwrap();
	assert_eq!((last["chat_id"].as_i64(), last["text"].as_str()), (Some(2), Some("Оплата получена\naquafix · L-1")));
}

#[tokio::test]
async fn a_note_skips_whoever_did_what_it_tells_of() {
	let s = setup().await;
	let (typist, colleague) = (Uuid::now_v7(), Uuid::now_v7());
	s.link(typist, aliases::operator(), 1, t0()).await;
	s.link(colleague, aliases::operator(), 2, t0()).await;
	s.note("new_lead", t0(), text("Новая заявка"), Value::Null, Some(typist), lead_buttons("p-1"));
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 1);
	assert_eq!(s.outbox().await, [(2, "new_lead".to_owned(), "pending".to_owned(), 0)], "the colleague only");
}

/// A page of notes is in the outbox before the cursor moves past it, in one transaction; a note
/// the hub cannot send stops it there, loudly, with nothing of the page queued.
#[tokio::test]
async fn the_cursor_moves_only_once_the_notes_are_in_the_outbox() {
	let s = setup().await;
	s.link(Uuid::now_v7(), aliases::operator(), 1, t0()).await;
	s.lead("L-1", t0());
	s.note("review_low", t0(), text("3★"), Value::Null, None, json!([]));
	s.lead("L-2", t0());

	let e = s.fan_out(&s.n, t0()).await.unwrap_err();
	assert!(format!("{e:#}").contains("review_low"), "{e:#}");
	assert_eq!(s.outbox().await, [], "nothing of the page");
	assert_eq!(s.panel.alerts_feed_after(Origin::Sites).await.unwrap(), 0);
	assert!(s.fan_out(&s.n, t0()).await.is_err(), "and stopped there, pass after pass");

	// A rule of another service's: not sites' to tell.
	s.sites.with(|sites| {
		sites.notes[1]["rule"] = json!("review_gone");
		sites.notes[1]["gmbs"] = json!([1]);
	});
	let e = s.fan_out(&s.n, t0()).await.unwrap_err();
	assert!(format!("{e:#}").contains("belongs to review_archive"), "{e:#}");

	s.sites.with(|sites| {
		sites.notes[1]["rule"] = json!("contact_overdue");
		sites.notes[1].as_object_mut().unwrap().remove("gmbs");
	});
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 3);
	assert_eq!(s.panel.alerts_feed_after(Origin::Sites).await.unwrap(), 3);
	let asked: Vec<String> = s
		.seen
		.lock()
		.unwrap()
		.iter()
		.filter(|r| r.path_and_query.starts_with("/api/conductor/notifications"))
		.map(|r| {
			assert!(matches!(r.assertion(Service::Sites, None).principal, Principal::Conductor));
			r.path_and_query.clone()
		})
		.collect();
	assert_eq!(asked.last().unwrap(), "/api/conductor/notifications?after=0&limit=500");
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 0);
	assert_eq!(s.seen.lock().unwrap().last().unwrap().path_and_query, "/api/conductor/notifications?after=3&limit=500");

	s.note(
		"new_lead",
		t0(),
		text("x"),
		Value::Null,
		None,
		json!([{"id": "t", "label": "T", "pressed": "T", "call": {"method": "POST", "path": "/api/v1/x", "body": null}, "leaves": []}]),
	);
	assert!(s.fan_out(&s.n, t0()).await.is_err(), "a button id that is no button id");
	s.sites
		.with(|sites| sites.notes[3]["buttons"] = json!([{"id": "go", "label": "Go", "pressed": "Go", "call": {"method": "POST", "path": "/api/v1/../admin", "body": null}, "leaves": []}]));
	assert!(s.fan_out(&s.n, t0()).await.is_err(), "a call out of sites' API");
	assert_eq!(s.panel.alerts_feed_after(Origin::Sites).await.unwrap(), 3);
}

#[tokio::test]
async fn a_silent_source_is_told_to_admins_once_a_day() {
	let s = setup().await;
	let admin = Uuid::now_v7();
	s.panel
		.add_source(
			"aquafix-site",
			sa_auth::SourceKind::Site,
			[sa_core::ids::BrandId::parse("aquafix").unwrap()].into_iter().collect(),
			Some(sa_core::ids::SiteOrigin::parse("https://aquafix.top").unwrap()),
		)
		.await
		.unwrap()
		.unwrap();
	s.panel
		.add_source(
			"vifnet-site",
			sa_auth::SourceKind::Site,
			[sa_core::ids::BrandId::parse("vifnet").unwrap()].into_iter().collect(),
			Some(sa_core::ids::SiteOrigin::parse("https://vifnet.site").unwrap()),
		)
		.await
		.unwrap()
		.unwrap();
	let added = t0() - SignedDuration::from_hours(25);
	// Back-dates the sources, which the schema refuses to anything but a test: the trigger goes
	// for this throwaway database alone.
	sqlx::raw_sql("DROP TRIGGER sources_only_revoked").execute(&s.db).await.unwrap();
	sqlx::query("UPDATE sources SET created_at = $1").bind(added.as_microsecond()).execute(&s.db).await.unwrap();
	// vifnet's site sent an event an hour ago, as sites says.
	s.sites.with(|sites| sites.received.insert("vifnet-site".into(), t0() - SignedDuration::from_hours(1)));
	s.link(admin, aliases::admin(), 5, t0()).await;
	s.set_rules(admin, &aliases::admin(), &[(Rule::SourceSilent, true)]).await.unwrap();
	s.mock.clear();
	let n = Hub { locale: Locale::En, ..s.n.clone() };
	assert_eq!(s.fan_out(&n, t0()).await.unwrap(), 1, "aquafix's, not vifnet's");
	let hour = SignedDuration::from_hours(1);
	// Silent for 25 h at t0: told. At 47 h, still the same day of silence; at 48 h, the next.
	s.panel.alerts_access_seen(admin, &aliases::admin(), "Ann", t0() + hour * 22).await.unwrap();
	assert_eq!(s.fan_out(&n, t0() + hour * 22).await.unwrap(), 0, "not twice for one day of silence");
	s.panel.alerts_access_seen(admin, &aliases::admin(), "Ann", t0() + hour * 23).await.unwrap();
	assert_eq!(s.fan_out(&n, t0() + hour * 23).await.unwrap(), 1, "the next day of it");
	n.deliver(t0() + hour * 23).await.unwrap();
	let told = texts(&s.mock.calls("sendMessage"));
	assert!(told.iter().all(|t| t.starts_with("Source silent for over a day\naquafix-site (site): no events yet")), "{told:?}");
	let asked = s.seen.lock().unwrap().iter().find(|r| r.path_and_query == "/api/conductor/sources/received").cloned().unwrap();
	assert!(matches!(asked.assertion(Service::Sites, None).principal, Principal::Conductor));
}

// ── review_archive's notes ───────────────────────────────────────────────────────────────

/// A review's note goes to the destinations of whoever owns a GMB it names — the GMB on, it and
/// its gmail not removed, the destination's gmail theirs if it has one — whatever their
/// permissions, and on their own switches: only `review_gone` is on by default.
#[tokio::test]
async fn a_review_goes_to_its_gmbs_owners_only() {
	let s = setup().await;
	let nobody: PermissionSet = std::iter::empty::<&str>().collect();
	let (owner, scoped, other, switched_off) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
	let (_, royat) = s.owner(owner, "a@gmail.com", ROYAT).await;
	let (_, scoped_gmb) = s.owner(scoped, "b@gmail.com", ROYAT).await;
	let (_, chamalieres) = s.owner(other, "c@gmail.com", CHAMALIERES).await;
	let (_, off) = s.owner(switched_off, "d@gmail.com", ROYAT).await;
	for (user, chat) in [(owner, 1), (scoped, 2), (other, 3), (switched_off, 4)] {
		s.link(user, nobody.clone(), chat, t0()).await;
	}
	s.panel.switch_gmbs(switched_off, &[off], false).await.unwrap().unwrap();
	// Scoped to a gmail that is not the GMB's: told nothing of it.
	let other_gmail = s.panel.add_gmail(scoped, "e@gmail.com", t0()).await.unwrap().unwrap();
	let scoped_dest = s.private_of(scoped).await.unwrap().id;
	s.panel.alerts_set_gmail(scoped, scoped_dest, Some(other_gmail.id)).await.unwrap().unwrap();
	assert!(
		matches!(s.panel.alerts_set_gmail(owner, scoped_dest, None).await.unwrap(), Err(Refused::NotFound)),
		"another's destination"
	);
	let alerts = s.panel.alerts(owner, &nobody).await.unwrap();
	assert!(alerts.rules.contains(&Rule::ReviewGone) && !alerts.rules.contains(&Rule::NewLead), "{:?}", alerts.rules);
	s.mock.clear();

	s.review("review_new", t0(), &[royat, scoped_gmb, chamalieres, off], None);
	s.review("review_gone", t0(), &[royat, scoped_gmb, off], None);
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 1, "review_new is off by default; review_gone to the owner alone");
	assert_eq!(s.outbox().await, [(1, "review_gone".to_owned(), "pending".to_owned(), 0)]);
	assert_eq!(s.panel.alerts_feed_after(Origin::ReviewArchive).await.unwrap(), 2);
	let asked = s.archive_seen.lock().unwrap().last().cloned().unwrap();
	assert!(matches!(asked.assertion(Service::ReviewArchive, None).principal, Principal::Conductor));

	// Back to every gmail, the other owner's switch on: both told now.
	s.panel.alerts_set_gmail(scoped, scoped_dest, None).await.unwrap().unwrap();
	s.set_rules(other, &nobody, &[(Rule::ReviewNew, true)]).await.unwrap();
	s.review("review_new", t0(), &[chamalieres], None);
	s.review("review_gone", t0(), &[scoped_gmb], None);
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 2);
	assert_eq!(s.private_of(scoped).await.unwrap().gmail_id, None);

	// A gmail removed takes its GMBs out of every audience.
	s.panel.remove_gmail(owner, s.panel.gmails(owner).await.unwrap()[0].0.id, t0()).await.unwrap().unwrap();
	s.review("review_gone", t0(), &[royat], None);
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 0);
}

/// A gone review's capture goes as a photo, fetched from review_archive under the conductor's
/// assertion at the send; the note's text its caption.
#[tokio::test]
async fn a_gone_review_carries_its_capture() {
	let s = setup().await;
	let owner = Uuid::now_v7();
	let (_, royat) = s.owner(owner, "a@gmail.com", ROYAT).await;
	s.link(owner, PermissionSet::from_iter(Vec::<String>::new()), 1, t0()).await;
	s.mock.clear();
	s.review("review_gone", t0(), &[royat], Some("/api/conductor/captures/abc.png"));
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 1);
	assert_eq!(s.n.deliver(t0()).await.unwrap().sent, 1);
	let sent = s.mock.calls("sendPhoto");
	assert_eq!(sent.len(), 1);
	assert_eq!(sent[0]["caption"], "review_gone · Plombier Royat");
	assert_eq!(sent[0]["photo_bytes"], json!(PNG.to_vec()));
	let fetched = s
		.archive_seen
		.lock()
		.unwrap()
		.iter()
		.find(|r| r.path_and_query == "/api/conductor/captures/abc.png")
		.cloned()
		.unwrap();
	assert!(matches!(fetched.assertion(Service::ReviewArchive, None).principal, Principal::Conductor));
}

/// A service not cut over answers 404: nothing taken, its cursor where it was, the others go on.
#[tokio::test]
async fn a_feed_not_published_yet_is_waited_for() {
	let s = setup().await;
	s.archive.with(|a| a.unpublished = true);
	s.link(Uuid::now_v7(), aliases::operator(), 1, t0()).await;
	s.lead("L-1", t0());
	assert_eq!(s.n.take_feed(Origin::ReviewArchive, t0()).await.unwrap(), None);
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 1, "sites' lead all the same");
	assert_eq!(s.panel.alerts_feed_after(Origin::ReviewArchive).await.unwrap(), 0);
}

// ── channels, groups, topics ─────────────────────────────────────────────────────────────

/// A chat is added only once the bot sees it, the user administers it (asked with their own
/// linked account) and a test went out there; it gets the non-PII text, no buttons, at its own
/// pace.
#[tokio::test]
async fn a_channel_is_added_by_its_admin_and_gets_no_pii() {
	let s = setup().await;
	let user = Uuid::now_v7();
	let operator = aliases::operator();
	let add = |chat: &'static str| s.n.add_chat(user, &operator, "Olga", chat, None, t0());

	assert!(matches!(add("@leads").await.unwrap(), Err(Refused::Invalid(m)) if m.contains("link your own Telegram first")));
	s.link(user, operator.clone(), 77, t0()).await;
	assert!(matches!(add("not a chat!").await.unwrap(), Err(Refused::Invalid(_))));
	assert!(
		matches!(add("@leads").await.unwrap(), Err(Refused::Invalid(m)) if m.contains("cannot see")),
		"the bot is not there"
	);
	s.mock.chat("@leads", -1001, "channel", false, 5);
	assert!(
		matches!(add("@leads").await.unwrap(), Err(Refused::Invalid(m)) if m.contains("does not administer")),
		"77 is not an admin"
	);
	s.mock.chat("@leads", -1001, "channel", false, 77);
	s.mock
		.script(403, json!({"ok": false, "error_code": 403, "description": "Forbidden: bot is not a member of the channel chat"}));
	assert!(
		matches!(add("@leads").await.unwrap(), Err(Refused::Invalid(m)) if m.contains("cannot post")),
		"the test did not go out"
	);
	assert_eq!(s.panel.alerts(user, &operator).await.unwrap().destinations.len(), 1, "nothing stored for a refusal");
	s.mock.clear();

	let channel = add("@leads").await.unwrap().unwrap();
	assert_eq!(texts(&s.mock.calls("sendMessage")), ["Сюда будут приходить оповещения панели Service-Arb."]);
	assert!(matches!(add("@leads").await.unwrap(), Err(Refused::Conflict(_))), "once");
	s.mock.chat("-1002", -1002, "supergroup", true, 77);
	let topic = add("-1002/5").await.unwrap().unwrap();
	assert_eq!(s.mock.calls("sendMessage").last().unwrap()["message_thread_id"], 5, "the test went to the topic");
	s.mock.clear();

	s.lead("L-1", t0());
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 3);
	assert_eq!(s.n.deliver(t0()).await.unwrap().sent, 3);
	let sent = s.mock.calls("sendMessage");
	let to = |chat: i64| sent.iter().find(|m| m["chat_id"] == chat).unwrap();
	assert_eq!(to(-1001)["text"], "Новая заявка\naquafix · paris-11", "a channel never gets PII");
	assert!(to(-1001).get("reply_markup").is_none(), "nor buttons");
	assert_eq!(to(-1002)["message_thread_id"], 5);
	assert!(to(77)["text"].as_str().unwrap().contains("+33"), "the user's own chat does");

	// A channel's pace: twenty a minute, not one a second.
	s.mock.clear();
	s.lead("L-2", at(1_000));
	assert_eq!(s.fan_out(&s.n, at(1_000)).await.unwrap(), 3);
	assert_eq!(s.n.deliver(at(1_000)).await.unwrap().sent, 1, "the private chat only");
	assert_eq!(s.n.deliver(at(2_999)).await.unwrap().sent, 0);
	assert_eq!(s.n.deliver(at(3_000)).await.unwrap().sent, 2);
	let dests = s.panel.alerts(user, &operator).await.unwrap().destinations;
	assert!(dests.iter().any(|d| d.id == channel) && dests.iter().any(|d| d.id == topic && d.topic_id == Some(5)));
}

/// A chat the bot was removed from goes dead; a test that arrives brings it back.
#[tokio::test]
async fn a_test_revives_a_dead_chat() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 77, t0()).await;
	s.mock.chat("@leads", -1001, "channel", false, 77);
	let channel = s.n.add_chat(user, &aliases::operator(), "Olga", "@leads", None, t0()).await.unwrap().unwrap();
	s.set_rules(user, &aliases::operator(), &[(Rule::NewLead, false)]).await.unwrap();
	s.lead("L-1", t0());
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 1, "the channel only");
	s.mock
		.script(403, json!({"ok": false, "error_code": 403, "description": "Forbidden: bot was kicked from the channel chat"}));
	assert_eq!(s.n.deliver(t0()).await.unwrap().dead, 1);
	let dead = || async {
		s.panel
			.alerts(user, &aliases::operator())
			.await
			.unwrap()
			.destinations
			.into_iter()
			.find(|d| d.id == channel)
			.unwrap()
			.dead
	};
	assert!(dead().await.is_some());

	s.mock.script(403, json!({"ok": false, "error_code": 403, "description": "Forbidden"}));
	assert!(matches!(s.n.test(user, channel).await.unwrap(), Err(Refused::Invalid(_))));
	assert!(dead().await.is_some(), "still dead");
	assert!(matches!(s.n.test(Uuid::now_v7(), channel).await.unwrap(), Err(Refused::NotFound)), "not theirs");
	// Since added, it became a group under another title: the test that arrives learns it.
	s.mock.chat("-1001", -1001, "supergroup", false, 77);
	s.n.test(user, channel).await.unwrap().unwrap();
	assert_eq!(dead().await, None);
	let now = s
		.panel
		.alerts(user, &aliases::operator())
		.await
		.unwrap()
		.destinations
		.into_iter()
		.find(|d| d.id == channel)
		.unwrap();
	assert_eq!((now.kind, now.title.as_deref()), (sa_panel::telegram::Kind::Group, Some("chat -1001")));
}

// ── the outbox ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_outbox_keeps_one_a_second_per_chat_and_25_in_all() {
	let s = setup().await;
	for chat in 1..=30 {
		s.link(Uuid::now_v7(), aliases::operator(), chat, t0()).await;
	}
	s.mock.clear();
	s.lead("L-1", t0());
	s.lead("L-2", t0());
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 60);
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 0, "idempotent by (rule, event, destination)");

	// Each pass at its virtual time: how many went, to which chats.
	let mut sent_at: HashMap<i64, Vec<i64>> = HashMap::new();
	let mut total = 0;
	for ms in [0, 500, 999, 1000, 1500, 2000, 2500, 3000, 4000] {
		let before = s.mock.calls("sendMessage").len();
		let done = s.n.deliver(at(ms)).await.unwrap();
		let now_sent = &s.mock.calls("sendMessage")[before..];
		assert_eq!(done.sent, now_sent.len());
		assert!(now_sent.len() <= 25, "at most 25 at {ms} ms");
		for m in now_sent {
			sent_at.entry(m["chat_id"].as_i64().unwrap()).or_default().push(ms);
		}
		total += now_sent.len();
		let per_second: usize = sent_at.values().flatten().filter(|&&t| t > ms - 1000 && t <= ms).count();
		assert!(per_second <= 25, "{per_second} in the second up to {ms} ms");
	}
	assert_eq!(total, 60, "everything went, in the end");
	for (chat, times) in &sent_at {
		assert!(times.windows(2).all(|w| w[1] - w[0] >= 1000), "chat {chat}: {times:?}");
	}
	assert_eq!(sent_at.values().flatten().filter(|&&t| t == 0).count(), 25);
	assert_eq!(sent_at.values().flatten().filter(|&&t| t == 500 || t == 999).count(), 0, "the second is spent");
}

#[tokio::test]
async fn a_429_waits_what_telegram_says_and_a_5xx_backs_off() {
	let s = setup().await;
	s.link(Uuid::now_v7(), aliases::operator(), 1, t0()).await;
	s.mock.clear();
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();

	s.mock.script(
		429,
		json!({"ok": false, "error_code": 429, "description": "Too Many Requests: retry after 7", "parameters": {"retry_after": 7}}),
	);
	assert_eq!(s.n.deliver(at(0)).await.unwrap().retrying, 1);
	assert_eq!(s.n.deliver(at(6_999)).await.unwrap().sent, 0, "not before retry_after");
	s.mock.script(502, json!({"ok": false, "error_code": 502, "description": "Bad Gateway"}));
	assert_eq!(s.n.deliver(at(7_000)).await.unwrap().retrying, 1);
	assert_eq!(s.n.deliver(at(7_000 + 9_999)).await.unwrap().sent, 0, "backing off: 10 s after the first counted failure");
	assert_eq!(s.n.deliver(at(7_000 + 10_000)).await.unwrap().sent, 1);
	assert_eq!(s.outbox().await, [(1, "new_lead".to_owned(), "sent".to_owned(), 2)], "the 429 cost no try");
}

#[tokio::test]
async fn a_blocked_bot_marks_the_chat_dead() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.mock.clear();
	s.lead("L-1", t0());
	s.lead("L-2", t0());
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 2);

	s.mock
		.script(403, json!({"ok": false, "error_code": 403, "description": "Forbidden: bot was blocked by the user"}));
	assert_eq!(s.n.deliver(at(0)).await.unwrap().dead, 1);
	let states: Vec<String> = s.outbox().await.into_iter().map(|(_, _, state, _)| state).collect();
	assert_eq!(states, ["dead", "dead"], "and what it was still owed is given up too");
	assert!(s.private_of(user).await.unwrap().dead.is_some());

	s.lead("L-3", at(5_000));
	assert_eq!(s.fan_out(&s.n, at(5_000)).await.unwrap(), 0, "nothing more to a dead chat");
	assert_eq!(s.n.deliver(at(10_000)).await.unwrap(), Default::default());

	// Linking again brings it back.
	s.link(user, aliases::operator(), 1, at(20_000)).await;
	assert_eq!(s.private_of(user).await.unwrap().dead, None);
}

#[tokio::test]
async fn a_chat_telegram_cannot_find_is_dead() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.mock.clear();
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	s.mock.script(400, json!({"ok": false, "error_code": 400, "description": "Bad Request: chat not found"}));
	assert_eq!(s.n.deliver(t0()).await.unwrap().dead, 1);
	assert!(s.private_of(user).await.unwrap().dead.is_some());
}

#[tokio::test]
async fn the_bot_does_not_chatter_and_a_429_pauses_everything() {
	let s = setup().await;
	s.n.handle(Update::Other { chat: private(9) }, t0()).await.unwrap();
	let help = || Update::Start {
		chat: private(9),
		payload: None,
		from: Account::default(),
	};
	for ms in [0, 1_000, 599_999] {
		s.n.handle(help(), at(ms)).await.unwrap();
	}
	assert_eq!(s.mock.calls("sendMessage").len(), 1, "silent on chatter; one help per ten minutes");
	s.n.handle(help(), at(600_000)).await.unwrap();
	assert_eq!(s.mock.calls("sendMessage").len(), 2);

	s.link(Uuid::now_v7(), aliases::operator(), 1, t0()).await;
	s.link(Uuid::now_v7(), aliases::operator(), 2, t0()).await;
	s.mock.clear();
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	s.mock
		.script(429, json!({"ok": false, "error_code": 429, "description": "Too Many Requests", "parameters": {"retry_after": 5}}));
	let first = s.n.deliver(at(0)).await.unwrap();
	assert_eq!((first.sent, first.retrying), (1, 1));
	s.lead("L-2", at(1_000));
	s.fan_out(&s.n, at(1_000)).await.unwrap();
	assert_eq!(s.n.deliver(at(4_999)).await.unwrap(), Default::default(), "the whole outbox waits, not only the chat");
	assert_eq!(s.n.deliver(at(5_000)).await.unwrap().sent, 2);
	let attempts: Vec<i32> = sqlx::query_scalar("SELECT attempts FROM telegram_outbox ORDER BY id").fetch_all(&s.db).await.unwrap();
	assert!(attempts.iter().all(|&a| a <= 1), "the 429 cost no try: {attempts:?}");
}

/// A lead message is news for an hour from when sites noted it: past that it is given up, and
/// a destination with a flood of one rule waiting gets one summary instead — any rule.
#[tokio::test]
async fn stale_lead_messages_are_dropped_and_a_flood_is_summarized() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.mock.clear();
	let late = t0() + SignedDuration::from_mins(61);
	s.panel.alerts_access_seen(user, &aliases::operator(), "Olga", late).await.unwrap();
	s.lead("L-0", t0());
	s.fan_out(&s.n, late).await.unwrap();
	assert_eq!(s.n.deliver(late).await.unwrap(), Default::default(), "noted an hour ago, taken only now");
	assert_eq!(outbox_errors(&s).await, [("dead".to_owned(), Some("stale".to_owned()))]);

	for i in 0..25 {
		s.lead(&format!("L-{}", i + 1), late + SignedDuration::from_secs(i));
	}
	assert_eq!(s.fan_out(&s.n, late + SignedDuration::from_secs(30)).await.unwrap(), 25);
	assert_eq!(s.n.deliver(late + SignedDuration::from_secs(30)).await.unwrap().sent, 1);
	let sent = texts(&s.mock.calls("sendMessage"));
	assert_eq!(sent, ["25 новых заявок ждут звонка. Откройте панель."], "one summary, without PII");
	let summarized: i64 = sqlx::query_scalar("SELECT count(*) FROM telegram_outbox WHERE last_error = 'summarized'")
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(summarized, 25);

	// Reviews flood the same way.
	let (_, royat) = s.owner(user, "a@gmail.com", ROYAT).await;
	for _ in 0..21 {
		s.review("review_gone", late, &[royat], None);
	}
	s.mock.clear();
	assert_eq!(s.fan_out(&s.n, late + SignedDuration::from_secs(40)).await.unwrap(), 21);
	assert_eq!(s.n.deliver(late + SignedDuration::from_secs(40)).await.unwrap().sent, 1);
	assert_eq!(texts(&s.mock.calls("sendMessage")), ["Ещё 21 уведомлений «отзыв пропал» не отправлены. Откройте панель."]);
}

async fn outbox_errors(s: &Setup) -> Vec<(String, Option<String>)> {
	sqlx::query_as("SELECT state, last_error FROM telegram_outbox ORDER BY id").fetch_all(&s.db).await.unwrap()
}

#[tokio::test]
async fn access_is_asked_again_when_a_message_is_sent() {
	let s = setup().await;
	let (stale, revoked) = (Uuid::now_v7(), Uuid::now_v7());
	s.link(stale, aliases::operator(), 1, t0() - SignedDuration::from_mins(50)).await;
	s.link(revoked, aliases::operator(), 2, t0()).await;
	s.mock.clear();
	s.lead("L-1", t0());
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 2, "both confirmed within the hour, then");

	// concierge now says one holds nothing; the other's confirmation lapses while the message
	// waits. Neither is sent what was queued.
	s.panel
		.alerts_access_seen(revoked, &PermissionSet::from_iter(Vec::<String>::new()), "Rex", at(1_000))
		.await
		.unwrap();
	assert_eq!(s.n.deliver(t0() + SignedDuration::from_mins(11)).await.unwrap().dead, 2);
	assert!(s.mock.calls("sendMessage").is_empty(), "no PII on stale or withdrawn permissions");
	let not_confirmed = ("dead".to_owned(), Some("access not confirmed".to_owned()));
	assert_eq!(outbox_errors(&s).await, [not_confirmed.clone(), not_confirmed]);
}

/// Each change of a user's destinations tells their live sockets, and nobody else's: linked,
/// `/stop`, removed from the page, and found blocked.
#[tokio::test]
async fn a_destination_changing_tells_its_user() {
	let s = setup().await;
	let user = Uuid::now_v7();
	let mut rx = s.panel.bus().subscribe();
	let mut told = || {
		let mut users = Vec::new();
		while let Ok(signal) = rx.try_recv() {
			if let sa_panel::live::Signal::Changed(c) = signal
				&& c.topic == "alerts"
			{
				assert!(c.visible_to(user, &aliases::operator()) && !c.visible_to(Uuid::now_v7(), &aliases::admin()));
				users.push(c.audience);
			}
		}
		users
	};
	let mine = sa_panel::live::Audience::User(user);
	s.link(user, aliases::operator(), 1, t0()).await;
	assert_eq!(told(), std::slice::from_ref(&mine), "linked");
	s.n.handle(Update::Stop { chat: private(1) }, t0()).await.unwrap();
	assert_eq!(told(), std::slice::from_ref(&mine), "/stop");
	s.n.handle(Update::Stop { chat: private(1) }, t0()).await.unwrap();
	assert_eq!(told(), [], "nothing was linked");
	s.link(user, aliases::operator(), 1, t0()).await;
	assert!(s.unlink(user).await);
	assert_eq!(told(), [mine.clone(), mine.clone()], "linked, removed from the page");

	s.link(user, aliases::operator(), 1, t0()).await;
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	told();
	s.mock
		.script(403, json!({"ok": false, "error_code": 403, "description": "Forbidden: bot was blocked by the user"}));
	assert_eq!(s.n.deliver(at(0)).await.unwrap().dead, 1);
	assert_eq!(told(), [mine], "blocked");
}

// ── the buttons ──────────────────────────────────────────────────────────────────────────

/// Delivers the one due message; its `(message_id, [take data, no-answer data], message)`.
async fn delivered(s: &Setup, now: Timestamp) -> (i64, Vec<String>, Rendered) {
	let before = s.mock.calls("sendMessage").len();
	assert_eq!(s.n.deliver(now).await.unwrap().sent, 1);
	let sent = s.mock.calls("sendMessage")[before].clone();
	let mid: i64 = sqlx::query_scalar("SELECT tg_message_id FROM telegram_outbox WHERE state = 'sent' ORDER BY sent_at DESC, id DESC LIMIT 1")
		.fetch_one(&s.db)
		.await
		.unwrap();
	let data = sent["reply_markup"]["inline_keyboard"]
		.as_array()
		.unwrap()
		.iter()
		.map(|row| row[0]["callback_data"].as_str().unwrap().to_owned())
		.collect();
	(mid, data, as_sent(&sent))
}

/// A sent message as the Bot API echoes it back.
fn as_sent(sent: &Value) -> Rendered {
	Rendered {
		text: sent["text"].as_str().unwrap().to_owned(),
		entities: sent["entities"].as_array().cloned().unwrap_or_default(),
	}
}

fn press(id: &str, chat: i64, message_id: i64, data: &str, message: &Rendered) -> Update {
	Update::Callback {
		id: id.into(),
		chat_id: chat,
		message_id,
		data: data.into(),
		message: message.clone(),
	}
}

fn answers(s: &Setup) -> Vec<String> {
	s.mock.calls("answerCallbackQuery").iter().map(|c| c["text"].as_str().unwrap().to_owned()).collect()
}

/// The outbox id a button's data names.
fn outbox_of(data: &str) -> i64 {
	data.split('.').nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn a_button_calls_sites_as_its_user_and_is_one_action_however_often_pressed() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.session(user, aliases::admin(), "Olga").await;
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	let (mid, data, text) = delivered(&s, t0()).await;
	let (take, no_answer) = (&data[0], &data[1]);
	let outbox = outbox_of(take);

	// "No answer": recorded by sites, then a redelivered update is answered as replayed.
	s.n.handle(press("cb-1", 1, mid, no_answer, &text), at(1_000)).await.unwrap();
	s.sites.with(|sites| sites.presses.push_back(200));
	s.n.handle(press("cb-1", 1, mid, no_answer, &text), at(1_000)).await.unwrap();
	let presses = s.presses();
	assert_eq!(presses.len(), 2);
	for p in &presses {
		assert_eq!((p.method.as_str(), p.path_and_query.as_str()), ("POST", "/api/v1/leads/aquafix/L-1/no_answer"));
		assert_eq!(p.headers["idempotency-key"], format!("tg/{outbox}/no_answer"), "the key the event ids derive from");
		assert!(p.body.is_empty(), "a null body is no body");
		let sa_auth::Principal::User(who) = p.assertion(Service::Sites, None).principal else {
			panic!("a press is its user's")
		};
		assert_eq!((who.sub.as_str(), who.email.as_str(), who.name.as_str()), (user.to_string().as_str(), "olga@example.com", "Olga"));
		let held: Vec<&str> = who.permissions.iter().collect();
		assert!(!held.is_empty() && held.iter().all(|p| p.starts_with("sa:sites:")), "sites' slice only: {held:?}");
	}
	assert_eq!(answers(&s), ["Записано.", "Уже записано."]);
	let edits = s.mock.calls("editMessageText");
	assert_eq!(edits.len(), 1, "edited once");
	assert!(edits[0]["text"].as_str().unwrap().ends_with("\n\nНе дозвонился: Olga"));
	let kept = edits[0]["reply_markup"]["inline_keyboard"].as_array().unwrap();
	assert_eq!(kept.len(), 1, "only Take is left");
	assert_eq!(kept[0][0]["text"], "Взял");

	// "Take": its body as sites described it; the buttons gone after.
	s.n.handle(press("cb-2", 1, mid, take, &text), at(2_000)).await.unwrap();
	let p = s.presses().pop().unwrap();
	assert_eq!(p.path_and_query, "/api/v1/leads/aquafix/L-1/stage");
	assert_eq!(p.headers["idempotency-key"], format!("tg/{outbox}/take"));
	assert_eq!(serde_json::from_slice::<Value>(&p.body).unwrap(), json!({"stage": "contacted"}));
	assert_eq!(p.headers["content-type"], "application/json");
	let edits = s.mock.calls("editMessageText");
	assert!(edits[1]["text"].as_str().unwrap().ends_with("\n\nВзял: Olga"));
	assert_eq!(edits[1]["reply_markup"]["inline_keyboard"], json!([]), "the buttons are gone");

	// What sites says otherwise, the user is told.
	for (status, reply) in [
		(409, "Уже записано."),
		(400, "Уже записано."),
		(404, "Заявка не найдена."),
		(403, "Нет доступа к панели."),
		(500, "Не удалось проверить доступ. Попробуйте через минуту."),
	] {
		s.sites.with(|sites| sites.presses.push_back(status));
		s.n.handle(press("cb-3", 1, mid, take, &text), at(3_000)).await.unwrap();
		assert_eq!(answers(&s).pop().unwrap(), reply, "{status}");
	}
	assert_eq!(s.mock.calls("editMessageText").len(), 2, "only what sites recorded edits the message");
}

/// A message sent before the buttons came from sites named its buttons by one letter: pressed
/// now, it still calls sites, under the key the press had then.
#[tokio::test]
async fn a_button_sent_before_the_cut_over_still_works() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.session(user, aliases::operator(), "Olga").await;
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	let (mid, data, text) = delivered(&s, t0()).await;
	let outbox = outbox_of(&data[0]);
	// As the earlier build signed it: one letter for the button, under the same key and domain.
	let key = DataKey::from_hex(DATA_KEY).unwrap().telegram_callback_key();
	let mut mac = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key.as_slice()).unwrap();
	mac.update(b"sa-panel/tg-callback/v1/");
	mac.update(&1i64.to_be_bytes());
	mac.update(&outbox.to_be_bytes());
	mac.update(b"t");
	let legacy = format!("t.{outbox}.{}", hex::encode(&mac.finalize().into_bytes()[..10]));

	s.n.handle(press("cb-1", 1, mid, &legacy, &text), at(1_000)).await.unwrap();
	assert_eq!(answers(&s), ["Записано."]);
	let p = s.presses().pop().unwrap();
	assert_eq!(p.path_and_query, "/api/v1/leads/aquafix/L-1/stage");
	assert_eq!(p.headers["idempotency-key"], format!("tg/{outbox}/take"));
}

#[tokio::test]
async fn a_button_needs_access_now() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	let (mid, data, text) = delivered(&s, t0()).await;

	// No session to ask concierge with: nothing is sent to sites on the old answer.
	s.n.handle(press("cb-1", 1, mid, &data[0], &text), at(1_000)).await.unwrap();
	assert_eq!(answers(&s), ["Откройте панель, чтобы подтвердить доступ, и нажмите снова."]);
	assert!(s.presses().is_empty());
	// A session, but concierge says the grant is gone: sites, told so, refuses.
	s.session(user, PermissionSet::from_iter(Vec::<String>::new()), "Olga").await;
	s.sites.with(|sites| sites.presses.push_back(403));
	s.n.handle(press("cb-2", 1, mid, &data[0], &text), at(2_000)).await.unwrap();
	assert_eq!(answers(&s)[1], "Нет доступа к панели.");
	let sa_auth::Principal::User(who) = s.presses()[0].assertion(Service::Sites, None).principal else {
		panic!("a press is its user's")
	};
	assert_eq!(who.permissions.iter().count(), 0);
	assert!(s.mock.calls("editMessageText").is_empty());

	// And the user gets nothing more until concierge says otherwise.
	s.lead("L-2", at(3_000));
	assert_eq!(s.fan_out(&s.n, at(3_000)).await.unwrap(), 0);
}

#[tokio::test]
async fn a_forged_button_is_refused() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.session(user, aliases::operator(), "Olga").await;
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	let (mid, data, text) = delivered(&s, t0()).await;
	let take = &data[0];
	let (outbox, mac) = (outbox_of(take), take.rsplit('.').next().unwrap().to_owned());

	let forged = [
		format!("take.{}.{mac}", outbox + 1),        // another message, the same MAC
		format!("no_answer.{outbox}.{mac}"),         // another button
		format!("n.{outbox}.{mac}"),                 // another button, as an earlier build named it
		format!("take.{outbox}.{}", "0".repeat(20)), // a made-up MAC
		"take.1".to_owned(),
		"garbage".to_owned(),
	];
	for (i, data) in forged.iter().enumerate() {
		s.n.handle(press(&format!("cb-{i}"), 1, mid, data, &text), at(1_000)).await.unwrap();
	}
	// The real data, pressed from another chat: bound to the chat it was sent to.
	s.n.handle(press("cb-x", 2, mid, take, &text), at(1_000)).await.unwrap();
	assert!(answers(&s).iter().all(|a| a == "Кнопка недействительна."), "{:?}", answers(&s));
	assert_eq!(answers(&s).len(), forged.len() + 1);
	assert!(s.presses().is_empty());
	assert!(s.mock.calls("editMessageText").is_empty());
}

#[tokio::test]
async fn a_session_concierge_will_not_rotate_ends_access_and_one_refusal_does_not() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.session(user, aliases::operator(), "Olga").await;
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	let (mid, data, message) = delivered(&s, t0()).await;

	// One refused GetMe is asked again, and the press goes through.
	*s.concierge.1.lock().unwrap() = 1;
	s.n.handle(press("cb-1", 1, mid, &data[1], &message), at(1_000)).await.unwrap();
	assert_eq!(answers(&s), ["Записано."]);
	assert_eq!(s.presses().len(), 1);

	// A session whose access token is due and whose refresh concierge refuses: the user's
	// alerts end, and what waited for them is dropped.
	let other = Uuid::now_v7();
	s.link(other, aliases::operator(), 2, t0()).await;
	s.session_until(other, aliases::operator(), "Oleg", t0() + SignedDuration::from_secs(10)).await;
	s.lead("L-2", at(2_000));
	s.fan_out(&s.n, at(2_000)).await.unwrap();
	let access = s.n.confirm_access(other, at(3_000)).await.unwrap();
	assert!(matches!(access, Access::NoSession), "{access:?}");
	let permissions: Option<String> = sqlx::query_scalar("SELECT permissions FROM alert_access WHERE user_id = $1")
		.bind(other)
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(permissions, None);
	let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM telegram_outbox WHERE user_id = $1 AND state = 'pending'")
		.bind(other)
		.fetch_one(&s.db)
		.await
		.unwrap();
	assert_eq!(pending, 0);
}

#[tokio::test]
async fn the_hub_asks_only_with_a_session_in_use() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.session(user, aliases::operator(), "Olga").await;
	sqlx::query("UPDATE sessions SET last_seen_at = $1")
		.bind((t0() - SignedDuration::from_hours(24 * 8)).as_microsecond())
		.execute(&s.db)
		.await
		.unwrap();
	s.lead("L-1", t0());
	s.fan_out(&s.n, t0()).await.unwrap();
	let (mid, data, message) = delivered(&s, t0()).await;
	s.n.handle(press("cb-1", 1, mid, &data[0], &message), at(1_000)).await.unwrap();
	assert_eq!(
		answers(&s),
		["Откройте панель, чтобы подтвердить доступ, и нажмите снова."],
		"a week unused: not the hub's to keep alive"
	);
}

/// What sites renders goes as it rendered it — the customer's words in `code` — and an edit
/// keeps that formatting while no link Telegram drew over a URL in them comes back as ours.
#[tokio::test]
async fn entities_go_as_given_and_an_edit_makes_no_link() {
	let s = setup().await;
	let user = Uuid::now_v7();
	s.link(user, aliases::operator(), 1, t0()).await;
	s.session(user, aliases::operator(), "Olga").await;
	s.mock.clear();
	let body = "Новая заявка\nНужно: see https://evil.example";
	let code = json!({"type": "code", "offset": 20, "length": 29});
	let bold = json!({"type": "bold", "offset": 0, "length": 12});
	s.note("new_lead", t0(), json!({"text": body, "entities": [bold, code]}), Value::Null, None, lead_buttons("L-1"));
	s.fan_out(&s.n, t0()).await.unwrap();
	let (mid, data, message) = delivered(&s, t0()).await;
	let sent = s.mock.calls("sendMessage").pop().unwrap();
	assert_eq!(sent["entities"], json!([bold, code]), "as sites gave them");
	assert_eq!(sent["link_preview_options"]["is_disabled"], true);

	// The callback carries the message as Telegram holds it, a link of its own drawn in.
	let mut held = message.clone();
	held.entities.push(json!({"type": "url", "offset": 28, "length": 21}));
	let raw = json!({"update_id": 1, "callback_query": {"id": "cb-1", "data": data[0], "message": {"message_id": mid, "chat": {"id": 1, "type": "private"}, "text": held.text, "entities": held.entities}}});
	s.n.handle(sa_server::telegram::parse_update(&raw).unwrap(), at(1_000)).await.unwrap();
	let edit = s.mock.calls("editMessageText").pop().unwrap();
	assert_eq!(edit["text"], format!("{body}\n\nВзял: Olga"));
	assert_eq!(edit["entities"], json!([bold, code]), "the formatting kept, no link");
	assert_eq!(edit["link_preview_options"]["is_disabled"], true);
}

// ── access requests ──────────────────────────────────────────────────────────────────────

/// The conductor, everyone signed in as `email` holding `permissions`, and a browser signed in
/// to it.
async fn requester(panel: &Panel, email: &str, permissions: &[&str]) -> (axum::Router, Browser) {
	let app = sa_server::testing::app_with(panel.clone(), permissions.iter().copied().collect(), email, "http://127.0.0.1:9", Default::default());
	let mut b = Browser::default();
	b.sign_in(&app).await;
	(app, b)
}

async fn ask(b: &mut Browser, app: &axum::Router, need: &str) -> (StatusCode, Value) {
	let body = axum::body::Body::from(json!({ "need": need }).to_string());
	let (status, _, answer) = b
		.send_body(app, axum::http::Method::POST, "/api/v1/access/requests", &[("content-type", "application/json")], true, body)
		.await;
	(status, serde_json::from_slice(&answer).unwrap_or(Value::Null))
}

async fn mine(b: &mut Browser, app: &axum::Router) -> Value {
	let (status, _, mine) = b
		.send_body(app, axum::http::Method::GET, "/api/v1/access/requests/mine", &[], false, axum::body::Body::empty())
		.await;
	assert_eq!(status, StatusCode::OK);
	serde_json::from_slice(&mine).unwrap()
}

#[tokio::test]
async fn an_access_request_reaches_the_admins_with_the_grant_link() {
	let s = setup().await;
	let now = Timestamp::now();
	let admin = Uuid::now_v7();
	s.link(admin, aliases::admin(), 1, now).await;
	s.link(Uuid::now_v7(), aliases::operator(), 2, now).await;
	// The admin's channel: told of the request too, but not who asked.
	s.mock.chat("@admins", -1009, "channel", false, 1);
	s.n.add_chat(admin, &aliases::admin(), "Ann", "@admins", None, now).await.unwrap().unwrap();
	s.mock.clear();
	let (app, mut b) = requester(&s.panel, "ann@example.com", &["sa:sites:work:read"]).await;
	let n = Hub { locale: Locale::En, ..s.n.clone() };

	let (status, asked) = ask(&mut b, &app, "sa:playbook:mcp:use").await;
	assert_eq!(status, StatusCode::CREATED, "{asked}");
	assert_eq!(asked["need"], "sa:playbook:mcp:use");
	assert_eq!(mine(&mut b, &app).await, json!({ "requests": [asked] }));
	assert_eq!(s.fan_out(&n, now).await.unwrap(), 1, "the admin's own chat, not the operator's, not a channel");
	assert_eq!(n.deliver(now).await.unwrap().sent, 1);
	let sent = s.mock.calls("sendMessage");
	assert_eq!(sent[0]["chat_id"], 1);
	assert_eq!(
		sent[0]["text"],
		"🔑 Playbook access request\nann@example.com wants to connect Claude Code to the playbook.\nName: Dev sign-in (ann@example.com)"
	);
	assert_eq!(
		sent[0]["reply_markup"]["inline_keyboard"],
		json!([[{"text": "Grant access", "url": "https://evinvest.test/admin/scopes/sa?email=ann%40example.com&target=sa%3Aplaybook%3Amcp%3Ause"}]])
	);

	assert_eq!(
		ask(&mut b, &app, "sa:playbook:mcp:use").await,
		(StatusCode::OK, asked.clone()),
		"asked again within the day: the same request"
	);
	assert_eq!(s.fan_out(&n, now).await.unwrap(), 0, "and no second message");

	for (need, want) in [
		("sa:sites:work:read", StatusCode::CONFLICT),
		("sa:nope:x", StatusCode::BAD_REQUEST),
		("sa:admin:sources", StatusCode::BAD_REQUEST),
		("", StatusCode::BAD_REQUEST),
		("sa:operator", StatusCode::CREATED),
	] {
		let (status, body) = ask(&mut b, &app, need).await;
		assert_eq!(status, want, "{need}: {body}");
	}
	assert_eq!(mine(&mut b, &app).await["requests"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_access_request_is_told_again_after_a_day_and_dropped_once_held() {
	let s = setup().await;
	let admin = Uuid::now_v7();
	s.link(admin, aliases::admin(), 1, t0()).await;
	s.mock.clear();
	let bob = Uuid::now_v7();
	let ask = |at| s.panel.request_access(bob, "bob@example.com", "Bob", "sa:operator", at);
	let day = SignedDuration::from_hours(24);

	assert_eq!(ask(t0()).await.unwrap(), (s.panel.pending_access(bob, |_| false).await.unwrap()[0].clone(), true));
	assert_eq!(s.fan_out(&s.n, t0()).await.unwrap(), 1);
	s.n.deliver(t0()).await.unwrap();
	let within = t0() + day - SignedDuration::from_secs(1);
	assert_eq!(ask(within).await.unwrap().0.requested_at, t0(), "within the day: the same");
	let next = t0() + day;
	s.panel.alerts_access_seen(admin, &aliases::admin(), "Ann", next).await.unwrap();
	assert_eq!(ask(next).await.unwrap().0.requested_at, next, "a day on: asked again");
	assert_eq!(s.fan_out(&s.n, next).await.unwrap(), 1, "and told again");
	s.n.deliver(next).await.unwrap();
	assert_eq!(texts(&s.mock.calls("sendMessage")), ["🔑 Запрос доступа\nbob@example.com просит sa:operator.\nИмя: Bob"; 2]);

	let pending = |held: &'static [&'static str]| s.panel.pending_access(bob, move |need| held.contains(&need));
	assert_eq!(pending(&[]).await.unwrap().len(), 1);
	assert_eq!(pending(&["sa:operator"]).await.unwrap(), [], "held: dropped");
	assert_eq!(pending(&[]).await.unwrap(), [], "for good");
}

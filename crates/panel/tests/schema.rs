//! Everything the runtime does runs on a freshly migrated database, and the schema itself
//! refuses what the code must never do: a source is only ever revoked. (SQLite has no roles, so
//! the tables' triggers say what a runtime role's grants would.)

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};
use panel::{
	seal::DataKey,
	session::{RefreshError, Refresher, SessionKey, Tokens},
	store::Store,
	telegram::{Account, Bot, Call, Chat, Directory, DirectoryError, Identity, InlineButton, NotePage, Notifier, Sites, Update},
	testing::{TestDb, panel_with, sign},
};
use panel_core::{
	ids::{BrandId, SiteOrigin},
	notify::{Failure, Locale, Rendered, Rule},
};
use sa_auth::{PermissionSet, SA_OPERATOR, SourceKind};
use serde_json::json;
use zeroize::Zeroizing;

/// A refresher that must not be asked: the tokens above are fresh.
struct Never;

impl Refresher for Never {
	async fn refresh(&self, _: &str) -> Result<Tokens, RefreshError> {
		unreachable!("the tokens are fresh")
	}
}

/// A refresher that rotates once, to `a2`/`r2`.
struct Rotates;

impl Refresher for Rotates {
	async fn refresh(&self, _: &str) -> Result<Tokens, RefreshError> {
		let now = Timestamp::now();
		Ok(Tokens {
			access: Zeroizing::new("a2".into()),
			access_expires_at: now + SignedDuration::from_hours(24 * 400),
			refresh: Zeroizing::new("r2".into()),
			refresh_expires_at: now + SignedDuration::from_hours(24 * 400),
		})
	}
}

/// A bot that delivers everything, or refuses every send as blocked.
struct FakeBot {
	blocked: bool,
}

impl Bot for FakeBot {
	async fn send(&self, _: i64, _: &Rendered, _: &[InlineButton]) -> Result<i64, Failure> {
		if self.blocked { Err(Failure::Blocked) } else { Ok(1) }
	}

	async fn edit(&self, _: i64, _: i64, _: &Rendered, _: &[InlineButton]) -> Result<(), Failure> {
		Ok(())
	}

	async fn answer(&self, _: &str, _: &str) -> Result<(), Failure> {
		Ok(())
	}
}

/// Refreshes nothing; refuses every token.
struct Operators;

impl Refresher for Operators {
	async fn refresh(&self, _: &str) -> Result<Tokens, RefreshError> {
		Err(RefreshError::Rejected)
	}
}

impl Directory for Operators {
	async fn me(&self, _: &str) -> Result<Identity, DirectoryError> {
		Err(DirectoryError::Refused)
	}
}

/// Sites with two new leads to tell of, and nothing else.
struct TwoLeads;

impl Sites for TwoLeads {
	async fn notes(&self, after: i64, _: usize) -> eyre::Result<NotePage> {
		let note = |seq: i64| {
			json!({"seq": seq, "rule": "new_lead", "event_id": uuid::Uuid::now_v7(), "created_at": Timestamp::now(),
				"text": {"text": "New lead", "entities": []}, "text_pii": null, "skip_user": null, "buttons": []})
		};
		let notes = if after == 0 { vec![note(1), note(2)] } else { vec![] };
		Ok(serde_json::from_value(json!({"notes": notes, "next": after.max(2)}))?)
	}

	async fn received(&self) -> eyre::Result<BTreeMap<String, Timestamp>> {
		Ok(BTreeMap::new())
	}

	async fn press(&self, _: &Identity, _: &Call, _: &str) -> eyre::Result<u16> {
		unreachable!("no button here")
	}
}

#[tokio::test]
async fn the_runtime_does_its_work_on_a_fresh_database() {
	let db = TestDb::create().await;
	Store::open(db.path()).await.expect("created and migrated");
	Store::open(db.path()).await.expect("again: nothing to do");
	{
		let key = DataKey::from_hex(&DataKey::generate_hex().unwrap()).unwrap();
		let panel = panel_with(db.store().await, key);
		let now = Timestamp::now();
		let secret = panel
			.add_source("aquafix-site", SourceKind::Site, [BrandId::parse("aquafix").unwrap()].into(), Some(origin("https://aquafix.top")))
			.await
			.unwrap()
			.unwrap()
			.secret
			.to_string();
		let grant = panel.authenticate(sign("aquafix-site", &secret, b"{}", now).batch(), now).await.unwrap();
		assert_eq!((grant.key_id.as_str(), grant.kind), ("aquafix-site", SourceKind::Site));
		assert_eq!(panel.store().sources().await.unwrap().len(), 1);
		assert!(panel.revoke_source("aquafix-site").await.unwrap());
		assert!(panel.authenticate(sign("aquafix-site", &secret, b"{}", now).batch(), now).await.is_err(), "revoked");

		// Signed-in users: a session, a rotation (its lease, its guarded write), a redeemed state,
		// a sign-out everywhere.
		let tokens = Tokens {
			access: Zeroizing::new("a".into()),
			access_expires_at: now + SignedDuration::from_mins(15),
			refresh: Zeroizing::new("r".into()),
			refresh_expires_at: now + SignedDuration::from_hours(1),
		};
		let user = uuid::Uuid::now_v7();
		let opened = panel.open_session(user, &tokens, now).await.unwrap();
		assert_eq!(panel.session(&opened.cookie, now, &Never).await.unwrap().user_id, user);
		assert!(panel.close_session(&SessionKey::of_cookie(&opened.cookie).unwrap()).await.unwrap());
		let opened = panel.open_session(user, &tokens, now).await.unwrap();
		let stale = now + SignedDuration::from_mins(15);
		assert_eq!(panel.session(&opened.cookie, stale, &Rotates).await.unwrap().access.as_str(), "a2");
		assert!(panel.consume_state("state", now).await.unwrap());
		assert!(!panel.consume_state("state", now).await.unwrap());
		assert!(panel.consume_state("state", now + SignedDuration::from_hours(1)).await.unwrap(), "expired marks are dropped");
		assert_eq!(panel.close_all_sessions(&SessionKey::of_cookie(&opened.cookie).unwrap()).await.unwrap(), Some(user));

		// Access requests, and Telegram: a link, rules, sites' notes and the conductor's own into
		// the outbox, a delivery, a dead chat, the access checks with their pruning, the poller.
		let (asked, made) = panel.request_access(user, "o@x.io", "Olga", "sa:operator", now).await.unwrap();
		assert!(made && asked.need == "sa:operator");
		let notifier = |blocked| Notifier {
			panel: panel.clone(),
			bot: FakeBot { blocked },
			concierge: Operators,
			sites: TwoLeads,
			locale: Locale::Ru,
			cabinet: cabinet(),
		};
		let operator: PermissionSet = SA_OPERATOR.members.iter().copied().collect();
		let token = panel.telegram_link_token(user, &operator, "Olga", now).await.unwrap();
		let start = |chat| Update::Start {
			chat: Chat { id: chat, private: true },
			payload: Some(token.to_string()),
			from: Account::default(),
		};
		notifier(false).handle(start(7), now).await.unwrap();
		panel.telegram_set_rules(user, &operator, &[(Rule::NewLead, true)]).await.unwrap().unwrap();
		panel.telegram_access_seen(user, &operator, "Olga", now).await.unwrap();
		assert_eq!(notifier(false).fan_out(now).await.unwrap(), 2, "sites' two leads; an operator is not told of access requests");
		assert_eq!(panel.telegram_notes_after().await.unwrap(), 2);
		assert_eq!(notifier(false).deliver(now).await.unwrap().sent, 1);
		assert_eq!(notifier(true).deliver(now + SignedDuration::from_secs(2)).await.unwrap().dead, 1);
		assert_eq!(
			notifier(false).recheck_access(now + SignedDuration::from_hours(24 * 8)).await.unwrap(),
			0,
			"a dead chat is not rechecked"
		);
		let holder = uuid::Uuid::now_v7();
		assert_eq!(panel.telegram_poll_lease(holder, now, now + SignedDuration::from_mins(1)).await.unwrap(), Some(0));
		assert!(panel.telegram_poll_advance(holder, 5).await.unwrap());
		panel.telegram_poll_release(holder).await.unwrap();
		notifier(false)
			.handle(
				Update::Stop {
					chat: Chat { id: 7, private: true },
				},
				now,
			)
			.await
			.unwrap();
		assert!(!panel.telegram_settings(user, &operator).await.unwrap().linked);

		let pool = panel.store().pool();
		for (sql, refusal) in [
			("DELETE FROM sources", "sources are never deleted"),
			("UPDATE sources SET brand_ids = '[\"x\"]'", "a source only ever gets revoked"),
			("INSERT INTO telegram_poller (id) VALUES (2)", "CHECK constraint failed"),
			(
				"INSERT INTO sources (key_id, kind, brand_ids, secret_sealed, data_key_fp) VALUES ('x', 'site', '[]', x'00', zeroblob(32))",
				"CHECK constraint failed",
			),
			(
				"INSERT INTO sources (key_id, kind, brand_ids, secret_sealed, data_key_fp) VALUES ('Bad Key', 'site', '[\"x\"]', x'00', zeroblob(32))",
				"CHECK constraint failed",
			),
		] {
			let err = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await.unwrap_err().to_string();
			assert!(err.contains(refusal), "{sql}: {err}");
		}
		pool.close().await;
	}
}

/// The database the conductor starts from is the panel's whole: the sites' tables go, the
/// conductor's rows stay, and a message sent before keeps buttons that call sites for the lead
/// it named.
#[tokio::test]
async fn the_sites_tables_go_and_sent_buttons_name_their_calls() {
	const MOVED_OUT: i64 = 20261102090000;
	let db = TestDb::create().await;
	let pool = db.pool().await;
	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, MOVED_OUT - 1).await.unwrap();
	for sql in [
		"INSERT INTO sources (key_id, kind, brand_ids, secret_sealed, data_key_fp) VALUES ('aquafix-site', 'site', '[\"aquafix\"]', x'00', zeroblob(32))",
		"INSERT INTO events (id, schema, type, type_version, occurred_at, received_at, source_kind, source_id, key_id, brand_id, lead_id, \
		 status, properties, content_mac) VALUES (zeroblob(16), 'sa.funnel.v1', 'lead.created', 1, 1, 1, 'site', 'aquafix-site', 'aquafix-site', \
		 'aquafix', 'L-1', 'registered', '{}', zeroblob(32))",
		"INSERT INTO telegram_outbox (user_id, chat_id, rule, event_id, brand_id, lead_id, buttons, state, next_attempt_at, created_at, sent_at, tg_message_id) \
		 VALUES (zeroblob(16), 7, 'new_lead', zeroblob(16), 'aquafix', 'L-1', '[\"take\",\"no_answer\"]', 'sent', 1, 1, 1, 9)",
		"INSERT INTO telegram_outbox (user_id, chat_id, rule, event_id, buttons, text_sealed, data_key_fp, next_attempt_at, created_at) \
		 VALUES (zeroblob(16), 7, 'access_requested', zeroblob(16), '[]', x'00', zeroblob(32), 1, 1)",
	] {
		sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&pool).await.unwrap();
	}

	migrator.run(&pool).await.unwrap();
	let tables: Vec<String> =
		sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE '\\_%' ESCAPE '\\' AND name <> 'sqlite_sequence' ORDER BY name")
			.fetch_all(&pool)
			.await
			.unwrap();
	assert_eq!(
		tables,
		[
			"access_requests",
			"charge_cursors",
			"consumed_states",
			"gmails",
			"gmbs",
			"sessions",
			"sources",
			"telegram_fanout",
			"telegram_link_tokens",
			"telegram_links",
			"telegram_outbox",
			"telegram_poller",
			"telegram_replies",
			"telegram_rules",
			"token_kickbacks",
			"token_ledger",
			"users"
		]
	);
	let sources: i64 = sqlx::query_scalar("SELECT count(*) FROM sources").fetch_one(&pool).await.unwrap();
	assert_eq!(sources, 1);
	let rows: Vec<(String, Option<String>)> = sqlx::query_as("SELECT buttons, last_error FROM telegram_outbox ORDER BY id").fetch_all(&pool).await.unwrap();
	let sent: Vec<panel::telegram::Button> = serde_json::from_str(&rows[0].0).unwrap();
	assert_eq!(
		sent.iter()
			.map(|b| (b.id.as_str(), b.call.method.as_str(), b.call.path.as_str(), &b.call.body, b.leaves.clone()))
			.collect::<Vec<_>>(),
		[
			("take", "POST", "/api/v1/leads/aquafix/L-1/stage", &json!({"stage": "contacted"}), vec![]),
			("no_answer", "POST", "/api/v1/leads/aquafix/L-1/no_answer", &json!(null), vec!["take".to_owned()]),
		]
	);
	assert_eq!(rows[1].1.as_deref(), Some("cut-over"), "what was queued is given up: its text is of the earlier shape");

	migrator.undo(&pool, MOVED_OUT - 1).await.unwrap();
	let back: (String, Option<String>, Option<String>) = sqlx::query_as("SELECT buttons, brand_id, lead_id FROM telegram_outbox ORDER BY id LIMIT 1")
		.fetch_one(&pool)
		.await
		.unwrap();
	assert_eq!(back, (r#"["take","no_answer"]"#.to_owned(), Some("aquafix".to_owned()), Some("L-1".to_owned())));
}

/// A Telegram link and a link token made under roles carry, after the migration, exactly what
/// the role's alias held; and go back to the role on the way down.
#[tokio::test]
async fn telegram_roles_become_permissions_both_ways() {
	const PERMISSIONS: i64 = 20261006090000;
	// `sa:admin` as the migration wrote it: the alias of that day, frozen (re-spelled under
	// `sa:sites:` since). A permission added to the alias since (`sa:sites:work:invoices:create`)
	// reaches a link when concierge is next asked.
	const ADMIN_WHEN_WRITTEN: &[&str] = &[
		"sa:admin:sources:manage",
		"sa:sites:analysis:experiments:edit",
		"sa:sites:analysis:read",
		"sa:playbook:mcp:use",
		"sa:review_archive:archive:operate",
		"sa:review_archive:members:act_as",
		"sa:review_archive:tokens:grant",
		"sa:sites:work:leads:edit",
		"sa:sites:work:pii:see",
		"sa:sites:work:places:edit",
		"sa:sites:work:pricing:edit",
		"sa:sites:work:read",
	];
	let db = TestDb::create().await;
	let pool = db.pool().await;
	let migrator = sqlx::migrate!("./migrations");
	migrator.undo(&pool, PERMISSIONS - 1).await.unwrap();
	let (ann, bob) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
	for (user, chat, role) in [(ann, 7, "operator"), (bob, 8, "admin")] {
		sqlx::query("INSERT INTO telegram_links (user_id, chat_id, linked_at, role, role_checked_at, display_name) VALUES ($1, $2, 0, $3, 5, 'x')")
			.bind(user)
			.bind(chat)
			.bind(role)
			.execute(&pool)
			.await
			.unwrap();
	}
	sqlx::query("INSERT INTO telegram_link_tokens (token_hash, user_id, role, display_name, expires_at) VALUES (zeroblob(32), $1, 'admin', 'x', 9)")
		.bind(bob)
		.execute(&pool)
		.await
		.unwrap();

	migrator.run(&pool).await.unwrap();
	let held = |sql: &'static str, user: uuid::Uuid| {
		let pool = pool.clone();
		async move {
			let raw: String = sqlx::query_scalar(sql).bind(user).fetch_one(&pool).await.unwrap();
			(serde_json::from_str::<PermissionSet>(&raw).unwrap(), raw)
		}
	};
	let alias = |a: &[&str]| a.iter().copied().collect::<PermissionSet>();
	let links = "SELECT permissions FROM telegram_links WHERE user_id = $1";
	let (operator, raw) = held(links, ann).await;
	assert_eq!(operator, alias(SA_OPERATOR.members));
	assert_eq!(raw, serde_json::to_string(&operator).unwrap(), "written as the panel writes a set");
	assert_eq!(held(links, bob).await.0, alias(ADMIN_WHEN_WRITTEN));
	assert_eq!(held("SELECT permissions FROM telegram_link_tokens WHERE user_id = $1", bob).await.0, alias(ADMIN_WHEN_WRITTEN));
	let checked: i64 = sqlx::query_scalar("SELECT permissions_checked_at FROM telegram_links WHERE user_id = $1")
		.bind(ann)
		.fetch_one(&pool)
		.await
		.unwrap();
	assert_eq!(checked, 5);

	migrator.undo(&pool, PERMISSIONS - 1).await.unwrap();
	let roles: Vec<(uuid::Uuid, String)> = sqlx::query_as("SELECT user_id, role FROM telegram_links ORDER BY user_id").fetch_all(&pool).await.unwrap();
	assert_eq!(roles, [(ann, "operator".to_owned()), (bob, "admin".to_owned())]);
	let token: String = sqlx::query_scalar("SELECT role FROM telegram_link_tokens").fetch_one(&pool).await.unwrap();
	assert_eq!(token, "admin");
}

/// A site key carries the origin its landing answers on; a key from before origins existed gets
/// one once, and then keeps it.
#[tokio::test]
async fn a_site_origin_is_set_once() {
	let db = TestDb::create().await;
	let key = DataKey::from_hex(&DataKey::generate_hex().unwrap()).unwrap();
	let panel = Panel::new(db.store().await, key);
	let aquafix = || [BrandId::parse("aquafix").unwrap()].into();
	assert!(panel.add_source("aquafix-site", SourceKind::Site, aquafix(), None).await.is_err(), "a site key without an origin");
	assert!(
		panel.add_source("aquafix-bot", SourceKind::Bot, aquafix(), Some(origin("https://aquafix.top"))).await.is_err(),
		"an origin on a key that is not a site's"
	);
	panel
		.add_source("vifnet-site", SourceKind::Site, aquafix(), Some(origin("https://vifnet.site")))
		.await
		.unwrap()
		.unwrap();
	panel.add_source("aquafix-bot", SourceKind::Bot, aquafix(), None).await.unwrap().unwrap();

	let pool = panel.store().pool();
	sqlx::query("INSERT INTO sources (key_id, kind, brand_ids, secret_sealed, data_key_fp) VALUES ('aquafix-site', 'site', '[\"aquafix\"]', x'00', zeroblob(32))")
		.execute(pool)
		.await
		.unwrap();
	let origin_of = |key: &'static str| {
		let panel = panel.clone();
		async move {
			panel
				.store()
				.sources()
				.await
				.unwrap()
				.into_iter()
				.find(|s| s.grant.key_id == key)
				.unwrap()
				.origin
				.map(|o| o.to_string())
		}
	};
	assert_eq!(origin_of("aquafix-site").await, None, "made before origins");
	panel.set_site_origin("aquafix-site", &origin("https://aquafix.top")).await.unwrap();
	panel.set_site_origin("aquafix-site", &origin("https://aquafix.top")).await.expect("the same again is no change");
	assert!(panel.set_site_origin("aquafix-site", &origin("https://elsewhere.top")).await.is_err());
	assert!(panel.set_site_origin("aquafix-bot", &origin("https://aquafix.top")).await.is_err());
	assert!(panel.set_site_origin("nobody", &origin("https://aquafix.top")).await.is_err());
	assert_eq!(origin_of("aquafix-site").await.as_deref(), Some("https://aquafix.top"));
	assert_eq!(origin_of("vifnet-site").await.as_deref(), Some("https://vifnet.site"));

	for (sql, refusal) in [
		(
			"UPDATE sources SET origin = 'https://elsewhere.top' WHERE key_id = 'aquafix-site'",
			"a source only ever gets revoked",
		),
		("UPDATE sources SET origin = NULL WHERE key_id = 'aquafix-site'", "a source only ever gets revoked"),
		("UPDATE sources SET origin = 'https://aquafix.top' WHERE key_id = 'aquafix-bot'", "CHECK constraint failed"),
	] {
		let err = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await.unwrap_err().to_string();
		assert!(err.contains(refusal), "{sql}: {err}");
	}
	pool.close().await;
}

fn origin(raw: &str) -> SiteOrigin {
	SiteOrigin::parse(raw).unwrap()
}

fn cabinet() -> url::Url {
	"https://evinvest.test".parse().unwrap()
}

//! Everything the runtime does runs on a freshly migrated database, and the schema itself
//! refuses what the code must never do: the journal is append-only, a source is only ever
//! revoked. (On Postgres a runtime role's grants said so; SQLite has no roles, so the tables'
//! triggers do.)

use jiff::{SignedDuration, Timestamp};
use panel::{
	Outcome, Panel,
	operator::{Actor, FunnelBy, LeadQuery, NewLead, Payment, Pii},
	seal::DataKey,
	session::{RefreshError, Refresher, SessionKey, Tokens},
	store::Store,
	telegram::{Account, Bot, Chat, Directory, DirectoryError, Identity, InlineButton, Notifier, Update},
	testing::{TestDb, event, sign},
};
use panel_core::{
	event::SourceKind,
	ids::{BrandId, LocationId},
	notify::{Failure, Locale, Rendered, Rule},
};
use sa_auth::{PermissionSet, SA_ADMIN, SA_OPERATOR};
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

/// Refreshes nothing; says every token is an operator's.
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

#[tokio::test]
async fn the_runtime_does_its_work_on_a_fresh_database() {
	let db = TestDb::create().await;
	Store::open(db.path()).await.expect("created and migrated");
	Store::open(db.path()).await.expect("again: nothing to do");
	{
		let key = DataKey::from_hex(&DataKey::generate_hex().unwrap()).unwrap();
		let panel = Panel::new(db.store().await, key);
		let secret = panel
			.add_source("aquafix-ops", SourceKind::Panel, [BrandId::parse("aquafix").unwrap()].into())
			.await
			.unwrap()
			.unwrap()
			.secret
			.to_string();
		let now = Timestamp::now();
		let lead = json!({"brandId": "aquafix", "leadId": "L-1"});
		let events = [
			event("lead.created", now - SignedDuration::from_mins(5), "panel", lead.clone(), json!({"channel": "phone_inbound"})),
			event("call.logged", now, "panel", lead.clone(), json!({"outcome": "answered"})),
			event("payment.received", now, "panel", lead, json!({"billed": 100, "commission": 10, "currency": "EUR"})),
		];
		let got = panel.ingest(sign("aquafix-ops", &secret, &events, now).batch(), now).await.unwrap();
		assert!(got.iter().all(|v| v.outcome == Outcome::Accepted { unregistered: false }), "{got:?}");
		assert_eq!(panel.rebuild_projections().await.unwrap().leads, 1);
		assert!(panel.store().revoke_source("aquafix-ops").await.unwrap());

		// Signed-in users: a session, and what an operator does and reads.
		let tokens = Tokens {
			access: Zeroizing::new("a".into()),
			access_expires_at: now + SignedDuration::from_mins(15),
			refresh: Zeroizing::new("r".into()),
			refresh_expires_at: now + SignedDuration::from_hours(1),
		};
		let user = uuid::Uuid::now_v7();
		let opened = panel.open_session(user, &tokens, now).await.unwrap();
		assert_eq!(panel.session(&opened.cookie, now, &Never).await.unwrap().user_id, user);
		let by = Actor(user);
		let new_lead = || NewLead {
			brand: BrandId::parse("aquafix").unwrap(),
			location: LocationId::parse("paris-11").unwrap(),
			need: "a boiler".into(),
			phone: None,
			channel: panel_core::fact::LeadChannel::PhoneInbound,
		};
		let (lead, _) = panel.create_lead(by, new_lead(), now).await.unwrap();
		let brand = BrandId::parse("aquafix").unwrap();
		panel.attempt_call(by, &brand, &lead, now).await.unwrap();
		assert_eq!(panel.leads(&LeadQuery { limit: 10, ..LeadQuery::default() }, Pii::Reveal, now).await.unwrap().leads.len(), 2);
		assert!(panel.lead_card(&brand, &lead, Pii::Reveal, now).await.unwrap().is_some());
		let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
		assert_eq!(panel.funnel(today, today, None).await.unwrap().manual, 2);
		let payment = Payment {
			billed: 100,
			commission: 10,
			currency: "EUR".into(),
		};
		panel.record_payment(by, &brand, &lead, payment, now).await.unwrap();
		let slices = panel.funnel_slices(today, today, None, FunnelBy::Location).await.unwrap();
		assert_eq!(slices[0].payments[0].billed, 100);
		assert_eq!(panel.places().await.unwrap().len(), 1);
		assert_eq!(panel.lead_counts(Some(&brand), None, now).await.unwrap().overdue, 0);
		assert!(panel.close_session(&SessionKey::of_cookie(&opened.cookie).unwrap()).await.unwrap());

		// A rotation (its lease, its guarded write), a redeemed state, a sign-out everywhere.
		let opened = panel.open_session(user, &tokens, now).await.unwrap();
		let stale = now + SignedDuration::from_mins(15);
		assert_eq!(panel.session(&opened.cookie, stale, &Rotates).await.unwrap().access.as_str(), "a2");
		assert!(panel.consume_state("state", now).await.unwrap());
		assert!(!panel.consume_state("state", now).await.unwrap());
		assert!(panel.consume_state("state", now + SignedDuration::from_hours(1)).await.unwrap(), "expired marks are dropped");
		assert_eq!(panel.close_all_sessions(&SessionKey::of_cookie(&opened.cookie).unwrap()).await.unwrap(), Some(user));

		// Telegram: a link, rules, a fan-out, a delivery, a dead chat, the access checks with
		// their pruning, the poller.
		let notifier = |blocked| Notifier {
			panel: panel.clone(),
			bot: FakeBot { blocked },
			concierge: Operators,
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
		panel.telegram_set_rules(user, &operator, &[(Rule::NewLead, true)]).await.unwrap();
		panel.telegram_access_seen(user, &operator, "Olga", now).await.unwrap();
		// By a colleague: the user's own lead above is not told to them.
		let colleague = Actor(uuid::Uuid::now_v7());
		for need in ["a tap", "a sink"] {
			panel.create_lead(colleague, NewLead { need: need.into(), ..new_lead() }, now).await.unwrap();
		}
		assert_eq!(panel.telegram_fan_out(now, Locale::Ru, &cabinet()).await.unwrap(), 2, "the colleague's two leads");
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
			("DELETE FROM events", "events is append-only"),
			("UPDATE events SET brand_id = 'x'", "events is append-only"),
			("UPDATE events SET received_at = received_at + 1", "events is append-only"),
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
			("UPDATE leads SET last_event_id = x'00000000000000000000000000000000'", "FOREIGN KEY constraint failed"),
		] {
			let err = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await.unwrap_err().to_string();
			assert!(err.contains(refusal), "{sql}: {err}");
		}
		sqlx::query("UPDATE events SET status = status, status_reason = 'x'")
			.execute(pool)
			.await
			.expect("status and its reason are the journal's one change");
		sqlx::query("UPDATE sources SET revoked_at = 1 WHERE revoked_at IS NULL")
			.execute(pool)
			.await
			.expect("revoking is the one change to a source");
		let n: i64 = sqlx::query_scalar("SELECT count(*) FROM reporting_leads").fetch_one(pool).await.unwrap();
		assert_eq!(n, 4, "the ingested lead and the three taken by phone");
		pool.close().await;
	}
}

/// A Telegram link and a link token made under roles carry, after the migration, exactly what
/// the role's alias held; and go back to the role on the way down.
#[tokio::test]
async fn telegram_roles_become_permissions_both_ways() {
	const PERMISSIONS: i64 = 20261006090000;
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
	assert_eq!(held(links, bob).await.0, alias(SA_ADMIN.members));
	assert_eq!(held("SELECT permissions FROM telegram_link_tokens WHERE user_id = $1", bob).await.0, alias(SA_ADMIN.members));
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

fn cabinet() -> url::Url {
	"https://evinvest.test".parse().unwrap()
}

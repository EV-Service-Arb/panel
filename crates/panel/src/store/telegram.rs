//! Telegram: link tokens, links, the users' rules, the fan-out marks, the outbox, the poller's
//! lease and the cursor into sites' notes. Every timestamp is the caller's `now`, never the database's clock, so the
//! pacing can be driven by a test's virtual time.

use eyre::WrapErr;
use jiff::{SignedDuration, Timestamp};
use panel_core::notify::{GLOBAL_PER_SECOND, PER_CHAT_GAP, Rule};
use sa_auth::PermissionSet;
use sqlx::{SqliteConnection, types::Json};
use uuid::Uuid;

use super::{begin_write, from_db, to_db};
use crate::telegram::Button;

// ── link tokens ─────────────────────────────────────────────────────────────────────────

/// Stores a link token's hash, in place of any the user had before; expired ones are
/// dropped on the way.
pub async fn insert_link_token(conn: &mut SqliteConnection, hash: &[u8], user: Uuid, permissions: &PermissionSet, display: &str, now: Timestamp, expires_at: Timestamp) -> eyre::Result<()> {
	sqlx::query("DELETE FROM telegram_link_tokens WHERE expires_at <= $1 OR user_id = $2")
		.bind(to_db(now))
		.bind(user)
		.execute(&mut *conn)
		.await
		.wrap_err("pruning link tokens")?;
	sqlx::query("INSERT INTO telegram_link_tokens (token_hash, user_id, permissions, display_name, expires_at, issued_at) VALUES ($1, $2, $3, $4, $5, $6)")
		.bind(hash)
		.bind(user)
		.bind(Json(permissions))
		.bind(display)
		.bind(to_db(expires_at))
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("storing a link token")?;
	Ok(())
}

/// A redeemed link token: whose, their permissions and name, and when it was issued.
#[derive(Clone, Debug)]
pub struct Redeemed {
	pub user_id: Uuid,
	pub permissions: PermissionSet,
	pub display_name: String,
	pub issued_at: Timestamp,
}

/// Redeems a link token: whose it was, if it existed and had not expired. Gone either way,
/// so a token opens at most one link — unless the caller rolls its transaction back.
pub async fn redeem_link_token(conn: &mut SqliteConnection, hash: &[u8], now: Timestamp) -> eyre::Result<Option<Redeemed>> {
	type Row = (Uuid, Json<PermissionSet>, String, i64, Option<i64>);
	let row: Option<Row> = sqlx::query_as("DELETE FROM telegram_link_tokens WHERE token_hash = $1 RETURNING user_id, permissions, display_name, expires_at, issued_at")
		.bind(hash)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("redeeming a link token")?;
	let Some((user_id, Json(permissions), display_name, expires_at, issued_at)) = row else {
		return Ok(None);
	};
	let expires_at = from_db(expires_at)?;
	if expires_at <= now {
		return Ok(None);
	}
	Ok(Some(Redeemed {
		user_id,
		permissions,
		display_name,
		// A token from before `issued_at` was stored: the earliest it could have been issued.
		issued_at: issued_at.map(from_db).transpose()?.unwrap_or(expires_at - crate::telegram::LINK_TTL),
	}))
}

// ── links ───────────────────────────────────────────────────────────────────────────────

/// A user's chat with the bot.
#[derive(Clone, Debug)]
pub struct LinkRow {
	pub user_id: Uuid,
	pub chat_id: i64,
	/// `None`: their access is known lost.
	pub permissions: Option<PermissionSet>,
	pub permissions_checked_at: Timestamp,
	pub display_name: String,
	pub dead: bool,
	/// The Telegram account: `@username`, else its first name.
	pub account: Option<String>,
}

type LinkDb = (Uuid, i64, Option<Json<PermissionSet>>, i64, String, Option<i64>, Option<String>, Option<String>);

fn link_row((user_id, chat_id, permissions, checked, display_name, dead_at, username, first_name): LinkDb) -> eyre::Result<LinkRow> {
	Ok(LinkRow {
		user_id,
		chat_id,
		permissions: permissions.map(|Json(p)| p),
		permissions_checked_at: from_db(checked)?,
		display_name,
		dead: dead_at.is_some(),
		account: username.map(|u| format!("@{u}")).or(first_name),
	})
}

// A macro, not a const, so every query stays a literal (`concat!`) that sqlx takes as audited.
macro_rules! link_select {
	() => {
		"SELECT user_id, chat_id, permissions, permissions_checked_at, display_name, dead_at, tg_username, tg_first_name FROM telegram_links "
	};
}

// A macro, not a const, so every query stays a literal (`concat!`) that sqlx takes as audited.
/// Gives up what the outbox still owed: the filter follows.
macro_rules! drop_pending {
	() => {
		"UPDATE telegram_outbox SET state = 'dead', text_sealed = NULL, data_key_fp = NULL, leased_until = NULL, last_error = $1 WHERE state = 'pending' AND "
	};
}

/// The Telegram account behind a chat, as its `/start` said.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Account {
	pub username: Option<String>,
	pub first_name: Option<String>,
}

/// Gives up what the outbox still owed a user, and why.
async fn drop_pending_of_user(conn: &mut SqliteConnection, user: Uuid, why: &str) -> eyre::Result<()> {
	sqlx::query(concat!(drop_pending!(), "user_id = $2"))
		.bind(why)
		.bind(user)
		.execute(&mut *conn)
		.await
		.wrap_err_with(|| format!("dropping what a user was owed ({why})"))?;
	Ok(())
}

/// Gives up what the outbox still owed a chat, and why.
async fn drop_pending_of_chat(conn: &mut SqliteConnection, chat: i64, why: &str) -> eyre::Result<()> {
	sqlx::query(concat!(drop_pending!(), "chat_id = $2"))
		.bind(why)
		.bind(chat)
		.execute(&mut *conn)
		.await
		.wrap_err_with(|| format!("dropping what a chat was owed ({why})"))?;
	Ok(())
}

/// Links `chat` to the token's user, their permissions confirmed as of the token's issue. The user's
/// previous chat is let go, with what the outbox still owed it. The caller has made sure the
/// chat is no one else's.
pub async fn link(conn: &mut SqliteConnection, t: &Redeemed, chat: i64, account: &Account, now: Timestamp) -> eyre::Result<()> {
	sqlx::query(concat!(drop_pending!(), "user_id = $2 AND chat_id <> $3"))
		.bind("relinked")
		.bind(t.user_id)
		.bind(chat)
		.execute(&mut *conn)
		.await
		.wrap_err("dropping what a relinked chat was owed")?;
	sqlx::query(
		"INSERT INTO telegram_links (user_id, chat_id, linked_at, permissions, permissions_checked_at, display_name, tg_username, tg_first_name) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
		 ON CONFLICT (user_id) DO UPDATE SET chat_id = $2, linked_at = $3, permissions = $4, permissions_checked_at = $5, access_tried_at = NULL, \
		 display_name = $6, dead_at = NULL, tg_username = $7, tg_first_name = $8",
	)
	.bind(t.user_id)
	.bind(chat)
	.bind(to_db(now))
	.bind(Json(&t.permissions))
	.bind(to_db(t.issued_at))
	.bind(&t.display_name)
	.bind(&account.username)
	.bind(&account.first_name)
	.execute(&mut *conn)
	.await
	.wrap_err("linking a chat")?;
	Ok(())
}

/// Unlinks a user's chat; `false` when there was none.
pub async fn unlink_user(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<bool> {
	drop_pending_of_user(conn, user, "unlinked").await?;
	let gone = sqlx::query("DELETE FROM telegram_links WHERE user_id = $1")
		.bind(user)
		.execute(&mut *conn)
		.await
		.wrap_err("unlinking a user")?
		.rows_affected();
	Ok(gone == 1)
}

/// Unlinks a chat (`/stop`): the user it was linked to, `None` when it was not.
pub async fn unlink_chat(conn: &mut SqliteConnection, chat: i64) -> eyre::Result<Option<Uuid>> {
	drop_pending_of_chat(conn, chat, "unlinked").await?;
	sqlx::query_scalar("DELETE FROM telegram_links WHERE chat_id = $1 RETURNING user_id")
		.bind(chat)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("unlinking a chat")
}

/// The bot was blocked in `chat`: nothing more goes there until the user links again.
pub async fn chat_dead(conn: &mut SqliteConnection, chat: i64, now: Timestamp) -> eyre::Result<()> {
	drop_pending_of_chat(conn, chat, "the bot is blocked").await?;
	sqlx::query("UPDATE telegram_links SET dead_at = $2 WHERE chat_id = $1 AND dead_at IS NULL")
		.bind(chat)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("marking a chat dead")?;
	Ok(())
}

pub async fn link_of_user(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<Option<LinkRow>> {
	let row: Option<LinkDb> = sqlx::query_as(concat!(link_select!(), "WHERE user_id = $1"))
		.bind(user)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a user's link")?;
	row.map(link_row).transpose()
}

pub async fn link_of_chat(conn: &mut SqliteConnection, chat: i64) -> eyre::Result<Option<LinkRow>> {
	let row: Option<LinkDb> = sqlx::query_as(concat!(link_select!(), "WHERE chat_id = $1"))
		.bind(chat)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a chat's link")?;
	row.map(link_row).transpose()
}

/// What concierge just said of a linked user: their permissions and name.
pub async fn access_seen(conn: &mut SqliteConnection, user: Uuid, permissions: &PermissionSet, display: &str, now: Timestamp) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_links SET permissions = $2, display_name = $3, permissions_checked_at = $4, access_tried_at = $4 WHERE user_id = $1")
		.bind(user)
		.bind(Json(permissions))
		.bind(display)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("recording a user's access")?;
	Ok(())
}

/// A try to confirm a user's access that got no answer.
pub async fn access_tried(conn: &mut SqliteConnection, user: Uuid, now: Timestamp) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_links SET access_tried_at = $2 WHERE user_id = $1")
		.bind(user)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("recording an access check")?;
	Ok(())
}

/// A user's access is known to be gone: nothing more is sent to them, queued or not.
pub async fn access_lost(conn: &mut SqliteConnection, user: Uuid, now: Timestamp) -> eyre::Result<()> {
	drop_pending_of_user(conn, user, "access lost").await?;
	sqlx::query("UPDATE telegram_links SET permissions = NULL, permissions_checked_at = $2, access_tried_at = $2 WHERE user_id = $1")
		.bind(user)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("recording a user's lost access")?;
	Ok(())
}

/// Linked users whose access was last confirmed or tried before `before`, oldest first.
pub async fn stale_access(conn: &mut SqliteConnection, before: Timestamp, limit: i64) -> eyre::Result<Vec<Uuid>> {
	sqlx::query_scalar(
		"SELECT user_id FROM telegram_links WHERE dead_at IS NULL AND permissions_checked_at < $1 AND (access_tried_at IS NULL OR access_tried_at < $1) \
		 ORDER BY COALESCE(access_tried_at, permissions_checked_at) LIMIT $2",
	)
	.bind(to_db(before))
	.bind(limit)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing links to confirm")
}

// ── rules ───────────────────────────────────────────────────────────────────────────────

/// The rules a user chose; the others are at their default.
pub async fn rules(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<Vec<(Rule, bool)>> {
	let rows: Vec<(String, bool)> = sqlx::query_as("SELECT rule, enabled FROM telegram_rules WHERE user_id = $1")
		.bind(user)
		.fetch_all(&mut *conn)
		.await
		.wrap_err("reading a user's rules")?;
	rows.into_iter().map(|(r, on)| Ok((r.parse::<Rule>().wrap_err("a stored rule")?, on))).collect()
}

pub async fn set_rule(conn: &mut SqliteConnection, user: Uuid, rule: Rule, enabled: bool) -> eyre::Result<()> {
	sqlx::query("INSERT INTO telegram_rules (user_id, rule, enabled) VALUES ($1, $2, $3) ON CONFLICT (user_id, rule) DO UPDATE SET enabled = $3")
		.bind(user)
		.bind(rule.as_str())
		.bind(enabled)
		.execute(&mut *conn)
		.await
		.wrap_err("setting a rule")?;
	Ok(())
}

/// Who may get a rule's message now: linked, the chat alive, the rule on, and permissions
/// concierge confirmed at or after `confirmed_since`. Whether they open the rule is the
/// caller's to ask.
#[derive(Clone, Debug)]
pub struct Recipient {
	pub user_id: Uuid,
	pub chat_id: i64,
	pub permissions: PermissionSet,
}

pub async fn recipients(conn: &mut SqliteConnection, rule: Rule, confirmed_since: Timestamp) -> eyre::Result<Vec<Recipient>> {
	let rows: Vec<(Uuid, i64, Json<PermissionSet>)> = sqlx::query_as(
		"SELECT l.user_id, l.chat_id, l.permissions FROM telegram_links l \
		 LEFT JOIN telegram_rules r ON r.user_id = l.user_id AND r.rule = $1 \
		 WHERE l.dead_at IS NULL AND l.permissions IS NOT NULL AND l.permissions_checked_at >= $2 AND COALESCE(r.enabled, $3) \
		 ORDER BY l.user_id",
	)
	.bind(rule.as_str())
	.bind(to_db(confirmed_since))
	.bind(rule.on_by_default())
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing a rule's recipients")?;
	Ok(rows
		.into_iter()
		.map(|(user_id, chat_id, Json(permissions))| Recipient { user_id, chat_id, permissions })
		.collect())
}

// ── fan-out ─────────────────────────────────────────────────────────────────────────────

/// An active source: its key id, kind and when it was added.
pub async fn active_sources(conn: &mut SqliteConnection) -> eyre::Result<Vec<(String, String, Timestamp)>> {
	let rows: Vec<(String, String, i64)> = sqlx::query_as("SELECT key_id, kind, created_at FROM sources WHERE revoked_at IS NULL ORDER BY key_id")
		.fetch_all(&mut *conn)
		.await
		.wrap_err("listing the active sources")?;
	rows.into_iter().map(|(key_id, kind, created_at)| Ok((key_id, kind, from_db(created_at)?))).collect()
}

/// Marks `(rule, event_id)` as fanned out; `false` when it already was (by another replica).
pub async fn claim_fanout(conn: &mut SqliteConnection, rule: Rule, event_id: Uuid, now: Timestamp) -> eyre::Result<bool> {
	let done = sqlx::query("INSERT INTO telegram_fanout (rule, event_id, fanned_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
		.bind(rule.as_str())
		.bind(event_id)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("marking a fan-out")?
		.rows_affected();
	Ok(done == 1)
}

/// A message to queue.
pub struct NewMessage<'a> {
	pub user_id: Uuid,
	pub chat_id: i64,
	pub rule: Rule,
	pub event_id: Uuid,
	pub buttons: &'a [Button],
	pub text_sealed: &'a [u8],
	pub data_key_fp: [u8; 32],
	/// When what it tells of was noted: the stale rule counts from it.
	pub created_at: Timestamp,
}

/// Queues a message; `false` when `(rule, event, chat)` is queued already.
pub async fn enqueue(conn: &mut SqliteConnection, m: &NewMessage<'_>, now: Timestamp) -> eyre::Result<bool> {
	let done = sqlx::query(
		"INSERT INTO telegram_outbox (user_id, chat_id, rule, event_id, buttons, text_sealed, data_key_fp, next_attempt_at, created_at) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (rule, event_id, chat_id) DO NOTHING",
	)
	.bind(m.user_id)
	.bind(m.chat_id)
	.bind(m.rule.as_str())
	.bind(m.event_id)
	.bind(Json(m.buttons))
	.bind(m.text_sealed)
	.bind(m.data_key_fp.as_slice())
	.bind(to_db(now))
	.bind(to_db(m.created_at))
	.execute(&mut *conn)
	.await
	.wrap_err("queueing a message")?
	.rows_affected();
	Ok(done == 1)
}

/// Drops fan-out marks and finished messages older than `before`.
pub async fn prune(conn: &mut SqliteConnection, before: Timestamp) -> eyre::Result<()> {
	let before = to_db(before);
	sqlx::query("DELETE FROM telegram_fanout WHERE fanned_at < $1")
		.bind(before)
		.execute(&mut *conn)
		.await
		.wrap_err("pruning fan-out marks")?;
	sqlx::query("DELETE FROM telegram_outbox WHERE state <> 'pending' AND created_at < $1")
		.bind(before)
		.execute(&mut *conn)
		.await
		.wrap_err("pruning the outbox")?;
	Ok(())
}

// ── delivery ────────────────────────────────────────────────────────────────────────────

/// A message claimed for sending.
#[derive(Clone, Debug)]
pub struct Due {
	pub id: i64,
	pub user_id: Uuid,
	pub chat_id: i64,
	pub rule: String,
	pub event_id: Uuid,
	pub buttons: Vec<Button>,
	pub text_sealed: Vec<u8>,
	pub data_key_fp: Vec<u8>,
	/// Tries so far, this one included.
	pub attempts: i32,
}

/// Claims what may be sent at `now`, leased until `lease_until`: at most one message per chat
/// that has had none in the last [`PER_CHAT_GAP`] and none in flight, and no more than
/// [`GLOBAL_PER_SECOND`] tries started in the last second — whoever else claims, since every
/// claim reads the pace and leases inside one write transaction, one at a time. Oldest first.
pub async fn claim_due(conn: &mut SqliteConnection, now: Timestamp, lease_until: Timestamp) -> eyre::Result<Vec<Due>> {
	let mut tx = begin_write(&mut *conn).await?;
	let now_db = to_db(now);
	let paused: Option<i64> = sqlx::query_scalar("SELECT outbox_paused_until FROM telegram_poller")
		.fetch_one(&mut *tx)
		.await
		.wrap_err("reading the outbox pause")?;
	if paused.is_some_and(|p| p > now_db) {
		tx.commit().await.wrap_err("ending a claim")?;
		return Ok(Vec::new());
	}
	let second_ago = to_db(now - SignedDuration::from_secs(1));
	let gap_ago = to_db(now - PER_CHAT_GAP);
	let started: i64 = sqlx::query_scalar("SELECT count(*) FROM telegram_outbox WHERE attempted_at > $1 AND attempted_at <= $2")
		.bind(second_ago)
		.bind(now_db)
		.fetch_one(&mut *tx)
		.await
		.wrap_err("counting recent sends")?;
	let budget = (GLOBAL_PER_SECOND - started).max(0);
	if budget == 0 {
		tx.commit().await.wrap_err("ending a claim")?;
		return Ok(Vec::new());
	}
	type Row = (i64, Uuid, i64, String, Uuid, Json<Vec<Button>>, Vec<u8>, Vec<u8>, i32);
	// `pick`: each free chat's oldest due message (the lowest id of its group).
	let rows: Vec<Row> = sqlx::query_as(
		"WITH busy AS ( \
		   SELECT DISTINCT chat_id FROM telegram_outbox \
		   WHERE (attempted_at > $2 AND attempted_at <= $1) OR (state = 'pending' AND leased_until > $1) \
		 ), pick AS ( \
		   SELECT min(id) AS id FROM telegram_outbox \
		   WHERE state = 'pending' AND next_attempt_at <= $1 AND (leased_until IS NULL OR leased_until <= $1) \
		     AND chat_id NOT IN (SELECT chat_id FROM busy) \
		   GROUP BY chat_id \
		 ), chosen AS (SELECT id FROM pick ORDER BY id LIMIT $4) \
		 UPDATE telegram_outbox SET leased_until = $3, attempted_at = $1, attempts = attempts + 1 \
		 WHERE id IN (SELECT id FROM chosen) \
		 RETURNING id, user_id, chat_id, rule, event_id, buttons, text_sealed, data_key_fp, attempts",
	)
	.bind(now_db)
	.bind(gap_ago)
	.bind(to_db(lease_until))
	.bind(budget)
	.fetch_all(&mut *tx)
	.await
	.wrap_err("claiming due messages")?;
	tx.commit().await.wrap_err("committing a claim")?;
	let mut due: Vec<Due> = rows
		.into_iter()
		.map(|(id, user_id, chat_id, rule, event_id, buttons, text_sealed, data_key_fp, attempts)| Due {
			id,
			user_id,
			chat_id,
			rule,
			event_id,
			buttons: buttons.0,
			text_sealed,
			data_key_fp,
			attempts,
		})
		.collect();
	due.sort_by_key(|d| d.id);
	Ok(due)
}

pub async fn sent(conn: &mut SqliteConnection, id: i64, tg_message_id: i64, now: Timestamp) -> eyre::Result<()> {
	sqlx::query(
		"UPDATE telegram_outbox SET state = 'sent', sent_at = $3, tg_message_id = $2, text_sealed = NULL, data_key_fp = NULL, leased_until = NULL, last_error = NULL \
		 WHERE id = $1",
	)
	.bind(id)
	.bind(tg_message_id)
	.bind(to_db(now))
	.execute(&mut *conn)
	.await
	.wrap_err("recording a sent message")?;
	Ok(())
}

/// A failed try, tried again at `at`. With `whole_chat` (Telegram asked the chat to wait),
/// nothing else pending for the chat goes before `at` either.
pub async fn retry(conn: &mut SqliteConnection, id: i64, chat: i64, at: Timestamp, error: &str, whole_chat: bool) -> eyre::Result<()> {
	let at = to_db(at);
	sqlx::query("UPDATE telegram_outbox SET next_attempt_at = $2, leased_until = NULL, last_error = $3 WHERE id = $1")
		.bind(id)
		.bind(at)
		.bind(error)
		.execute(&mut *conn)
		.await
		.wrap_err("scheduling a retry")?;
	if whole_chat {
		sqlx::query("UPDATE telegram_outbox SET next_attempt_at = max(next_attempt_at, $2) WHERE chat_id = $1 AND state = 'pending'")
			.bind(chat)
			.bind(at)
			.execute(&mut *conn)
			.await
			.wrap_err("holding a chat back")?;
	}
	Ok(())
}

/// Telegram asked the bot to wait (429): nothing leaves the outbox before `until`, and the
/// try is not counted against the message — the wait was the bot's, not the chat's.
pub async fn pause(conn: &mut SqliteConnection, id: i64, until: Timestamp, error: &str) -> eyre::Result<()> {
	let until = to_db(until);
	sqlx::query("UPDATE telegram_poller SET outbox_paused_until = max(COALESCE(outbox_paused_until, $1), $1)")
		.bind(until)
		.execute(&mut *conn)
		.await
		.wrap_err("pausing the outbox")?;
	sqlx::query("UPDATE telegram_outbox SET attempts = attempts - 1, next_attempt_at = $2, leased_until = NULL, last_error = $3 WHERE id = $1")
		.bind(id)
		.bind(until)
		.bind(error)
		.execute(&mut *conn)
		.await
		.wrap_err("putting a message back")?;
	Ok(())
}

/// Gives up on lead messages queued before `before`: past it they are news nobody needs.
pub async fn expire_stale(conn: &mut SqliteConnection, before: Timestamp) -> eyre::Result<u64> {
	Ok(sqlx::query(concat!(drop_pending!(), "rule IN ('new_lead', 'contact_overdue') AND created_at < $2"))
		.bind("stale")
		.bind(to_db(before))
		.execute(&mut *conn)
		.await
		.wrap_err("expiring stale messages")?
		.rows_affected())
}

/// Chats with more than `over` lead messages waiting (none of them in flight): `(chat, user,
/// how many)`.
pub async fn crowded(conn: &mut SqliteConnection, over: i64, now: Timestamp) -> eyre::Result<Vec<(i64, Uuid, i64)>> {
	sqlx::query_as(
		"SELECT chat_id, min(user_id), count(*) FROM telegram_outbox \
		 WHERE state = 'pending' AND rule IN ('new_lead', 'contact_overdue') AND (leased_until IS NULL OR leased_until <= $2) \
		 GROUP BY chat_id HAVING count(*) > $1",
	)
	.bind(over)
	.bind(to_db(now))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("finding crowded chats")
}

/// Gives up on a chat's waiting lead messages, a summary taking their place; how many.
pub async fn fold_into_summary(conn: &mut SqliteConnection, chat: i64, now: Timestamp) -> eyre::Result<u64> {
	Ok(sqlx::query(concat!(
		drop_pending!(),
		"chat_id = $2 AND rule IN ('new_lead', 'contact_overdue') AND (leased_until IS NULL OR leased_until <= $3)"
	))
	.bind("summarized")
	.bind(chat)
	.bind(to_db(now))
	.execute(&mut *conn)
	.await
	.wrap_err("folding messages into a summary")?
	.rows_affected())
}

/// Whether the bot may send an unsolicited reply to `chat` now: at most one per `every`.
pub async fn may_reply(conn: &mut SqliteConnection, chat: i64, now: Timestamp, every: SignedDuration) -> eyre::Result<bool> {
	let taken: Option<i64> = sqlx::query_scalar(
		"INSERT INTO telegram_replies (chat_id, last_at) VALUES ($1, $2) \
		 ON CONFLICT (chat_id) DO UPDATE SET last_at = $2 WHERE telegram_replies.last_at <= $3 RETURNING chat_id",
	)
	.bind(chat)
	.bind(to_db(now))
	.bind(to_db(now - every))
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("throttling a reply")?;
	Ok(taken.is_some())
}

/// Given up on.
pub async fn dead(conn: &mut SqliteConnection, id: i64, error: &str) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_outbox SET state = 'dead', text_sealed = NULL, data_key_fp = NULL, leased_until = NULL, last_error = $2 WHERE id = $1")
		.bind(id)
		.bind(error)
		.execute(&mut *conn)
		.await
		.wrap_err("giving up on a message")?;
	Ok(())
}

/// A sent message a button names.
#[derive(Clone, Debug)]
pub struct SentMessage {
	pub user_id: Uuid,
	pub chat_id: i64,
	pub buttons: Vec<Button>,
}

pub async fn message(conn: &mut SqliteConnection, id: i64) -> eyre::Result<Option<SentMessage>> {
	type Row = (Uuid, i64, Json<Vec<Button>>);
	let row: Option<Row> = sqlx::query_as("SELECT user_id, chat_id, buttons FROM telegram_outbox WHERE id = $1 AND state = 'sent'")
		.bind(id)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a sent message")?;
	Ok(row.map(|(user_id, chat_id, Json(buttons))| SentMessage { user_id, chat_id, buttons }))
}

// ── sites' notes ────────────────────────────────────────────────────────────────────────

/// The last of sites' notes already in the outbox.
pub async fn notes_after(conn: &mut SqliteConnection) -> eyre::Result<i64> {
	sqlx::query_scalar("SELECT notes_after FROM telegram_poller")
		.fetch_one(&mut *conn)
		.await
		.wrap_err("reading the notes' cursor")
}

pub async fn set_notes_after(conn: &mut SqliteConnection, after: i64) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_poller SET notes_after = max(notes_after, $1)")
		.bind(after)
		.execute(&mut *conn)
		.await
		.wrap_err("moving the notes' cursor")?;
	Ok(())
}

// ── the poller ──────────────────────────────────────────────────────────────────────────

/// Takes or renews the poller's lease for `holder` until `until`: the next update id to ask
/// for, or `None` when another replica holds it.
pub async fn poll_lease(conn: &mut SqliteConnection, holder: Uuid, now: Timestamp, until: Timestamp) -> eyre::Result<Option<i64>> {
	sqlx::query_scalar(
		"UPDATE telegram_poller SET holder = $1, leased_until = $3 \
		 WHERE holder = $1 OR holder IS NULL OR leased_until IS NULL OR leased_until <= $2 RETURNING next_update_id",
	)
	.bind(holder)
	.bind(to_db(now))
	.bind(to_db(until))
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("leasing the poller")
}

/// Moves the offset past a handled update, if `holder` still holds the lease.
pub async fn poll_advance(conn: &mut SqliteConnection, holder: Uuid, next_update_id: i64) -> eyre::Result<bool> {
	let done = sqlx::query("UPDATE telegram_poller SET next_update_id = max(next_update_id, $2) WHERE holder = $1")
		.bind(holder)
		.bind(next_update_id)
		.execute(&mut *conn)
		.await
		.wrap_err("advancing the poller")?
		.rows_affected();
	Ok(done == 1)
}

/// Lets the lease go at shutdown, so another replica takes over at once.
pub async fn poll_release(conn: &mut SqliteConnection, holder: Uuid) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_poller SET holder = NULL, leased_until = NULL WHERE holder = $1")
		.bind(holder)
		.execute(&mut *conn)
		.await
		.wrap_err("releasing the poller")?;
	Ok(())
}

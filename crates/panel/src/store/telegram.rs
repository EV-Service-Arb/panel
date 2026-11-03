//! Telegram: link tokens, the chats destinations are reached at, the outbox, the poller's lease
//! and the bot's replies. Every timestamp is the caller's `now`, never the database's clock, so
//! the pacing can be driven by a test's virtual time.

use eyre::WrapErr;
use jiff::{SignedDuration, Timestamp};
use panel_core::notify::{GLOBAL_PER_SECOND, PER_CHAT_GAP, PER_GROUP_GAP};
use sa_alerts::{Button, Rule};
use sa_auth::PermissionSet;
use sqlx::{SqliteConnection, types::Json};
use uuid::Uuid;

use super::{alerts, begin_write, from_db, to_db};

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

// ── chats ───────────────────────────────────────────────────────────────────────────────

/// What a destination's chat is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::AsRefStr, strum::EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum Kind {
	/// The user's own chat with the bot: the only one PII and buttons go to.
	Private,
	Channel,
	/// A group, or one topic of a forum.
	Group,
}

/// The Telegram account behind a chat, as its `/start` said.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Account {
	pub username: Option<String>,
	pub first_name: Option<String>,
}

// A macro, not a const, so every query stays a literal (`concat!`) that sqlx takes as audited.
/// Gives up what the outbox still owed: the filter follows.
macro_rules! drop_pending {
	() => {
		"UPDATE telegram_outbox SET state = 'dead', text_sealed = NULL, data_key_fp = NULL, leased_until = NULL, last_error = $1 WHERE state = 'pending' AND "
	};
}

/// Gives up what the outbox still owed a user, and why.
pub(super) async fn drop_pending_of_user(conn: &mut SqliteConnection, user: Uuid, why: &str) -> eyre::Result<()> {
	sqlx::query(concat!(drop_pending!(), "user_id = $2"))
		.bind(why)
		.bind(user)
		.execute(&mut *conn)
		.await
		.wrap_err_with(|| format!("dropping what a user was owed ({why})"))?;
	Ok(())
}

/// The user's private chat: its destination and chat id.
pub async fn private_of_user(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<Option<(i64, i64)>> {
	sqlx::query_as(
		"SELECT t.destination_id, t.chat_id FROM telegram_destinations t JOIN alert_destinations d ON d.id = t.destination_id \
		 WHERE d.user_id = $1 AND t.kind = 'private'",
	)
	.bind(user)
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("reading a user's private chat")
}

/// Whose private chat `chat` is.
pub async fn owner_of_private(conn: &mut SqliteConnection, chat: i64) -> eyre::Result<Option<Uuid>> {
	sqlx::query_scalar(
		"SELECT d.user_id FROM telegram_destinations t JOIN alert_destinations d ON d.id = t.destination_id \
		 WHERE t.chat_id = $1 AND t.kind = 'private'",
	)
	.bind(chat)
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("reading a chat's link")
}

/// Links `chat` to the token's user as their private chat, their permissions confirmed as of the
/// token's issue. A chat they had before is let go, with what the outbox still owed it; the rules
/// they chose stay. The caller has made sure the chat is no one else's.
pub async fn link(conn: &mut SqliteConnection, t: &Redeemed, chat: i64, account: &Account, now: Timestamp) -> eyre::Result<()> {
	let title = account.username.as_ref().map(|u| format!("@{u}")).or_else(|| account.first_name.clone());
	match private_of_user(conn, t.user_id).await? {
		Some((destination, _)) => {
			sqlx::query(concat!(drop_pending!(), "destination_id = $2 AND chat_id <> $3"))
				.bind("relinked")
				.bind(destination)
				.bind(chat)
				.execute(&mut *conn)
				.await
				.wrap_err("dropping what a relinked chat was owed")?;
			sqlx::query("UPDATE telegram_destinations SET chat_id = $2, title = $3, dead_at = NULL, dead_reason = NULL WHERE destination_id = $1")
				.bind(destination)
				.bind(chat)
				.bind(title)
				.execute(&mut *conn)
				.await
				.wrap_err("relinking a chat")?;
		}
		None => {
			let destination = alerts::add(conn, t.user_id, None, now).await?;
			insert(conn, destination, Kind::Private, chat, None, title.as_deref()).await?;
		}
	}
	alerts::access_seen(conn, t.user_id, &t.permissions, &t.display_name, t.issued_at).await
}

/// A destination's chat.
pub async fn insert(conn: &mut SqliteConnection, destination: i64, kind: Kind, chat: i64, topic: Option<i64>, title: Option<&str>) -> eyre::Result<()> {
	sqlx::query("INSERT INTO telegram_destinations (destination_id, kind, chat_id, topic_id, title) VALUES ($1, $2, $3, $4, $5)")
		.bind(destination)
		.bind(kind.as_ref())
		.bind(chat)
		.bind(topic)
		.bind(title)
		.execute(&mut *conn)
		.await
		.wrap_err("adding a chat")?;
	Ok(())
}

/// Whether a destination stands for `chat` (and `topic`) already.
pub async fn chat_taken(conn: &mut SqliteConnection, chat: i64, topic: Option<i64>) -> eyre::Result<bool> {
	sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM telegram_destinations WHERE chat_id = $1 AND ifnull(topic_id, 0) = ifnull($2, 0))")
		.bind(chat)
		.bind(topic)
		.fetch_one(&mut *conn)
		.await
		.wrap_err("looking a chat up")
}

/// Unlinks a private chat (`/stop`): the user it was linked to, `None` when it was not.
pub async fn unlink_private(conn: &mut SqliteConnection, chat: i64) -> eyre::Result<Option<Uuid>> {
	sqlx::query_scalar(
		"DELETE FROM alert_destinations WHERE id = (SELECT destination_id FROM telegram_destinations WHERE chat_id = $1 AND kind = 'private') \
		 RETURNING user_id",
	)
	.bind(chat)
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("unlinking a chat")
}

/// Nothing reaches `chat` (the bot is blocked there, or gone from it): nothing more goes there
/// until a send succeeds again. The users whose destinations it held.
pub async fn chat_dead(conn: &mut SqliteConnection, chat: i64, why: &str, now: Timestamp) -> eyre::Result<Vec<Uuid>> {
	sqlx::query(concat!(drop_pending!(), "chat_id = $2"))
		.bind(why)
		.bind(chat)
		.execute(&mut *conn)
		.await
		.wrap_err("dropping what a dead chat was owed")?;
	let users = sqlx::query_scalar(
		"SELECT DISTINCT d.user_id FROM telegram_destinations t JOIN alert_destinations d ON d.id = t.destination_id \
		 WHERE t.chat_id = $1 AND t.dead_at IS NULL",
	)
	.bind(chat)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("reading a dead chat's users")?;
	sqlx::query("UPDATE telegram_destinations SET dead_at = $2, dead_reason = $3 WHERE chat_id = $1 AND dead_at IS NULL")
		.bind(chat)
		.bind(to_db(now))
		.bind(why)
		.execute(&mut *conn)
		.await
		.wrap_err("marking a chat dead")?;
	Ok(users)
}

/// A send to the destination succeeded: it is reached again.
pub async fn revive(conn: &mut SqliteConnection, destination: i64) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_destinations SET dead_at = NULL, dead_reason = NULL WHERE destination_id = $1")
		.bind(destination)
		.execute(&mut *conn)
		.await
		.wrap_err("reviving a destination")?;
	Ok(())
}

/// What a channel's or group's destination is, as Telegram says now.
pub async fn describe(conn: &mut SqliteConnection, destination: i64, kind: Kind, title: Option<&str>) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_destinations SET kind = $2, title = $3 WHERE destination_id = $1 AND kind <> 'private'")
		.bind(destination)
		.bind(kind.as_ref())
		.bind(title)
		.execute(&mut *conn)
		.await
		.wrap_err("describing a chat")?;
	Ok(())
}

/// A destination's chat and topic as they stand, and whether it is reached.
pub async fn reach(conn: &mut SqliteConnection, destination: i64) -> eyre::Result<Option<(i64, Option<i64>, bool)>> {
	sqlx::query_as("SELECT chat_id, topic_id, dead_at IS NULL FROM telegram_destinations WHERE destination_id = $1")
		.bind(destination)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a destination's chat")
}

// ── the outbox ──────────────────────────────────────────────────────────────────────────

/// A message to queue.
pub struct NewMessage<'a> {
	pub destination_id: i64,
	pub user_id: Uuid,
	pub chat_id: i64,
	pub topic_id: Option<i64>,
	pub rule: Rule,
	pub event_id: Uuid,
	pub buttons: &'a [Button],
	pub photo: Option<&'a str>,
	pub text_sealed: &'a [u8],
	pub data_key_fp: [u8; 32],
	/// When what it tells of was noted: the stale rule counts from it.
	pub created_at: Timestamp,
}

/// Queues a message; `false` when `(rule, event, destination)` is queued already.
pub async fn enqueue(conn: &mut SqliteConnection, m: &NewMessage<'_>, now: Timestamp) -> eyre::Result<bool> {
	let done = sqlx::query(
		"INSERT INTO telegram_outbox (destination_id, user_id, chat_id, topic_id, rule, event_id, buttons, photo, text_sealed, data_key_fp, next_attempt_at, created_at) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT (rule, event_id, destination_id) DO NOTHING",
	)
	.bind(m.destination_id)
	.bind(m.user_id)
	.bind(m.chat_id)
	.bind(m.topic_id)
	.bind(m.rule.as_str())
	.bind(m.event_id)
	.bind(Json(m.buttons))
	.bind(m.photo)
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

/// Drops finished messages older than `before`.
pub async fn prune(conn: &mut SqliteConnection, before: Timestamp) -> eyre::Result<()> {
	sqlx::query("DELETE FROM telegram_outbox WHERE state <> 'pending' AND created_at < $1")
		.bind(to_db(before))
		.execute(&mut *conn)
		.await
		.wrap_err("pruning the outbox")?;
	Ok(())
}

/// A message claimed for sending.
#[derive(Clone, Debug)]
pub struct Due {
	pub id: i64,
	pub destination_id: i64,
	pub user_id: Uuid,
	pub chat_id: i64,
	pub topic_id: Option<i64>,
	pub rule: Rule,
	pub event_id: Uuid,
	pub buttons: Vec<Button>,
	pub photo: Option<String>,
	pub text_sealed: Vec<u8>,
	pub data_key_fp: Vec<u8>,
	/// Tries so far, this one included.
	pub attempts: i32,
}

/// Claims what may be sent at `now`, leased until `lease_until`: at most one message per chat
/// that has had none in the last [`PER_CHAT_GAP`] (a private chat) or [`PER_GROUP_GAP`] (a
/// channel or group, whose ids are negative) and none in flight, and no more than
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
	let started: i64 = sqlx::query_scalar("SELECT count(*) FROM telegram_outbox WHERE attempted_at > $1 AND attempted_at <= $2")
		.bind(to_db(now - SignedDuration::from_secs(1)))
		.bind(now_db)
		.fetch_one(&mut *tx)
		.await
		.wrap_err("counting recent sends")?;
	let budget = (GLOBAL_PER_SECOND - started).max(0);
	if budget == 0 {
		tx.commit().await.wrap_err("ending a claim")?;
		return Ok(Vec::new());
	}
	type Row = (i64, i64, Uuid, i64, Option<i64>, String, Uuid, Json<Vec<Button>>, Option<String>, Vec<u8>, Vec<u8>, i32);
	// `pick`: each free chat's oldest due message (the lowest id of its group).
	let rows: Vec<Row> = sqlx::query_as(
		"WITH busy AS ( \
		   SELECT DISTINCT chat_id FROM telegram_outbox \
		   WHERE (attempted_at > CASE WHEN chat_id < 0 THEN $5 ELSE $2 END AND attempted_at <= $1) OR (state = 'pending' AND leased_until > $1) \
		 ), pick AS ( \
		   SELECT min(id) AS id FROM telegram_outbox \
		   WHERE state = 'pending' AND next_attempt_at <= $1 AND (leased_until IS NULL OR leased_until <= $1) \
		     AND chat_id NOT IN (SELECT chat_id FROM busy) \
		   GROUP BY chat_id \
		 ), chosen AS (SELECT id FROM pick ORDER BY id LIMIT $4) \
		 UPDATE telegram_outbox SET leased_until = $3, attempted_at = $1, attempts = attempts + 1 \
		 WHERE id IN (SELECT id FROM chosen) \
		 RETURNING id, destination_id, user_id, chat_id, topic_id, rule, event_id, buttons, photo, text_sealed, data_key_fp, attempts",
	)
	.bind(now_db)
	.bind(to_db(now - PER_CHAT_GAP))
	.bind(to_db(lease_until))
	.bind(budget)
	.bind(to_db(now - PER_GROUP_GAP))
	.fetch_all(&mut *tx)
	.await
	.wrap_err("claiming due messages")?;
	tx.commit().await.wrap_err("committing a claim")?;
	let mut due = rows
		.into_iter()
		.map(
			|(id, destination_id, user_id, chat_id, topic_id, rule, event_id, Json(buttons), photo, text_sealed, data_key_fp, attempts)| {
				Ok(Due {
					id,
					destination_id,
					user_id,
					chat_id,
					topic_id,
					rule: rule.parse().wrap_err_with(|| format!("message {id}'s rule"))?,
					event_id,
					buttons,
					photo,
					text_sealed,
					data_key_fp,
					attempts,
				})
			},
		)
		.collect::<eyre::Result<Vec<_>>>()?;
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

/// A failed try, tried again at `at`.
pub async fn retry(conn: &mut SqliteConnection, id: i64, at: Timestamp, error: &str) -> eyre::Result<()> {
	sqlx::query("UPDATE telegram_outbox SET next_attempt_at = $2, leased_until = NULL, last_error = $3 WHERE id = $1")
		.bind(id)
		.bind(to_db(at))
		.bind(error)
		.execute(&mut *conn)
		.await
		.wrap_err("scheduling a retry")?;
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

/// A destination with more than its share of one rule's messages waiting.
pub struct Crowded {
	pub destination_id: i64,
	pub user_id: Uuid,
	pub chat_id: i64,
	pub topic_id: Option<i64>,
	pub rule: Rule,
}

/// Destinations with more than `over` messages of one rule waiting (none of them in flight).
pub async fn crowded(conn: &mut SqliteConnection, over: i64, now: Timestamp) -> eyre::Result<Vec<Crowded>> {
	let rows: Vec<(i64, Uuid, i64, Option<i64>, String)> = sqlx::query_as(
		"SELECT destination_id, min(user_id), min(chat_id), min(topic_id), rule FROM telegram_outbox \
		 WHERE state = 'pending' AND (leased_until IS NULL OR leased_until <= $2) \
		 GROUP BY destination_id, rule HAVING count(*) > $1",
	)
	.bind(over)
	.bind(to_db(now))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("finding crowded destinations")?;
	rows.into_iter()
		.map(|(destination_id, user_id, chat_id, topic_id, rule)| {
			Ok(Crowded {
				destination_id,
				user_id,
				chat_id,
				topic_id,
				rule: rule.parse()?,
			})
		})
		.collect()
}

/// Gives up on a destination's waiting messages of `rule`, a summary taking their place; how many.
pub async fn fold_into_summary(conn: &mut SqliteConnection, destination: i64, rule: Rule, now: Timestamp) -> eyre::Result<u64> {
	Ok(
		sqlx::query(concat!(drop_pending!(), "destination_id = $2 AND rule = $3 AND (leased_until IS NULL OR leased_until <= $4)"))
			.bind("summarized")
			.bind(destination)
			.bind(rule.as_str())
			.bind(to_db(now))
			.execute(&mut *conn)
			.await
			.wrap_err("folding messages into a summary")?
			.rows_affected(),
	)
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

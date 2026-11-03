//! Alerts: what concierge last said each user holds, their destinations and the rules on each,
//! the fan-out marks and the services' feed cursors. A destination's method keeps the rest
//! (`telegram`). Every timestamp is the caller's `now`.

use eyre::WrapErr;
use jiff::Timestamp;
use sa_alerts::{Origin, Rule};
use sa_auth::PermissionSet;
use sqlx::{SqliteConnection, types::Json};
use uuid::Uuid;

use super::{from_db, telegram::Kind, to_db};

// ── access ──────────────────────────────────────────────────────────────────────────────

/// What concierge said of a user at `at`: their permissions and name. An answer older than the
/// one kept is not taken (a link token's, redeemed after the gate asked again).
pub async fn access_seen(conn: &mut SqliteConnection, user: Uuid, permissions: &PermissionSet, display: &str, at: Timestamp) -> eyre::Result<()> {
	sqlx::query(
		"INSERT INTO alert_access (user_id, permissions, confirmed_at, tried_at, display_name) VALUES ($1, $2, $3, $3, $4) \
		 ON CONFLICT (user_id) DO UPDATE SET permissions = $2, confirmed_at = $3, tried_at = max(coalesce(tried_at, $3), $3), display_name = $4 \
		 WHERE excluded.confirmed_at >= alert_access.confirmed_at",
	)
	.bind(user)
	.bind(Json(permissions))
	.bind(to_db(at))
	.bind(display)
	.execute(&mut *conn)
	.await
	.wrap_err("recording a user's access")?;
	Ok(())
}

/// A try to confirm a user's access that got no answer.
pub async fn access_tried(conn: &mut SqliteConnection, user: Uuid, now: Timestamp) -> eyre::Result<()> {
	sqlx::query("UPDATE alert_access SET tried_at = $2 WHERE user_id = $1")
		.bind(user)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("recording an access check")?;
	Ok(())
}

/// A user's access is known to be gone: nothing more is sent to them, queued or not.
pub async fn access_lost(conn: &mut SqliteConnection, user: Uuid, now: Timestamp) -> eyre::Result<()> {
	super::telegram::drop_pending_of_user(conn, user, "access lost").await?;
	sqlx::query("UPDATE alert_access SET permissions = NULL, confirmed_at = $2, tried_at = $2 WHERE user_id = $1")
		.bind(user)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("recording a user's lost access")?;
	Ok(())
}

/// What concierge last confirmed of a user, and when: `None` inside once their access is known
/// lost.
pub async fn access(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<Option<(Option<PermissionSet>, Timestamp)>> {
	let row: Option<(Option<Json<PermissionSet>>, i64)> = sqlx::query_as("SELECT permissions, confirmed_at FROM alert_access WHERE user_id = $1")
		.bind(user)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a user's access")?;
	row.map(|(permissions, at)| Ok((permissions.map(|Json(p)| p), from_db(at)?))).transpose()
}

/// Users with a destination still reached whose access was last confirmed or tried before
/// `before`, oldest first.
pub async fn stale_access(conn: &mut SqliteConnection, before: Timestamp, limit: i64) -> eyre::Result<Vec<Uuid>> {
	sqlx::query_scalar(
		"SELECT a.user_id FROM alert_access a \
		 WHERE a.confirmed_at < $1 AND (a.tried_at IS NULL OR a.tried_at < $1) \
		 AND EXISTS (SELECT 1 FROM alert_destinations d JOIN telegram_destinations t ON t.destination_id = d.id \
		             WHERE d.user_id = a.user_id AND t.dead_at IS NULL) \
		 ORDER BY coalesce(a.tried_at, a.confirmed_at) LIMIT $2",
	)
	.bind(to_db(before))
	.bind(limit)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing users to confirm")
}

// ── destinations ────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
	pub id: i64,
	pub user_id: Uuid,
	pub gmail_id: Option<i64>,
	pub kind: Kind,
	pub chat_id: i64,
	pub topic_id: Option<i64>,
	pub title: Option<String>,
	/// Why nothing reaches it, when nothing does.
	pub dead: Option<String>,
	/// The rules turned on or off there; the others are at their default.
	pub chosen: Vec<(Rule, bool)>,
}

type DestinationRow = (i64, Uuid, Option<i64>, String, i64, Option<i64>, Option<String>, Option<String>, Json<Vec<(String, bool)>>);

macro_rules! destination_select {
	() => {
		"SELECT d.id, d.user_id, d.gmail_id, t.kind, t.chat_id, t.topic_id, t.title, t.dead_reason, \
		 (SELECT json_group_array(json_array(r.rule, json(CASE r.enabled WHEN 1 THEN 'true' ELSE 'false' END))) \
		  FROM alert_rules r WHERE r.destination_id = d.id) \
		 FROM alert_destinations d JOIN telegram_destinations t ON t.destination_id = d.id "
	};
}

fn destination((id, user_id, gmail_id, kind, chat_id, topic_id, title, dead, Json(chosen)): DestinationRow) -> eyre::Result<Destination> {
	Ok(Destination {
		id,
		user_id,
		gmail_id,
		kind: kind.parse()?,
		chat_id,
		topic_id,
		title,
		dead,
		chosen: chosen
			.into_iter()
			.map(|(rule, on)| Ok((rule.parse::<Rule>().wrap_err("a stored rule")?, on)))
			.collect::<eyre::Result<_>>()?,
	})
}

/// The user's destinations, oldest first.
pub async fn destinations(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<Vec<Destination>> {
	let rows: Vec<DestinationRow> = sqlx::query_as(concat!(destination_select!(), "WHERE d.user_id = $1 ORDER BY d.id"))
		.bind(user)
		.fetch_all(&mut *conn)
		.await
		.wrap_err("listing a user's destinations")?;
	rows.into_iter().map(destination).collect()
}

/// One of the user's destinations.
pub async fn destination_of(conn: &mut SqliteConnection, user: Uuid, id: i64) -> eyre::Result<Option<Destination>> {
	let row: Option<DestinationRow> = sqlx::query_as(concat!(destination_select!(), "WHERE d.user_id = $1 AND d.id = $2"))
		.bind(user)
		.bind(id)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading a destination")?;
	row.map(destination).transpose()
}

/// A new destination of the user's; its method's row is the caller's to add.
pub async fn add(conn: &mut SqliteConnection, user: Uuid, gmail: Option<i64>, now: Timestamp) -> eyre::Result<i64> {
	sqlx::query_scalar("INSERT INTO alert_destinations (user_id, method, gmail_id, created_at) VALUES ($1, 'telegram', $2, $3) RETURNING id")
		.bind(user)
		.bind(gmail)
		.bind(to_db(now))
		.fetch_one(&mut *conn)
		.await
		.wrap_err("adding a destination")
}

/// Removes one of the user's destinations, its rules and its messages; `false` when it was not
/// theirs.
pub async fn remove(conn: &mut SqliteConnection, user: Uuid, id: i64) -> eyre::Result<bool> {
	let gone = sqlx::query("DELETE FROM alert_destinations WHERE id = $1 AND user_id = $2")
		.bind(id)
		.bind(user)
		.execute(&mut *conn)
		.await
		.wrap_err("removing a destination")?
		.rows_affected();
	Ok(gone == 1)
}

pub async fn set_rule(conn: &mut SqliteConnection, destination: i64, rule: Rule, enabled: bool) -> eyre::Result<()> {
	sqlx::query("INSERT INTO alert_rules (destination_id, rule, enabled) VALUES ($1, $2, $3) ON CONFLICT (destination_id, rule) DO UPDATE SET enabled = $3")
		.bind(destination)
		.bind(rule.as_str())
		.bind(enabled)
		.execute(&mut *conn)
		.await
		.wrap_err("setting a rule")?;
	Ok(())
}

/// Scopes the user's destination to one of their gmails not removed, or to none; `false` when
/// either is not theirs.
pub async fn set_gmail(conn: &mut SqliteConnection, user: Uuid, destination: i64, gmail: Option<i64>) -> eyre::Result<bool> {
	let done = sqlx::query(
		"UPDATE alert_destinations SET gmail_id = $3 WHERE id = $1 AND user_id = $2 \
		 AND ($3 IS NULL OR EXISTS (SELECT 1 FROM gmails WHERE id = $3 AND owner = $2 AND removed_at IS NULL))",
	)
	.bind(destination)
	.bind(user)
	.bind(gmail)
	.execute(&mut *conn)
	.await
	.wrap_err("scoping a destination")?
	.rows_affected();
	Ok(done == 1)
}

/// Whether `gmail` is the user's and not removed.
pub async fn owns_gmail(conn: &mut SqliteConnection, user: Uuid, gmail: i64) -> eyre::Result<bool> {
	sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM gmails WHERE id = $1 AND owner = $2 AND removed_at IS NULL)")
		.bind(gmail)
		.bind(user)
		.fetch_one(&mut *conn)
		.await
		.wrap_err("looking a gmail up")
}

/// Whether the user has a GMB not removed, under a gmail not removed: the GMB rules are theirs.
pub async fn has_gmbs(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<bool> {
	sqlx::query_scalar(
		"SELECT EXISTS (SELECT 1 FROM gmbs b JOIN gmails g ON g.id = b.gmail_id \
		 WHERE g.owner = $1 AND g.removed_at IS NULL AND b.removed_at IS NULL)",
	)
	.bind(user)
	.fetch_one(&mut *conn)
	.await
	.wrap_err("looking a user's GMBs up")
}

/// A destination a rule's message may go to now: reached, the rule on there, its user's access
/// confirmed at or after `confirmed_since`. Whether their permissions open the rule is the
/// caller's to ask.
#[derive(Clone, Debug)]
pub struct Recipient {
	pub destination_id: i64,
	pub user_id: Uuid,
	pub permissions: PermissionSet,
	pub chat_id: i64,
	pub topic_id: Option<i64>,
	pub private: bool,
}

/// `gmbs`: for a GMB rule, the GMBs its note names; only destinations of their owners (the GMB
/// and its gmail not removed, the GMB on), scoped to none or to one of those gmails.
pub async fn recipients(conn: &mut SqliteConnection, rule: Rule, confirmed_since: Timestamp, gmbs: Option<&[i64]>) -> eyre::Result<Vec<Recipient>> {
	let rows: Vec<(i64, Uuid, Json<PermissionSet>, i64, Option<i64>, String)> = sqlx::query_as(
		"SELECT d.id, d.user_id, a.permissions, t.chat_id, t.topic_id, t.kind FROM alert_destinations d \
		 JOIN telegram_destinations t ON t.destination_id = d.id \
		 JOIN alert_access a ON a.user_id = d.user_id \
		 LEFT JOIN alert_rules r ON r.destination_id = d.id AND r.rule = $1 \
		 WHERE t.dead_at IS NULL AND a.permissions IS NOT NULL AND a.confirmed_at >= $2 AND coalesce(r.enabled, $3) \
		 AND ($4 IS NULL OR EXISTS ( \
		   SELECT 1 FROM json_each($4) j JOIN gmbs b ON b.id = j.value JOIN gmails g ON g.id = b.gmail_id \
		   WHERE g.owner = d.user_id AND g.removed_at IS NULL AND b.removed_at IS NULL \
		   AND NOT EXISTS (SELECT 1 FROM gmbs_off o WHERE o.gmb_id = b.id) \
		   AND (d.gmail_id IS NULL OR d.gmail_id = g.id))) \
		 ORDER BY d.id",
	)
	.bind(rule.as_str())
	.bind(to_db(confirmed_since))
	.bind(rule.on_by_default())
	.bind(gmbs.map(Json))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing a rule's recipients")?;
	rows.into_iter()
		.map(|(destination_id, user_id, Json(permissions), chat_id, topic_id, kind)| {
			Ok(Recipient {
				destination_id,
				user_id,
				permissions,
				chat_id,
				topic_id,
				private: kind.parse::<Kind>()? == Kind::Private,
			})
		})
		.collect()
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
	let done = sqlx::query("INSERT INTO alert_fanout (rule, event_id, fanned_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
		.bind(rule.as_str())
		.bind(event_id)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("marking a fan-out")?
		.rows_affected();
	Ok(done == 1)
}

/// Drops fan-out marks older than `before`.
pub async fn prune(conn: &mut SqliteConnection, before: Timestamp) -> eyre::Result<()> {
	sqlx::query("DELETE FROM alert_fanout WHERE fanned_at < $1")
		.bind(to_db(before))
		.execute(&mut *conn)
		.await
		.wrap_err("pruning fan-out marks")?;
	Ok(())
}

// ── the feeds ───────────────────────────────────────────────────────────────────────────

/// The last of a service's notes already fanned out.
pub async fn feed_after(conn: &mut SqliteConnection, service: Origin) -> eyre::Result<i64> {
	sqlx::query_scalar("SELECT after FROM alert_feeds WHERE service = $1")
		.bind(service.as_str())
		.fetch_one(&mut *conn)
		.await
		.wrap_err_with(|| format!("reading {service}'s feed cursor"))
}

/// Moves a service's cursor from `after` to `next`; `false` when it no longer stood at `after`
/// (another replica took the page).
pub async fn move_feed(conn: &mut SqliteConnection, service: Origin, after: i64, next: i64) -> eyre::Result<bool> {
	let moved = sqlx::query("UPDATE alert_feeds SET after = $3 WHERE service = $1 AND after = $2")
		.bind(service.as_str())
		.bind(after)
		.bind(next)
		.execute(&mut *conn)
		.await
		.wrap_err_with(|| format!("moving {service}'s feed cursor"))?
		.rows_affected();
	Ok(moved == 1)
}

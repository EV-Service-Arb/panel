//! Access requests: a signed-in user asking the admins for a permission or an alias.

use eyre::WrapErr;
use jiff::Timestamp;
use sqlx::SqliteConnection;
use uuid::Uuid;

use super::{from_db, to_db};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessRequest {
	pub user_id: Uuid,
	pub need: String,
	pub email: String,
	pub name: String,
	pub requested_at: Timestamp,
	/// What the admins' message fans out under.
	pub event_id: Uuid,
}

type Row = (Uuid, String, String, String, i64, Uuid);

macro_rules! columns {
	() => {
		"user_id, need, email, name, requested_at, event_id"
	};
}

fn row((user_id, need, email, name, requested_at, event_id): Row) -> eyre::Result<AccessRequest> {
	Ok(AccessRequest {
		user_id,
		need,
		email,
		name,
		requested_at: from_db(requested_at)?,
		event_id,
	})
}

pub async fn of_user(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<Vec<AccessRequest>> {
	let rows: Vec<Row> = sqlx::query_as(concat!("SELECT ", columns!(), " FROM access_requests WHERE user_id = $1 ORDER BY need"))
		.bind(user)
		.fetch_all(&mut *conn)
		.await
		.wrap_err("reading a user's access requests")?;
	rows.into_iter().map(row).collect()
}

/// Inserts the request, or replaces the user's one for the same need.
pub async fn put(conn: &mut SqliteConnection, r: &AccessRequest) -> eyre::Result<()> {
	sqlx::query(
		"INSERT INTO access_requests (user_id, need, email, name, requested_at, event_id) VALUES ($1, $2, $3, $4, $5, $6) \
		 ON CONFLICT (user_id, need) DO UPDATE SET email = $3, name = $4, requested_at = $5, event_id = $6",
	)
	.bind(r.user_id)
	.bind(&r.need)
	.bind(&r.email)
	.bind(&r.name)
	.bind(to_db(r.requested_at))
	.bind(r.event_id)
	.execute(&mut *conn)
	.await
	.wrap_err("storing an access request")?;
	Ok(())
}

pub async fn delete(conn: &mut SqliteConnection, user: Uuid, need: &str) -> eyre::Result<()> {
	sqlx::query("DELETE FROM access_requests WHERE user_id = $1 AND need = $2")
		.bind(user)
		.bind(need)
		.execute(&mut *conn)
		.await
		.wrap_err("dropping an access request")?;
	Ok(())
}

/// Requests made since `since` not yet fanned out to the admins.
pub async fn to_tell(conn: &mut SqliteConnection, since: Timestamp, limit: i64) -> eyre::Result<Vec<AccessRequest>> {
	let rows: Vec<Row> = sqlx::query_as(concat!(
		"SELECT ",
		columns!(),
		" FROM access_requests a WHERE requested_at >= $1 \
		 AND NOT EXISTS (SELECT 1 FROM alert_fanout t WHERE t.rule = 'access_requested' AND t.event_id = a.event_id) \
		 ORDER BY requested_at LIMIT $2"
	))
	.bind(to_db(since))
	.bind(limit)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("finding access requests to tell")?;
	rows.into_iter().map(row).collect()
}

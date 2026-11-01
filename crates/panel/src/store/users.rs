//! The people concierge has told the conductor of.

use eyre::WrapErr;
use jiff::Timestamp;
use sqlx::SqliteConnection;
use uuid::Uuid;

use super::to_db;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct User {
	pub user_id: Uuid,
	pub email: String,
	pub name: String,
}

/// Seen between `first` and `last`: the email and name kept are the latest seen.
pub async fn seen(conn: &mut SqliteConnection, user: &User, first: Timestamp, last: Timestamp) -> eyre::Result<()> {
	sqlx::query(
		"INSERT INTO users (user_id, email, name, first_seen, last_seen) VALUES ($1, $2, $3, $4, $5) \
		 ON CONFLICT (user_id) DO UPDATE SET \
		 email = CASE WHEN excluded.last_seen >= last_seen THEN excluded.email ELSE email END, \
		 name = CASE WHEN excluded.last_seen >= last_seen THEN excluded.name ELSE name END, \
		 first_seen = min(first_seen, excluded.first_seen), last_seen = max(last_seen, excluded.last_seen)",
	)
	.bind(user.user_id)
	.bind(&user.email)
	.bind(&user.name)
	.bind(to_db(first))
	.bind(to_db(last))
	.execute(&mut *conn)
	.await
	.wrap_err("recording a user seen")?;
	Ok(())
}

pub async fn all(conn: &mut SqliteConnection) -> eyre::Result<Vec<User>> {
	let rows: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT user_id, email, name FROM users ORDER BY email, user_id")
		.fetch_all(&mut *conn)
		.await
		.wrap_err("listing users")?;
	Ok(rows.into_iter().map(|(user_id, email, name)| User { user_id, email, name }).collect())
}

pub async fn exists(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<bool> {
	sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE user_id = $1)")
		.bind(user)
		.fetch_one(&mut *conn)
		.await
		.wrap_err("looking a user up")
}

/// Everyone last seen with `email`.
pub async fn by_email(conn: &mut SqliteConnection, email: &str) -> eyre::Result<Vec<Uuid>> {
	sqlx::query_scalar("SELECT user_id FROM users WHERE email = $1")
		.bind(email)
		.fetch_all(&mut *conn)
		.await
		.wrap_err("looking a user up by email")
}

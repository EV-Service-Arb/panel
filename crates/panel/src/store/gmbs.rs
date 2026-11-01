//! People's gmails and the GMBs each manages. Never deleted: a removed row keeps its id, which
//! the services hold.

use std::collections::BTreeSet;

use eyre::WrapErr;
use jiff::Timestamp;
use sqlx::SqliteConnection;
use uuid::Uuid;

use super::{from_db, to_db};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Gmail {
	pub id: i64,
	pub owner: Uuid,
	pub gmail: String,
	pub created_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Gmb {
	pub id: i64,
	pub gmail_id: i64,
	pub place_id: String,
	pub name: Option<String>,
	pub address: Option<String>,
	pub gbp_account: Option<String>,
	pub gbp_location: Option<String>,
	pub created_at: Timestamp,
	/// Its owner's switch.
	pub enabled: bool,
}

/// A GMB as the services are told of it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Managed {
	pub id: i64,
	pub gmail_id: i64,
	pub gmail: String,
	pub owner: Uuid,
	/// The GMB's or its gmail's.
	pub removed: bool,
	pub place_id: String,
	/// Its owner's switch.
	pub enabled: bool,
}

type GmailRow = (i64, Uuid, String, i64);
type GmbRow = (i64, i64, String, Option<String>, Option<String>, Option<String>, Option<String>, i64, bool);

fn gmail((id, owner, gmail, created_at): GmailRow) -> eyre::Result<Gmail> {
	Ok(Gmail {
		id,
		owner,
		gmail,
		created_at: from_db(created_at)?,
	})
}

fn gmb((id, gmail_id, place_id, name, address, gbp_account, gbp_location, created_at, enabled): GmbRow) -> eyre::Result<Gmb> {
	Ok(Gmb {
		id,
		gmail_id,
		place_id,
		name,
		address,
		gbp_account,
		gbp_location,
		created_at: from_db(created_at)?,
		enabled,
	})
}

/// The owner's gmails not removed, by address.
pub async fn gmails(conn: &mut SqliteConnection, owner: Uuid) -> eyre::Result<Vec<Gmail>> {
	let rows: Vec<GmailRow> = sqlx::query_as("SELECT id, owner, gmail, created_at FROM gmails WHERE owner = $1 AND removed_at IS NULL ORDER BY gmail")
		.bind(owner)
		.fetch_all(&mut *conn)
		.await
		.wrap_err("listing gmails")?;
	rows.into_iter().map(gmail).collect()
}

/// The GMBs not removed under the owner's gmails not removed, oldest first.
pub async fn gmbs(conn: &mut SqliteConnection, owner: Uuid) -> eyre::Result<Vec<Gmb>> {
	let rows: Vec<GmbRow> = sqlx::query_as(
		"SELECT b.id, b.gmail_id, b.place_id, b.name, b.address, b.gbp_account, b.gbp_location, b.created_at, \
		 NOT EXISTS (SELECT 1 FROM gmbs_off o WHERE o.gmb_id = b.id) \
		 FROM gmbs b JOIN gmails g ON g.id = b.gmail_id \
		 WHERE g.owner = $1 AND g.removed_at IS NULL AND b.removed_at IS NULL ORDER BY b.id",
	)
	.bind(owner)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing GMBs")?;
	rows.into_iter().map(gmb).collect()
}

/// `id` explicit only on import. `None`: the owner has that gmail already.
pub async fn add_gmail(conn: &mut SqliteConnection, id: Option<i64>, owner: Uuid, address: &str, now: Timestamp) -> eyre::Result<Option<Gmail>> {
	let row: Option<GmailRow> = sqlx::query_as(
		"INSERT INTO gmails (id, owner, gmail, created_at) VALUES ($1, $2, $3, $4) \
		 ON CONFLICT DO NOTHING RETURNING id, owner, gmail, created_at",
	)
	.bind(id)
	.bind(owner)
	.bind(address)
	.bind(to_db(now))
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("adding a gmail")?;
	row.map(gmail).transpose()
}

/// `false`: no such gmail of the owner's, or it is removed already.
pub async fn remove_gmail(conn: &mut SqliteConnection, owner: Uuid, id: i64, now: Timestamp) -> eyre::Result<bool> {
	let done = sqlx::query("UPDATE gmails SET removed_at = $3 WHERE id = $1 AND owner = $2 AND removed_at IS NULL")
		.bind(id)
		.bind(owner)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("removing a gmail")?;
	Ok(done.rows_affected() == 1)
}

/// The owner of a gmail not removed.
pub async fn gmail_owner(conn: &mut SqliteConnection, id: i64) -> eyre::Result<Option<Uuid>> {
	sqlx::query_scalar("SELECT owner FROM gmails WHERE id = $1 AND removed_at IS NULL")
		.bind(id)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("loading a gmail")
}

/// A GMB under `gmail`: `id` explicit only on import. `None`: the gmail has that place already.
pub async fn add_gmb(conn: &mut SqliteConnection, id: Option<i64>, gmail_id: i64, place: &sa_places::Place, now: Timestamp) -> eyre::Result<Option<Gmb>> {
	let row: Option<GmbRow> = sqlx::query_as(
		"INSERT INTO gmbs (id, gmail_id, place_id, name, address, created_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING \
		 RETURNING id, gmail_id, place_id, name, address, gbp_account, gbp_location, created_at, TRUE", // an id is never reused, so never off
	)
	.bind(id)
	.bind(gmail_id)
	.bind(&place.place_id)
	.bind(&place.name)
	.bind(&place.address)
	.bind(to_db(now))
	.fetch_optional(&mut *conn)
	.await
	.wrap_err("adding a GMB")?;
	row.map(gmb).transpose()
}

/// `false`: no such GMB under a gmail of the owner's, or one of them is removed already.
pub async fn remove_gmb(conn: &mut SqliteConnection, owner: Uuid, id: i64, now: Timestamp) -> eyre::Result<bool> {
	let done = sqlx::query(
		"UPDATE gmbs SET removed_at = $3 WHERE id = $1 AND removed_at IS NULL \
		 AND gmail_id IN (SELECT id FROM gmails WHERE owner = $2 AND removed_at IS NULL)",
	)
	.bind(id)
	.bind(owner)
	.bind(to_db(now))
	.execute(&mut *conn)
	.await
	.wrap_err("removing a GMB")?;
	Ok(done.rows_affected() == 1)
}

/// Each of `ids` on or off, or none of them: `false` when one is no GMB, not removed, under a
/// gmail of the owner's not removed.
pub async fn switch(conn: &mut SqliteConnection, owner: Uuid, ids: &BTreeSet<i64>, on: bool) -> eyre::Result<bool> {
	let asked = i64::try_from(ids.len()).expect("a few ids");
	let ids = serde_json::to_string(ids).expect("ids serialize");
	let theirs: i64 = sqlx::query_scalar(
		"SELECT COUNT(*) FROM gmbs b JOIN gmails g ON g.id = b.gmail_id \
		 WHERE b.id IN (SELECT value FROM json_each($1)) AND g.owner = $2 AND g.removed_at IS NULL AND b.removed_at IS NULL",
	)
	.bind(&ids)
	.bind(owner)
	.fetch_one(&mut *conn)
	.await
	.wrap_err("checking whose GMBs these are")?;
	if theirs != asked {
		return Ok(false);
	}
	let sql = match on {
		true => "DELETE FROM gmbs_off WHERE gmb_id IN (SELECT value FROM json_each($1))",
		// `WHERE true`: SQLite would read ON CONFLICT as the SELECT's join
		false => "INSERT INTO gmbs_off (gmb_id) SELECT value FROM json_each($1) WHERE true ON CONFLICT DO NOTHING",
	};
	sqlx::query(sql).bind(&ids).execute(&mut *conn).await.wrap_err("switching GMBs")?;
	Ok(true)
}

/// Every GMB there ever was, by id.
pub async fn managed(conn: &mut SqliteConnection) -> eyre::Result<Vec<Managed>> {
	type Row = (i64, i64, String, Uuid, bool, String, bool);
	let rows: Vec<Row> = sqlx::query_as(
		"SELECT b.id, b.gmail_id, g.gmail, g.owner, b.removed_at IS NOT NULL OR g.removed_at IS NOT NULL, b.place_id, \
		 NOT EXISTS (SELECT 1 FROM gmbs_off o WHERE o.gmb_id = b.id) \
		 FROM gmbs b JOIN gmails g ON g.id = b.gmail_id ORDER BY b.id",
	)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("listing every GMB")?;
	Ok(rows
		.into_iter()
		.map(|(id, gmail_id, gmail, owner, removed, place_id, enabled)| Managed {
			id,
			gmail_id,
			gmail,
			owner,
			removed,
			place_id,
			enabled,
		})
		.collect())
}

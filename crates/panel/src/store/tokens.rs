//! The token ledger, whose rows are only ever added: a balance is its user's sum. What a user is
//! owed until they claim it, and the cursor of review_archive's charges.

use eyre::WrapErr;
use jiff::Timestamp;
use sa_core::tokens::{Renewal, accrue};
use sqlx::SqliteConnection;
use uuid::Uuid;

use super::{from_db, to_db};

/// The service whose charges the ledger follows, as its rows name it.
pub const REVIEW_ARCHIVE: &str = "review_archive";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
	/// Renewal, or a first balance.
	Accrual,
	Add,
	Set,
	Charge,
	/// A kick-back claimed.
	Kickback,
}

impl Kind {
	pub const ALL: [Self; 5] = [Self::Accrual, Self::Add, Self::Set, Self::Charge, Self::Kickback];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Accrual => "accrual",
			Self::Add => "add",
			Self::Set => "set",
			Self::Charge => "charge",
			Self::Kickback => "kickback",
		}
	}
}

impl std::str::FromStr for Kind {
	type Err = eyre::Report;

	fn from_str(s: &str) -> eyre::Result<Self> {
		Ok(match s {
			"accrual" => Self::Accrual,
			"add" => Self::Add,
			"set" => Self::Set,
			"charge" => Self::Charge,
			"kickback" => Self::Kickback,
			other => eyre::bail!("ledger kind {other:?}"),
		})
	}
}

/// What a service charged for: `key` its own (`run:42`), `label` what the person reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Charged {
	pub service: String,
	pub key: String,
	pub label: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
	pub at: Timestamp,
	pub delta: i64,
	pub kind: Kind,
	pub charged: Option<Charged>,
	/// The admin's email.
	pub by: Option<String>,
	pub note: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Owed {
	/// A charge kicked back: the walk failed.
	Charge(Charged),
	Grant {
		by: String,
		note: String,
	},
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Kickback {
	pub id: i64,
	pub at: Timestamp,
	pub amount: i64,
	pub why: Owed,
}

pub async fn append(conn: &mut SqliteConnection, user: Uuid, e: &Entry) -> eyre::Result<()> {
	let c = e.charged.as_ref();
	sqlx::query("INSERT INTO token_ledger (user_id, at, delta, kind, service, key, label, by_email, note) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
		.bind(user)
		.bind(to_db(e.at))
		.bind(e.delta)
		.bind(e.kind.as_str())
		.bind(c.map(|c| &c.service))
		.bind(c.map(|c| &c.key))
		.bind(c.and_then(|c| c.label.as_ref()))
		.bind(&e.by)
		.bind(&e.note)
		.execute(&mut *conn)
		.await
		.wrap_err("writing to the ledger")?;
	Ok(())
}

/// The user's balance, renewed up to `now` inside the caller's write. Seen for the first time,
/// a user starts with [`Renewal::start`].
pub async fn renewed(conn: &mut SqliteConnection, user: Uuid, now: Timestamp, r: &Renewal) -> eyre::Result<i64> {
	let (balance, last): (i64, Option<i64>) = sqlx::query_as("SELECT COALESCE(SUM(delta), 0), MAX(CASE WHEN kind = 'accrual' THEN at END) FROM token_ledger WHERE user_id = $1")
		.bind(user)
		.fetch_one(&mut *conn)
		.await
		.wrap_err("summing a balance")?;
	let (delta, at) = match last {
		None => (r.start(), now),
		Some(last) => {
			let last = from_db(last)?;
			match accrue(balance, last, now, r.daily, r.cap) {
				(_, at) if at == last => return Ok(balance),
				renewal => renewal,
			}
		}
	};
	// a day with nothing added is still written: renewal counts from it, not from the last top-up
	let entry = Entry {
		at,
		delta,
		kind: Kind::Accrual,
		charged: None,
		by: None,
		note: None,
	};
	append(conn, user, &entry).await?;
	Ok(balance + delta)
}

pub async fn owe(conn: &mut SqliteConnection, user: Uuid, at: Timestamp, amount: i64, why: &Owed) -> eyre::Result<()> {
	let (c, by, note) = match why {
		Owed::Charge(c) => (Some(c), None, None),
		Owed::Grant { by, note } => (None, Some(by), Some(note)),
	};
	sqlx::query("INSERT INTO token_kickbacks (user_id, service, key, label, amount, at, by_email, note) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
		.bind(user)
		.bind(c.map(|c| &c.service))
		.bind(c.map(|c| &c.key))
		.bind(c.and_then(|c| c.label.as_ref()))
		.bind(amount)
		.bind(to_db(at))
		.bind(by)
		.bind(note)
		.execute(&mut *conn)
		.await
		.wrap_err("owing a kick-back")?;
	Ok(())
}

/// The user's kick-backs not claimed yet, newest first.
pub async fn pending(conn: &mut SqliteConnection, user: Uuid) -> eyre::Result<Vec<Kickback>> {
	type Row = (i64, i64, i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>);
	let rows: Vec<Row> = sqlx::query_as("SELECT id, at, amount, service, key, label, by_email, note FROM token_kickbacks WHERE user_id = $1 AND claimed_at IS NULL ORDER BY id DESC")
		.bind(user)
		.fetch_all(&mut *conn)
		.await
		.wrap_err("listing pending kick-backs")?;
	rows.into_iter()
		.map(|(id, at, amount, service, key, label, by, note)| {
			let why = match (charged(service, key, label), by, note) {
				(Some(c), None, None) => Owed::Charge(c),
				(None, Some(by), Some(note)) => Owed::Grant { by, note },
				other => unreachable!("the table's CHECK: a charge's, or a grant with its admin and note; got {other:?}"),
			};
			Ok(Kickback { id, at: from_db(at)?, amount, why })
		})
		.collect()
}

/// The user's pending kick-backs into the ledger; what they added.
pub async fn claim(conn: &mut SqliteConnection, user: Uuid, now: Timestamp) -> eyre::Result<i64> {
	let added: Vec<i64> = sqlx::query_scalar(
		"INSERT INTO token_ledger (user_id, at, delta, kind, service, key, label, by_email, note) \
		 SELECT user_id, $2, amount, 'kickback', service, key, label, by_email, note FROM token_kickbacks \
		 WHERE user_id = $1 AND claimed_at IS NULL ORDER BY id RETURNING delta",
	)
	.bind(user)
	.bind(to_db(now))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("claiming kick-backs")?;
	sqlx::query("UPDATE token_kickbacks SET claimed_at = $2 WHERE user_id = $1 AND claimed_at IS NULL")
		.bind(user)
		.bind(to_db(now))
		.execute(&mut *conn)
		.await
		.wrap_err("marking kick-backs claimed")?;
	Ok(added.into_iter().sum())
}

/// Up to `limit` of the user's rows before `before`, newest first; a day renewal added nothing
/// to is left out.
pub async fn ledger(conn: &mut SqliteConnection, user: Uuid, before: Option<i64>, limit: i64) -> eyre::Result<Vec<(i64, Entry)>> {
	type Row = (i64, i64, i64, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>);
	let rows: Vec<Row> = sqlx::query_as(
		"SELECT id, at, delta, kind, service, key, label, by_email, note FROM token_ledger \
		 WHERE user_id = $1 AND delta != 0 AND ($2 IS NULL OR id < $2) ORDER BY id DESC LIMIT $3",
	)
	.bind(user)
	.bind(before)
	.bind(limit)
	.fetch_all(&mut *conn)
	.await
	.wrap_err("reading a ledger")?;
	rows.into_iter()
		.map(|(id, at, delta, kind, service, key, label, by, note)| {
			let entry = Entry {
				at: from_db(at)?,
				delta,
				kind: kind.parse()?,
				charged: charged(service, key, label),
				by,
				note,
			};
			Ok((id, entry))
		})
		.collect()
}

/// The user's charges since `since` by UTC day (days since the epoch): how many, and their tokens.
pub async fn charges_by_day(conn: &mut SqliteConnection, user: Uuid, since: Timestamp) -> eyre::Result<Vec<(i64, i64, i64)>> {
	sqlx::query_as(
		"SELECT at / 86400000000 AS day, COUNT(*), -SUM(delta) FROM token_ledger \
		 WHERE user_id = $1 AND kind = 'charge' AND at >= $2 GROUP BY day ORDER BY day",
	)
	.bind(user)
	.bind(to_db(since))
	.fetch_all(&mut *conn)
	.await
	.wrap_err("summing charges by day")
}

/// The last of review_archive's charges in the ledger; `None` before the import.
pub async fn cursor(conn: &mut SqliteConnection) -> eyre::Result<Option<i64>> {
	sqlx::query_scalar("SELECT after FROM charge_cursors WHERE service = $1")
		.bind(REVIEW_ARCHIVE)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err("reading the charges' cursor")
}

/// Opens tokens: review_archive's charges are followed from its first.
pub async fn open(conn: &mut SqliteConnection) -> eyre::Result<()> {
	sqlx::query("INSERT INTO charge_cursors (service, after) VALUES ($1, 0)")
		.bind(REVIEW_ARCHIVE)
		.execute(&mut *conn)
		.await
		.wrap_err("opening the charges' cursor")?;
	Ok(())
}

/// `from` → `to`; `false` when the cursor is not at `from`.
pub async fn move_cursor(conn: &mut SqliteConnection, from: i64, to: i64) -> eyre::Result<bool> {
	let done = sqlx::query("UPDATE charge_cursors SET after = $3 WHERE service = $1 AND after = $2")
		.bind(REVIEW_ARCHIVE)
		.bind(from)
		.bind(to)
		.execute(&mut *conn)
		.await
		.wrap_err("moving the charges' cursor")?;
	Ok(done.rows_affected() == 1)
}

/// No gmail, GMB, ledger row or kick-back yet.
pub async fn untouched(conn: &mut SqliteConnection) -> eyre::Result<bool> {
	sqlx::query_scalar(
		"SELECT NOT (EXISTS (SELECT 1 FROM gmails) OR EXISTS (SELECT 1 FROM gmbs) OR EXISTS (SELECT 1 FROM token_ledger) \
		 OR EXISTS (SELECT 1 FROM token_kickbacks))",
	)
	.fetch_one(&mut *conn)
	.await
	.wrap_err("looking for tokens and GMBs")
}

fn charged(service: Option<String>, key: Option<String>, label: Option<String>) -> Option<Charged> {
	match (service, key) {
		(Some(service), Some(key)) => Some(Charged { service, key, label }),
		(None, None) => None,
		other => unreachable!("the table's CHECK: service and key together; got {other:?}"),
	}
}

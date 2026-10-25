//! SQLite: runtime `sqlx` queries, migrations embedded from `migrations/` and applied on open.
//! Every query here is executed by the integration tests (`tests/`), which stand in for the
//! compile-time checks the `query!` macros would give.
//!
//! One file, one writer at a time (WAL: readers never wait for it). Every transaction that
//! writes begins `BEGIN IMMEDIATE` ([`begin_write`]): it takes the write lock up front, so two
//! of them queue on `busy_timeout` instead of both reading under a plain `BEGIN` and the
//! second failing with SQLITE_BUSY when it tries to upgrade to a write. That queue is also
//! what orders ingest against a rebuild, and two events of one lead against each other: what
//! Postgres needed advisory locks for, the single writer gives for nothing. A lone statement
//! outside a transaction is atomic by itself and waits on `busy_timeout` the same way.

pub mod access;
pub mod bookings;
pub mod events;
pub mod experiments;
pub mod invoices;
pub mod places;
pub mod posthog;
pub mod pricing;
pub mod projections;
pub mod reads;
pub mod sessions;
pub mod sources;
pub mod telegram;

use std::{path::Path, time::Duration};

use eyre::WrapErr;
use jiff::{Timestamp, civil::Date};
use sqlx::{
	Sqlite, SqliteConnection, SqlitePool, Transaction,
	migrate::Migrator,
	sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

/// How long a statement waits for another connection's write lock before it fails. Writes are
/// short; the longest is a rebuild of the projections, which ingest waits out up to this.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// The schema, embedded from `migrations/`.
fn migrator() -> Migrator {
	let mut m = sqlx::migrate!("./migrations");
	// A migration the database has and this build does not know is a rollback onto a schema
	// moved on: fine, as it was on Postgres.
	m.set_ignore_missing(true);
	m
}

/// The connection settings every connection to the panel's file gets.
pub fn options(path: &Path) -> SqliteConnectOptions {
	SqliteConnectOptions::new()
		.filename(path)
		.create_if_missing(true)
		// Readers do not block the writer nor it them; and litestream replicates the WAL.
		.journal_mode(SqliteJournalMode::Wal)
		// Durable at every checkpoint, not every commit: in WAL a crash loses at most the
		// last commits, never consistency — and litestream ships the WAL off the pod anyway.
		.synchronous(SqliteSynchronous::Normal)
		.foreign_keys(true)
		.busy_timeout(BUSY_TIMEOUT)
}

/// The panel's database.
#[derive(Clone, Debug)]
pub struct Store {
	pool: SqlitePool,
}

impl Store {
	/// Opens (creating it if need be) the database at `path` and applies the migrations it
	/// lacks. There is no separate migrate step: the process that owns the file migrates it.
	pub async fn open(path: &Path) -> eyre::Result<Self> {
		Self::open_with(options(path)).await.wrap_err_with(|| format!("opening the database at {}", path.display()))
	}

	pub async fn open_with(options: SqliteConnectOptions) -> eyre::Result<Self> {
		// A request waits at most this long for a connection, then fails: a pool drained by a
		// burst answers 500 quickly instead of stacking every request behind it.
		let pool = SqlitePoolOptions::new()
			.max_connections(8)
			.acquire_timeout(Duration::from_secs(3))
			.connect_with(options)
			.await
			.wrap_err("connecting to SQLite")?;
		migrator().run(&pool).await.wrap_err("applying migrations")?;
		Ok(Self { pool })
	}

	pub fn pool(&self) -> &SqlitePool {
		&self.pool
	}

	/// A transaction that will write: the write lock is taken at its start (see the module).
	pub async fn begin_write(&self) -> eyre::Result<Transaction<'static, Sqlite>> {
		self.pool.begin_with("BEGIN IMMEDIATE").await.wrap_err("beginning a write")
	}
}

/// [`Store::begin_write`] on a connection already in hand.
pub async fn begin_write(conn: &mut SqliteConnection) -> eyre::Result<Transaction<'_, Sqlite>> {
	sqlx::Connection::begin_with(conn, "BEGIN IMMEDIATE").await.wrap_err("beginning a write")
}

/// A timestamp as the database holds it: microseconds since the epoch, the precision Postgres'
/// timestamptz had. Anything finer is dropped.
pub(crate) fn to_db(ts: Timestamp) -> i64 {
	ts.as_microsecond()
}

pub(crate) fn from_db(us: i64) -> eyre::Result<Timestamp> {
	Timestamp::from_microsecond(us).wrap_err_with(|| format!("stored timestamp {us}"))
}

/// A civil date as the database holds it: `YYYY-MM-DD`, which sorts as the date does.
pub(crate) fn day_to_db(d: Date) -> String {
	d.strftime("%Y-%m-%d").to_string()
}

pub(crate) fn day_from_db(s: &str) -> eyre::Result<Date> {
	s.parse().wrap_err_with(|| format!("stored date {s:?}"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn timestamps_round_trip() {
		for s in ["2026-09-30T10:00:00.123456Z", "1969-12-31T23:59:59.5Z", "1970-01-01T00:00:00Z"] {
			let ts: Timestamp = s.parse().unwrap();
			assert_eq!(from_db(to_db(ts)).unwrap(), ts, "{s}");
		}
		let fine: Timestamp = "2026-09-30T10:00:00.123456789Z".parse().unwrap();
		assert_eq!(from_db(to_db(fine)).unwrap().to_string(), "2026-09-30T10:00:00.123456Z", "nanoseconds are dropped");
	}

	#[test]
	fn days_round_trip_and_sort() {
		for s in ["2026-09-30", "2024-02-29", "0001-01-01"] {
			let d: Date = s.parse().unwrap();
			assert_eq!(day_to_db(d), s);
			assert_eq!(day_from_db(&day_to_db(d)).unwrap(), d, "{s}");
		}
	}
}

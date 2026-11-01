//! People's tokens: a ledger whose rows are only ever added, renewed a day's worth at a time
//! whenever a balance is read ([`panel_core::tokens`]).
//!
//! ```text
//! review_archive GET /api/conductor/charges?after=<cursor> ─ a page ─ one write: payers renewed,
//!   each charge split over them within their balances, kick-backs owed, the cursor moved
//! every change, every page, every 60 s ─ snapshot (GMBs, balances, the cursor) ─ PUT /api/conductor/gmbs
//! ```
//!
//! **A charge is in the ledger before the cursor passes it**, and the page is taken only from
//! where the cursor stands, so a page pulled twice — a crash before its commit, a replica behind —
//! charges once. Until the import from review_archive ([`crate::import`]) opens the cursor, tokens
//! and GMBs are closed: every balance read writes a renewal, which would make the import refuse.

use std::collections::{BTreeMap, BTreeSet};

use eyre::WrapErr;
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use panel_core::tokens::{Renewal, split};
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use uuid::Uuid;

pub use crate::store::{
	gmbs::Managed,
	tokens::{Charged, Entry, Kickback, Kind, Owed},
	users::User,
};
use crate::{
	Panel, Refused,
	store::{
		gmbs as gmbs_db,
		tokens::{self as db, REVIEW_ARCHIVE},
		users,
	},
};

/// How many days `usage` reads, today's included.
pub const USAGE_DAYS: i64 = 30;

/// One of review_archive's charges (`GET /api/conductor/charges`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Charge {
	pub id: i64,
	/// review_archive's, e.g. `run:42`.
	pub key: String,
	pub payers: Vec<Uuid>,
	pub spent: i64,
	/// Each payer is owed back what they paid.
	pub kick_back: bool,
	pub label: String,
	pub at: Timestamp,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChargePage {
	pub charges: Vec<Charge>,
	pub next: i64,
}

/// What review_archive is told (`PUT /api/conductor/gmbs`).
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
	/// Every GMB there ever was.
	pub gmbs: Vec<Managed>,
	/// Every user's, renewed up to the snapshot.
	pub balances: BTreeMap<Uuid, i64>,
	/// The last of its charges in these balances.
	pub charges_through: i64,
}

/// An admin's change to a balance.
#[derive(Clone, Debug)]
pub enum Change {
	/// To `n`, 0 or more.
	Set { n: i64, note: Option<String> },
	/// `n`, 1 or more: a sale is one, its reference in the note.
	Add { n: i64, note: Option<String> },
	/// Owed until claimed, 1 or more; the note says what for.
	Grant { n: i64, note: String },
}

/// Charges and tokens of one UTC day.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageDay {
	pub day: Date,
	pub charges: i64,
	pub tokens: i64,
}

/// Whether the import has opened tokens and GMBs.
pub(crate) async fn is_open(conn: &mut SqliteConnection) -> eyre::Result<bool> {
	Ok(db::cursor(conn).await?.is_some())
}

impl Panel {
	pub fn renewal(&self) -> &Renewal {
		&self.renewal
	}

	/// Resolves when something review_archive's snapshot carries has changed since it was last
	/// awaited.
	pub async fn snapshot_due(&self) {
		self.snapshot_due.notified().await;
	}

	/// What concierge just told the `/api/v1` gate of a user.
	pub async fn user_seen(&self, user: &User, now: Timestamp) -> eyre::Result<()> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection to record a user")?;
		users::seen(&mut conn, user, now, now).await
	}

	/// The one user last seen with `email`.
	pub async fn user_by_email(&self, email: &str) -> eyre::Result<Uuid> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection to find a user")?;
		match users::by_email(&mut conn, email).await?.as_slice() {
			[one] => Ok(*one),
			[] => eyre::bail!("no user {email}: they sign in to the conductor first"),
			many => eyre::bail!("{} users are {email}: {many:?}", many.len()),
		}
	}

	/// The user's balance, renewed, and their kick-backs not claimed yet.
	pub async fn tokens(&self, user: Uuid, now: Timestamp) -> eyre::Result<Result<(i64, Vec<Kickback>), Refused>> {
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		let balance = db::renewed(&mut tx, user, now, &self.renewal).await?;
		let pending = db::pending(&mut tx, user).await?;
		tx.commit().await.wrap_err("committing a renewal")?;
		Ok(Ok((balance, pending)))
	}

	/// The user's kick-backs into their balance: as [`Self::tokens`] then.
	pub async fn claim(&self, user: Uuid, now: Timestamp) -> eyre::Result<Result<(i64, Vec<Kickback>), Refused>> {
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		let balance = db::renewed(&mut tx, user, now, &self.renewal).await?;
		let added = db::claim(&mut tx, user, now).await?;
		tx.commit().await.wrap_err("committing a claim")?;
		if added > 0 {
			self.snapshot_due.notify_one();
		}
		Ok(Ok((balance + added, Vec::new())))
	}

	/// Up to `limit` of the user's ledger before the entry `before`, newest first.
	pub async fn ledger(&self, user: Uuid, before: Option<i64>, limit: i64) -> eyre::Result<Result<Vec<(i64, Entry)>, Refused>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for a ledger")?;
		if !is_open(&mut conn).await? {
			return Ok(Err(Refused::Closed));
		}
		Ok(Ok(db::ledger(&mut conn, user, before, limit).await?))
	}

	/// The user's charges by UTC day, over the [`USAGE_DAYS`] up to `now`, oldest first.
	pub async fn usage(&self, user: Uuid, now: Timestamp) -> eyre::Result<Result<Vec<UsageDay>, Refused>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for usage")?;
		if !is_open(&mut conn).await? {
			return Ok(Err(Refused::Closed));
		}
		let today = now.to_zoned(TimeZone::UTC).date();
		let first = today.checked_sub(jiff::Span::new().days(USAGE_DAYS - 1)).wrap_err("30 days before today")?;
		let since = first.to_zoned(TimeZone::UTC).wrap_err("a day's start")?.timestamp();
		let epoch_day = |d: Date| d.to_zoned(TimeZone::UTC).map(|z| z.timestamp().as_second() / 86_400);
		let charged = db::charges_by_day(&mut conn, user, since).await?;
		first
			.series(jiff::Span::new().days(1))
			.take(USAGE_DAYS as usize)
			.map(|day| {
				let n = epoch_day(day).wrap_err("a day's start")?;
				// a day without a charge row charged nothing
				let (charges, tokens) = charged.iter().find(|(d, ..)| *d == n).map_or((0, 0), |&(_, c, t)| (c, t));
				Ok(UsageDay { day, charges, tokens })
			})
			.collect::<eyre::Result<_>>()
			.map(Ok)
	}

	/// Everyone's balance, renewed.
	pub async fn holders(&self, now: Timestamp) -> eyre::Result<Result<Vec<(User, i64)>, Refused>> {
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		let mut out = Vec::new();
		for u in users::all(&mut tx).await? {
			let balance = db::renewed(&mut tx, u.user_id, now, &self.renewal).await?;
			out.push((u, balance));
		}
		tx.commit().await.wrap_err("committing renewals")?;
		Ok(Ok(out))
	}

	/// An admin (`by`, their email) changes the user's balance, or owes them a grant: the
	/// balance then.
	pub async fn change_balance(&self, user: Uuid, change: Change, by: &str, now: Timestamp) -> eyre::Result<Result<i64, Refused>> {
		let mut tx = self.store.begin_write().await?;
		if !is_open(&mut tx).await? {
			return Ok(Err(Refused::Closed));
		}
		if !users::exists(&mut tx, user).await? {
			return Ok(Err(Refused::NotFound));
		}
		let balance = db::renewed(&mut tx, user, now, &self.renewal).await?;
		let note = |n: Option<String>| n.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
		let (kind, delta, note) = match change {
			Change::Set { n, note: why } if n >= 0 => (Kind::Set, n - balance, note(why)),
			Change::Add { n, note: why } if n >= 1 => (Kind::Add, n, note(why)),
			Change::Grant { n, note: why } if n >= 1 => {
				let Some(why) = note(Some(why)) else {
					return Ok(Err(Refused::Invalid("a grant says what it is for, in its note".into())));
				};
				let grant = Owed::Grant { by: by.to_owned(), note: why };
				db::owe(&mut tx, user, now, n, &grant).await?;
				tx.commit().await.wrap_err("committing a grant")?;
				tracing::info!(user_id = %user, by, n, "tokens granted");
				return Ok(Ok(balance));
			}
			_ => return Ok(Err(Refused::Invalid("a balance is set to 0 or more; adds and grants are 1 or more".into()))),
		};
		let entry = Entry {
			at: now,
			delta,
			kind,
			charged: None,
			by: Some(by.to_owned()),
			note,
		};
		db::append(&mut tx, user, &entry).await?;
		tx.commit().await.wrap_err("committing a balance change")?;
		tracing::info!(user_id = %user, by, kind = kind.as_str(), delta, "balance changed");
		self.snapshot_due.notify_one();
		Ok(Ok(balance + delta))
	}

	/// Where review_archive's next page of charges starts; `None` before the import.
	pub async fn charges_after(&self) -> eyre::Result<Option<i64>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for the charges' cursor")?;
		db::cursor(&mut conn).await
	}

	/// A page of review_archive's charges, asked for after `after`, into the ledger, and the
	/// cursor moved to its `next`, in one write transaction. A page that does not start at the
	/// cursor, or names a payer the conductor does not know, fails whole: nothing is written and
	/// the cursor stays. How many charges were taken.
	pub async fn take_charges(&self, page: ChargePage, after: i64, now: Timestamp) -> eyre::Result<usize> {
		let last = page.charges.last().map_or(after, |c| c.id);
		eyre::ensure!(page.next == last, "review_archive answered next {} for a page ending at {last}", page.next);
		let mut prev = after;
		for c in &page.charges {
			eyre::ensure!(c.id > prev, "charge {} after {prev}: ids ascend past the cursor", c.id);
			eyre::ensure!(c.spent > 0, "charge {} spends {}", c.id, c.spent);
			let distinct: BTreeSet<_> = c.payers.iter().collect();
			eyre::ensure!(!c.payers.is_empty() && distinct.len() == c.payers.len(), "charge {}: payers {:?}", c.id, c.payers);
			prev = c.id;
		}
		if page.charges.is_empty() {
			return Ok(0);
		}
		let mut tx = self.store.begin_write().await?;
		match db::cursor(&mut tx).await? {
			Some(at) if at == after => {}
			at => eyre::bail!("a page of charges after {after}, the cursor at {at:?}"),
		}
		for c in &page.charges {
			let mut balances = Vec::with_capacity(c.payers.len());
			for p in &c.payers {
				eyre::ensure!(users::exists(&mut tx, *p).await?, "charge {}: payer {p} is no user here", c.id);
				balances.push((*p, db::renewed(&mut tx, *p, now, &self.renewal).await?));
			}
			let charged = Charged {
				service: REVIEW_ARCHIVE.to_owned(),
				key: c.key.clone(),
				label: Some(c.label.clone()),
			};
			let paid = split(c.spent, &balances);
			let short = c.spent - paid.iter().map(|(_, p)| p).sum::<i64>();
			if short > 0 {
				tracing::info!(charge = c.id, short, "tokens: a charge past its payers' balances, clamped");
			}
			for (p, n) in paid {
				let entry = Entry {
					at: c.at,
					delta: -n,
					kind: Kind::Charge,
					charged: Some(charged.clone()),
					by: None,
					note: None,
				};
				db::append(&mut tx, p, &entry).await?;
				if c.kick_back {
					db::owe(&mut tx, p, c.at, n, &Owed::Charge(charged.clone())).await?;
				}
			}
		}
		eyre::ensure!(db::move_cursor(&mut tx, after, page.next).await?, "the charges' cursor moved meanwhile");
		tx.commit().await.wrap_err("committing a page of charges")?;
		self.snapshot_due.notify_one();
		Ok(page.charges.len())
	}

	/// What review_archive is told, every balance renewed in the transaction that reads the
	/// cursor; `None` before the import.
	pub async fn snapshot(&self, now: Timestamp) -> eyre::Result<Option<Snapshot>> {
		let mut tx = self.store.begin_write().await?;
		let Some(charges_through) = db::cursor(&mut tx).await? else {
			return Ok(None);
		};
		let mut balances = BTreeMap::new();
		for u in users::all(&mut tx).await? {
			balances.insert(u.user_id, db::renewed(&mut tx, u.user_id, now, &self.renewal).await?);
		}
		let gmbs = gmbs_db::managed(&mut tx).await?;
		tx.commit().await.wrap_err("committing the snapshot's renewals")?;
		Ok(Some(Snapshot { gmbs, balances, charges_through }))
	}
}

//! The invoices the panel asked banking for, and the position of banking's `MoneyEvents`
//! stream the payments are confirmed from. Nothing here touches the journal or `payments`: an
//! invoice is banking's, and the panel only mirrors it.

use eyre::WrapErr;
use jiff::Timestamp;
use panel_core::invoice::InvoiceState;
use sqlx::SqliteConnection;
use uuid::Uuid;

use super::{from_db, to_db};

/// One stored invoice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invoice {
	/// The panel's id: derived from the operator and their idempotency key.
	pub id: Uuid,
	/// Banking's; `None` while [`InvoiceState::Creating`].
	pub invoice_id: Option<String>,
	/// Canonical decimal USDT.
	pub amount: String,
	pub memo: String,
	pub state: InvoiceState,
	/// The concierge user who issued it.
	pub created_by: Uuid,
	pub created_at: Timestamp,
	pub expires_at: Timestamp,
	/// Banking's page on evinvest.ltd where the payer pays; `None` while creating.
	pub pay_url: Option<String>,
	/// Set once, when the ledger took the payment.
	pub paid: Option<Paid>,
}

/// Who paid an invoice, and when the ledger booked it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paid {
	pub at: Timestamp,
	/// The payer's `auth_subject`: opaque, never parsed.
	pub by: String,
}

type Row = (Uuid, Option<String>, String, String, String, Uuid, i64, i64, Option<String>, Option<i64>, Option<String>);

macro_rules! columns {
	() => {
		"id, invoice_id, amount, memo, state, created_by, created_at, expires_at, pay_url, paid_at, paid_by"
	};
}

fn row((id, invoice_id, amount, memo, state, created_by, created_at, expires_at, pay_url, paid_at, paid_by): Row) -> eyre::Result<Invoice> {
	let paid = match (paid_at, paid_by) {
		(Some(at), Some(by)) => Some(Paid { at: from_db(at)?, by }),
		(None, None) => None,
		_ => eyre::bail!("invoice {id}: paid_at and paid_by are stored one without the other"),
	};
	Ok(Invoice {
		id,
		invoice_id,
		amount,
		memo,
		state: InvoiceState::parse(&state).wrap_err_with(|| format!("invoice {id}"))?,
		created_by,
		created_at: from_db(created_at)?,
		expires_at: from_db(expires_at)?,
		pay_url,
		paid,
	})
}

pub async fn get(conn: &mut SqliteConnection, id: Uuid) -> eyre::Result<Option<Invoice>> {
	let r: Option<Row> = sqlx::query_as(concat!("SELECT ", columns!(), " FROM invoices WHERE id = $1"))
		.bind(id)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err_with(|| format!("reading invoice {id}"))?;
	r.map(row).transpose()
}

pub async fn by_invoice_id(conn: &mut SqliteConnection, invoice_id: &str) -> eyre::Result<Option<Invoice>> {
	let r: Option<Row> = sqlx::query_as(concat!("SELECT ", columns!(), " FROM invoices WHERE invoice_id = $1"))
		.bind(invoice_id)
		.fetch_optional(&mut *conn)
		.await
		.wrap_err_with(|| format!("reading banking's invoice {invoice_id}"))?;
	r.map(row).transpose()
}

/// The newest `limit`, newest first.
pub async fn newest(conn: &mut SqliteConnection, limit: i64) -> eyre::Result<Vec<Invoice>> {
	let rows: Vec<Row> = sqlx::query_as(concat!("SELECT ", columns!(), " FROM invoices ORDER BY created_at DESC, id DESC LIMIT $1"))
		.bind(limit)
		.fetch_all(&mut *conn)
		.await
		.wrap_err("listing invoices")?;
	rows.into_iter().map(row).collect()
}

/// A new row, before banking is asked.
pub async fn insert(conn: &mut SqliteConnection, i: &Invoice) -> eyre::Result<()> {
	sqlx::query(
		"INSERT INTO invoices (id, invoice_id, amount, memo, state, created_by, created_at, expires_at, pay_url, paid_at, paid_by) \
		 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
	)
	.bind(i.id)
	.bind(&i.invoice_id)
	.bind(&i.amount)
	.bind(&i.memo)
	.bind(i.state.as_str())
	.bind(i.created_by)
	.bind(to_db(i.created_at))
	.bind(to_db(i.expires_at))
	.bind(&i.pay_url)
	.bind(i.paid.as_ref().map(|p| to_db(p.at)))
	.bind(i.paid.as_ref().map(|p| p.by.as_str()))
	.execute(&mut *conn)
	.await
	.wrap_err_with(|| format!("storing invoice {}", i.id))?;
	Ok(())
}

/// What banking said of the invoice: its id and pay page once issued, its state, and the
/// payment once paid. The payment, once stored, is kept as first stored.
pub async fn update(conn: &mut SqliteConnection, i: &Invoice) -> eyre::Result<()> {
	sqlx::query(
		"UPDATE invoices SET invoice_id = $2, state = $3, pay_url = $4, \
		 paid_at = COALESCE(paid_at, $5), paid_by = COALESCE(paid_by, $6) WHERE id = $1",
	)
	.bind(i.id)
	.bind(&i.invoice_id)
	.bind(i.state.as_str())
	.bind(&i.pay_url)
	.bind(i.paid.as_ref().map(|p| to_db(p.at)))
	.bind(i.paid.as_ref().map(|p| p.by.as_str()))
	.execute(&mut *conn)
	.await
	.wrap_err_with(|| format!("updating invoice {}", i.id))?;
	Ok(())
}

/// Drops a row banking refused to issue: nothing stands behind it.
pub async fn delete_creating(conn: &mut SqliteConnection, id: Uuid) -> eyre::Result<()> {
	sqlx::query("DELETE FROM invoices WHERE id = $1 AND state = 'creating'")
		.bind(id)
		.execute(&mut *conn)
		.await
		.wrap_err_with(|| format!("dropping refused invoice {id}"))?;
	Ok(())
}

/// Marks banking's invoice paid on the money event `event_id`: `true` if this event was taken
/// now, `false` for an invoice the panel does not know or an event it took already. A payment
/// already stored (by a read of the invoice) is kept; the event is still recorded on it.
pub async fn mark_paid(conn: &mut SqliteConnection, invoice_id: &str, paid: &Paid, event_id: &str) -> eyre::Result<bool> {
	let done = sqlx::query(
		"UPDATE invoices SET state = 'paid', paid_at = COALESCE(paid_at, $2), paid_by = COALESCE(paid_by, $3), paid_event_id = $4 \
		 WHERE invoice_id = $1 AND paid_event_id IS NULL",
	)
	.bind(invoice_id)
	.bind(to_db(paid.at))
	.bind(&paid.by)
	.bind(event_id)
	.execute(&mut *conn)
	.await
	.wrap_err_with(|| format!("marking banking's invoice {invoice_id} paid"))?;
	Ok(done.rows_affected() == 1)
}

/// Where the next `MoneyEvents.Pull` starts (exclusive).
pub async fn cursor(conn: &mut SqliteConnection) -> eyre::Result<i64> {
	sqlx::query_scalar("SELECT position FROM invoice_cursor WHERE id = 1")
		.fetch_one(&mut *conn)
		.await
		.wrap_err("reading the invoice events' cursor")
}

pub async fn set_cursor(conn: &mut SqliteConnection, position: i64) -> eyre::Result<()> {
	sqlx::query("UPDATE invoice_cursor SET position = $1 WHERE id = 1")
		.bind(position)
		.execute(&mut *conn)
		.await
		.wrap_err("moving the invoice events' cursor")?;
	Ok(())
}

//! Invoices: an operator asks a customer to pay through banking (`scope.v1.InvoiceService` in
//! the scope `sa`), and the panel learns of the payment from banking's `MoneyEvents`.
//!
//! Banking is the authority; [`Ledger`] is the port it is reached through (the server's gRPC
//! client, a fake in the tests). The panel keeps one row per invoice it asked for:
//!
//! - **Issue.** The row is stored first (`creating`), keyed by the operator's idempotency key,
//!   with the terms sent; then banking is asked, under a key derived from the row. A retry
//!   whose first attempt never heard back sends the stored terms again, so banking answers it
//!   as a repeat, not a second invoice. A refusal drops the row: banking issued nothing.
//! - **Read.** One invoice is asked of banking and its state stored; the list is the rows.
//! - **Confirm.** [`Panel::confirm_invoices`] pulls the scope's money events after the stored
//!   cursor and, in one transaction per page, marks each paid invoice the panel knows and moves
//!   the cursor. An event already taken is skipped (its id is stored on the invoice).
//!
//! Paid means posted: `processing` is never paid, and a paid invoice is never anything else.
//! Nothing here writes the journal or `payments`: the lead payments are typed in by hand and
//! stay apart.

use eyre::WrapErr;
use jiff::{SignedDuration, Timestamp};
use panel_core::{
	Invalid,
	invoice::{self as rules, InvoiceState},
};
use uuid::Uuid;

pub use crate::store::invoices::{Invoice, Paid};
use crate::{
	Panel,
	operator::{Actor, Done, idempotent_id},
	store::invoices as db,
};

/// What an operator asks for.
#[derive(Clone, Debug)]
pub struct NewInvoice {
	/// Decimal USDT as typed; made canonical here.
	pub amount: String,
	pub memo: String,
	pub expires_in_days: u32,
}

/// `CreateInvoice`, as the panel sends it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueRequest {
	pub amount: String,
	pub memo: String,
	pub expires_at: Timestamp,
	/// `sa_invoice:<row id>`.
	pub subject_ref: String,
	pub key: String,
}

/// An invoice as banking reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Issued {
	pub invoice_id: String,
	/// Never [`InvoiceState::Creating`].
	pub state: InvoiceState,
	pub pay_url: String,
	/// Present exactly when `state` is paid.
	pub paid: Option<Paid>,
}

/// One page of `MoneyEvents.Pull`.
#[derive(Clone, Debug)]
pub struct EventPage {
	pub events: Vec<MoneyEvent>,
	/// The next `after_position`.
	pub next_position: i64,
}

#[derive(Clone, Debug)]
pub struct MoneyEvent {
	/// The unit of banking's at-least-once delivery.
	pub event_id: String,
	pub kind: EventKind,
}

/// What the panel reads of a money event. Every kind but the invoice ones is [`Self::Other`].
#[derive(Clone, Debug)]
pub enum EventKind {
	InvoicePaid { invoice_id: String, paid: Paid },
	InvoiceCancelled { invoice_id: String },
	InvoiceExpired { invoice_id: String },
	Other,
}

/// Why banking did not do what was asked.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LedgerError {
	/// FAILED_PRECONDITION or PERMISSION_DENIED: banking's machine reason, e.g. `not_granted`,
	/// `invoice_not_open`, `scope_frozen`.
	#[error("refused: {0}")]
	Refused(String),
	/// INVALID_ARGUMENT: banking's words on what was wrong with the request.
	#[error("invalid: {0}")]
	Invalid(String),
	/// ALREADY_EXISTS, e.g. `idempotency_conflict`.
	#[error("conflict: {0}")]
	Conflict(String),
	#[error("not found")]
	NotFound,
	/// Banking could not be reached, or did not answer in time; the same request may be sent again.
	#[error("unavailable: {0}")]
	Unavailable(String),
	/// An answer the contract does not allow.
	#[error("protocol: {0}")]
	Protocol(String),
}

/// banking's ScopeLedger, as far as the panel's invoices need it.
pub trait Ledger: Sync {
	/// Opens the scope account invoices pay into ([`rules::ACCOUNT_LABEL`]); a repeat is a no-op.
	/// Called before every invoice is issued: an adapter may answer from memory once it has
	/// succeeded.
	fn open_account(&self) -> impl Future<Output = Result<(), LedgerError>> + Send;
	fn create_invoice(&self, request: &IssueRequest) -> impl Future<Output = Result<Issued, LedgerError>> + Send;
	fn get_invoice(&self, invoice_id: &str) -> impl Future<Output = Result<Issued, LedgerError>> + Send;
	/// The scope's money events after `after_position`, oldest first, at most `limit`.
	fn pull(&self, after_position: i64, limit: u32) -> impl Future<Output = Result<EventPage, LedgerError>> + Send;
}

/// Why an invoice request failed.
#[derive(Debug, thiserror::Error)]
pub enum InvoiceError {
	#[error(transparent)]
	Invalid(#[from] Invalid),
	/// The idempotency key was used for another invoice.
	#[error("{0}")]
	Conflict(&'static str),
	#[error("no such invoice")]
	NotFound,
	#[error("the ledger: {0}")]
	Ledger(#[from] LedgerError),
	#[error(transparent)]
	Internal(#[from] eyre::Report),
}

/// The most invoices the list returns.
pub const LIST_LIMIT: i64 = 500;
/// Money events asked per pull, and the pulls one pass may make before it waits for the next.
pub const PULL_LIMIT: u32 = 200;
const PULLS_PER_PASS: u32 = 20;

/// What one pass of [`Panel::confirm_invoices`] did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Confirmed {
	pub events: u64,
	pub paid: u64,
	pub position: i64,
}

impl Panel {
	/// Issues the invoice `by` asks for, at most once per their idempotency `key`: the first
	/// call asks banking (`replayed: false`), a retry is answered with what the first stored.
	pub async fn issue_invoice_once(&self, ledger: &impl Ledger, by: Actor, new: NewInvoice, key: &str, now: Timestamp) -> Result<Done<Invoice>, InvoiceError> {
		let amount = rules::canonical_amount(&new.amount)?;
		rules::check_memo(&new.memo)?;
		rules::check_expires_in_days(new.expires_in_days)?;
		let lifetime = SignedDuration::from_hours(24 * i64::from(new.expires_in_days));
		let id = idempotent_id(by, "invoice", key);

		let mut tx = self.store.begin_write().await?;
		let (row, replayed) = match db::get(&mut tx, id).await? {
			Some(row) => {
				let same = row.amount == amount && row.memo == new.memo && row.expires_at.duration_since(row.created_at) == lifetime;
				if !same {
					return Err(InvoiceError::Conflict("this Idempotency-Key was used for another invoice"));
				}
				(row, true)
			}
			None => {
				// As it reads back, so the terms banking gets are the stored ones to the microsecond.
				let now = crate::store::from_db(crate::store::to_db(now))?;
				let row = Invoice {
					id,
					invoice_id: None,
					amount,
					memo: new.memo,
					state: InvoiceState::Creating,
					created_by: by.0,
					created_at: now,
					expires_at: now.checked_add(lifetime).wrap_err("an invoice's expiry")?,
					pay_url: None,
					paid: None,
				};
				db::insert(&mut tx, &row).await?;
				(row, false)
			}
		};
		tx.commit().await.wrap_err("committing an invoice")?;
		if row.state != InvoiceState::Creating {
			return Ok(Done { value: row, replayed });
		}
		let row = self.ask(ledger, row).await?;
		tracing::info!(invoice = %row.id, invoice_id = row.invoice_id.as_deref(), amount = row.amount, "invoice issued");
		Ok(Done { value: row, replayed })
	}

	/// Asks banking for the invoice a `creating` row stands for, under the row's own key: a
	/// repeat of an earlier ask is answered with what that one created.
	async fn ask(&self, ledger: &impl Ledger, row: Invoice) -> Result<Invoice, InvoiceError> {
		let id = row.id;
		let request = IssueRequest {
			amount: row.amount.clone(),
			memo: row.memo.clone(),
			expires_at: row.expires_at,
			subject_ref: rules::subject_ref(id),
			key: rules::ledger_key(id),
		};
		let issued = match ledger.open_account().await {
			Ok(()) => ledger.create_invoice(&request).await,
			Err(e) => Err(e),
		};
		let issued = match issued {
			Ok(issued) => issued,
			Err(e) => {
				// A refusal is final and moved nothing: the row goes, so the key can be used
				// again. Anything else may have reached banking: the row stays for the retry.
				if matches!(e, LedgerError::Refused(_) | LedgerError::Invalid(_)) {
					let mut conn = self.store.pool().acquire().await.wrap_err("a connection for invoices")?;
					db::delete_creating(&mut conn, id).await?;
				}
				return Err(e.into());
			}
		};
		Ok(self.store_issued(row, issued).await?)
	}

	/// The newest invoices, newest first, at most [`LIST_LIMIT`].
	pub async fn invoices(&self) -> eyre::Result<Vec<Invoice>> {
		let mut conn = self.store.pool().acquire().await.wrap_err("a connection for invoices")?;
		db::newest(&mut conn, LIST_LIMIT).await
	}

	/// The invoice as banking has it now, stored. One in a final state is answered as stored:
	/// banking has nothing newer to say of it. One still `creating` is asked for again, as the
	/// operator's retry would: its first ask was lost, or the panel stopped before storing the
	/// answer.
	pub async fn refresh_invoice(&self, ledger: &impl Ledger, id: Uuid) -> Result<Invoice, InvoiceError> {
		let row = {
			let mut conn = self.store.pool().acquire().await.wrap_err("a connection for invoices")?;
			db::get(&mut conn, id).await?.ok_or(InvoiceError::NotFound)?
		};
		let Some(invoice_id) = row.invoice_id.clone() else { return self.ask(ledger, row).await };
		if row.state.is_final() {
			return Ok(row);
		}
		let issued = ledger.get_invoice(&invoice_id).await?;
		Ok(self.store_issued(row, issued).await?)
	}

	/// Takes what banking said of `row`'s invoice into the store.
	async fn store_issued(&self, row: Invoice, issued: Issued) -> eyre::Result<Invoice> {
		eyre::ensure!(issued.state != InvoiceState::Creating, "the ledger cannot report an invoice as being created");
		eyre::ensure!(
			(issued.state == InvoiceState::Paid) == issued.paid.is_some(),
			"the ledger reported invoice {} {} with{} a payer",
			issued.invoice_id,
			issued.state.as_str(),
			if issued.paid.is_some() { "" } else { "out" }
		);
		if let Some(known) = &row.invoice_id {
			eyre::ensure!(
				*known == issued.invoice_id,
				"invoice {}: the ledger answered for {} instead of {known}",
				row.id,
				issued.invoice_id
			);
		}
		let mut tx = self.store.begin_write().await?;
		// Read again inside the write: the confirmation may have marked it paid meanwhile.
		let current = db::get(&mut tx, row.id).await?.ok_or_else(|| eyre::eyre!("invoice {} went away", row.id))?;
		let state = current.state.after(issued.state);
		let updated = Invoice {
			invoice_id: Some(issued.invoice_id),
			state,
			pay_url: Some(issued.pay_url),
			paid: current.paid.clone().or(if state == InvoiceState::Paid { issued.paid } else { None }),
			..current
		};
		db::update(&mut tx, &updated).await?;
		tx.commit().await.wrap_err("committing an invoice's state")?;
		Ok(updated)
	}

	/// One pass over banking's money events in the scope: each invoice paid is marked once, and
	/// the cursor moves with it, page by page.
	pub async fn confirm_invoices(&self, ledger: &impl Ledger) -> eyre::Result<Confirmed> {
		let mut done = Confirmed::default();
		for _ in 0..PULLS_PER_PASS {
			// No connection is held across the pull: it may take the call's whole timeout.
			let after = {
				let mut conn = self.store.pool().acquire().await.wrap_err("a connection for invoices")?;
				db::cursor(&mut conn).await?
			};
			let page = ledger.pull(after, PULL_LIMIT).await.wrap_err_with(|| format!("pulling money events after {after}"))?;
			let full = page.events.len() >= PULL_LIMIT as usize;
			let mut tx = self.store.begin_write().await?;
			eyre::ensure!(db::cursor(&mut tx).await? == after, "the invoice events' cursor moved under the pull");
			for event in &page.events {
				done.events += 1;
				match &event.kind {
					// An invoice whose row is still `creating` is not found here, and its event is
					// passed by: the panel stopped after banking issued it and before the answer was
					// stored. Nothing is lost: the next read of the row asks banking again
					// ([`Self::refresh_invoice`]) and stores what it says, paid included.
					EventKind::InvoicePaid { invoice_id, paid } =>
						if db::mark_paid(&mut tx, invoice_id, paid, &event.event_id).await? {
							done.paid += 1;
							tracing::info!(invoice_id, event_id = event.event_id, "invoice paid");
						},
					EventKind::InvoiceCancelled { invoice_id } => ended(&mut tx, invoice_id, InvoiceState::Cancelled).await?,
					EventKind::InvoiceExpired { invoice_id } => ended(&mut tx, invoice_id, InvoiceState::Expired).await?,
					EventKind::Other => {}
				}
			}
			done.position = page.next_position.max(after);
			db::set_cursor(&mut tx, done.position).await?;
			tx.commit().await.wrap_err("committing confirmed invoices")?;
			if !full {
				break;
			}
		}
		Ok(done)
	}
}

/// An invoice the panel knows was cancelled or lapsed at banking; a paid one stays paid.
async fn ended(conn: &mut sqlx::SqliteConnection, invoice_id: &str, to: InvoiceState) -> eyre::Result<()> {
	let Some(row) = db::by_invoice_id(conn, invoice_id).await? else { return Ok(()) };
	let state = row.state.after(to);
	if state != row.state {
		db::update(conn, &Invoice { state, ..row }).await?;
	}
	Ok(())
}

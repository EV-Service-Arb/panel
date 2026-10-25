//! Invoices over HTTP (`panel::invoices`): an operator issues one through banking's ScopeLedger
//! and opens its pay page for the customer; the payment is confirmed in the background
//! ([`crate::scope_ledger::run`]).
//!
//! ```text
//! POST /api/v1/invoices          invoices:create  Idempotency-Key required;
//!                                                 {amount, memo?, expires_in_days} → 201 Invoice,
//!                                                 200 Invoice on a retry
//! GET  /api/v1/invoices                           {invoices: [Invoice]}: newest first, from storage
//! GET  /api/v1/invoices/{id}                      Invoice, as banking has it now
//!
//! Invoice: {id, invoice_id | null, amount, currency, memo, state, pay_url | null, created_by,
//!           created_at, expires_at, paid_at | null, paid_by | null}
//! ```
//!
//! Errors are `{error}`: 400 for what the operator typed or banking found wrong with it; 409 for
//! a key used for other terms, or banking's reason (`invoice_not_open`, `scope_frozen`, …); 503
//! `invoices_unavailable` when the panel has no ledger configured, and banking's reason
//! (`not_granted`) or `ledger_unavailable` when banking cannot do it now.

use std::sync::Arc;

use axum::{
	Extension, Json,
	extract::{Path, State, rejection::JsonRejection},
	http::{HeaderMap, Method},
	response::Response,
};
use jiff::Timestamp;
use panel::{
	Panel,
	invoices::{Invoice, InvoiceError, LedgerError, NewInvoice},
	operator::Actor,
};
use panel_core::invoice::CURRENCY;
use sa_auth::Invoices;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
	api::{ApiError, ApiResult, body, idempotency_key, recorded},
	http::{ApiRoute, Section},
	scope_ledger::ScopeLedger,
	signin::{Caller, Freshness},
};

/// The ledger, when the panel is configured for one (`SCOPE_LEDGER_*`).
type MaybeLedger = Option<Extension<Arc<ScopeLedger>>>;

pub(crate) fn routes() -> Vec<ApiRoute> {
	use Freshness::{Cached, Fresh};
	use Method as M;
	let work = Some(Section::Work);
	vec![
		ApiRoute::new(M::GET, "/invoices", work, Cached, list),
		ApiRoute::new(M::GET, "/invoices/{id}", work, Cached, item),
		// Money is asked for on a permission concierge confirms now, not one cached minutes ago.
		ApiRoute::new(M::POST, "/invoices", work, Fresh, create),
	]
}

fn ledger(l: MaybeLedger) -> ApiResult<Arc<ScopeLedger>> {
	l.map(|Extension(l)| l).ok_or_else(|| ApiError::Unavailable("invoices_unavailable".into()))
}

impl From<InvoiceError> for ApiError {
	fn from(e: InvoiceError) -> Self {
		match e {
			InvoiceError::Invalid(e) => Self::BadRequest(e.0),
			InvoiceError::Conflict(m) => Self::Conflict(m.to_owned()),
			InvoiceError::NotFound => Self::NotFound,
			InvoiceError::Ledger(e) => e.into(),
			InvoiceError::Internal(e) => Self::Internal(e),
		}
	}
}

impl From<LedgerError> for ApiError {
	fn from(e: LedgerError) -> Self {
		match e {
			// A grant banking has not given the panel is configuration, not the operator's doing.
			LedgerError::Refused(reason) if reason == "not_granted" => Self::Unavailable(reason),
			LedgerError::Refused(reason) | LedgerError::Conflict(reason) => Self::Conflict(reason),
			LedgerError::Invalid(said) => Self::BadRequest(said),
			// Every invoice the panel asks of banking is one it stored: banking not knowing it
			// is banking's state gone wrong, not the caller's 404.
			LedgerError::NotFound => Self::Internal(eyre::eyre!("the ledger does not know an invoice the panel stored")),
			LedgerError::Unavailable(said) => {
				tracing::warn!(reason = said, "the ScopeLedger could not be reached");
				Self::Unavailable("ledger_unavailable".into())
			}
			LedgerError::Protocol(said) => Self::Internal(eyre::eyre!("the ScopeLedger broke its contract: {said}")),
		}
	}
}

#[derive(Serialize)]
struct InvoiceDto {
	id: String,
	invoice_id: Option<String>,
	amount: String,
	currency: &'static str,
	memo: String,
	/// creating | open | processing | paid | cancelled | expired
	state: &'static str,
	pay_url: Option<String>,
	created_by: String,
	created_at: String,
	expires_at: String,
	paid_at: Option<String>,
	/// The payer's `auth_subject` at banking.
	paid_by: Option<String>,
}

fn dto(i: Invoice) -> InvoiceDto {
	InvoiceDto {
		id: i.id.to_string(),
		invoice_id: i.invoice_id,
		amount: i.amount,
		currency: CURRENCY,
		memo: i.memo,
		state: i.state.as_str(),
		pay_url: i.pay_url,
		created_by: i.created_by.to_string(),
		created_at: i.created_at.to_string(),
		expires_at: i.expires_at.to_string(),
		paid_at: i.paid.as_ref().map(|p| p.at.to_string()),
		paid_by: i.paid.map(|p| p.by),
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
	/// Decimal USDT as a string: a JSON number would round it.
	amount: String,
	#[serde(default)]
	memo: String,
	expires_in_days: u32,
}

async fn create(State(panel): State<Panel>, Extension(caller): Extension<Caller>, l: MaybeLedger, headers: HeaderMap, b: Result<Json<CreateBody>, JsonRejection>) -> ApiResult<Response> {
	if !caller.permissions.may(Invoices::Create) {
		return Err(ApiError::Forbidden);
	}
	let ledger = ledger(l)?;
	// Required, not optional as elsewhere: a lost answer retried without one would bill twice.
	let key = idempotency_key(&headers)?.ok_or_else(|| ApiError::BadRequest("an invoice needs an Idempotency-Key".into()))?;
	let b = body(b)?;
	let new = NewInvoice {
		amount: b.amount,
		memo: b.memo,
		expires_in_days: b.expires_in_days,
	};
	let done = panel.issue_invoice_once(&*ledger, Actor(caller.user_id), new, &key, Timestamp::now()).await?;
	Ok(recorded(done.replayed, json!(dto(done.value))))
}

async fn list(State(panel): State<Panel>) -> ApiResult<Json<serde_json::Value>> {
	let invoices: Vec<InvoiceDto> = panel.invoices().await?.into_iter().map(dto).collect();
	Ok(Json(json!({ "invoices": invoices })))
}

async fn item(State(panel): State<Panel>, l: MaybeLedger, Path(id): Path<String>) -> ApiResult<Json<InvoiceDto>> {
	let ledger = ledger(l)?;
	let id = Uuid::parse_str(&id).map_err(|_| ApiError::NotFound)?;
	Ok(Json(dto(panel.refresh_invoice(&*ledger, id).await?)))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn status(e: impl Into<ApiError>) -> u16 {
		axum::response::IntoResponse::into_response(e.into()).status().as_u16()
	}

	#[test]
	fn banking_s_answers_map_to_the_contract() {
		assert_eq!(status(LedgerError::Refused("not_granted".into())), 503);
		assert_eq!(status(LedgerError::Refused("scope_frozen".into())), 409);
		assert_eq!(status(LedgerError::Conflict("idempotency_conflict".into())), 409);
		assert_eq!(status(LedgerError::Invalid("memo has a link".into())), 400);
		assert_eq!(status(LedgerError::Unavailable("down".into())), 503);
		assert_eq!(status(InvoiceError::NotFound), 404);
	}
}

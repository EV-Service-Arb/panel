//! banking's ScopeLedger over gRPC (`contracts/proto/scope`, vendored): the panel's
//! invoices in the scope `sa` ([`panel::invoices::Ledger`]), and the background pass that
//! confirms their payments from `MoneyEvents` ([`run`]).
//!
//! banking proves the panel twice, as it does invest (whose client this follows): the client
//! certificate (mTLS; banking maps its fingerprint to the panel's client id) and a
//! `typ=service` token whose `sub` must be that same client, sent as `authorization: Bearer`.
//! The token is minted over the same channel (`ScopeAuthService.MintServiceToken`) and
//! renewed before it lapses (`Tokens`, below); nothing is provisioned out of band.
//!
//! Refusals arrive as (code, message), the machine reason as the whole message; they become a
//! [`LedgerError`] and nothing else. No call is retried here: an invoice is retried by its
//! operator under the same idempotency key, and the pull by the next pass.

use std::{sync::Arc, time::Duration};

use jiff::Timestamp;
use panel::{
	Panel,
	invoices::{EventKind, EventPage, IssueRequest, Issued, Ledger, LedgerError, MoneyEvent, Paid},
};
use panel_contracts::scope::v1::{
	self as pb, MintServiceTokenRequest, invoice_service_client::InvoiceServiceClient, money_event::Kind, money_events_client::MoneyEventsClient,
	scope_auth_service_client::ScopeAuthServiceClient, scope_ledger_service_client::ScopeLedgerServiceClient,
};
use panel_core::invoice::{self as rules, InvoiceState};
use tokio::{sync::watch, time::Instant};
use tonic::{
	Code, Request, Status,
	metadata::{Ascii, MetadataValue},
	transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity},
};
use zeroize::Zeroizing;

/// Deadline on one call, connecting included. An invoice is retried by its operator, the pull
/// by the next pass: a short bound costs a retry, and a hung banking holds nothing. A first
/// create runs a mint, `OpenAccount` and `CreateInvoice` in a row, and 5 + 4 + 4 s stays under
/// the 15 s `/api/v1` timeout.
const CALL_TIMEOUT: Duration = Duration::from_secs(4);

/// How often the payments are confirmed from the money events.
pub const CONFIRM_EVERY: Duration = Duration::from_secs(15);

/// Where banking's ScopeLedger listens and how the panel proves itself to it. Every PEM is the
/// file's contents, read at boot.
pub struct ScopeLedgerConfig {
	/// `https://host:port`: banking's `SCOPE_LEDGER_BIND` (50054).
	pub addr: String,
	/// The CA banking's server certificate chains to: cluster-internal, so pinned rather than
	/// trusted from the system store.
	pub ca_pem: String,
	/// The panel's client certificate and key: the identity banking maps to the panel's client.
	pub client_cert_pem: String,
	pub client_key_pem: Zeroizing<String>,
}

impl std::fmt::Debug for ScopeLedgerConfig {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ScopeLedgerConfig").field("addr", &self.addr).finish_non_exhaustive()
	}
}

/// The panel's client of banking's ScopeLedger listener.
pub struct ScopeLedger {
	ledger: ScopeLedgerServiceClient<Channel>,
	invoices: InvoiceServiceClient<Channel>,
	events: MoneyEventsClient<Channel>,
	tokens: Tokens,
	/// Set once `OpenAccount` has succeeded: the account exists for good after that.
	opened: tokio::sync::OnceCell<()>,
}

impl std::fmt::Debug for ScopeLedger {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ScopeLedger").field("account_opened", &self.opened.initialized()).finish_non_exhaustive()
	}
}

impl ScopeLedger {
	/// A client over mTLS. Lazy: nothing is dialled until the first call, so a banking that is
	/// down at boot is a failed call later, not a failed boot.
	pub fn connect(config: &ScopeLedgerConfig) -> eyre::Result<Self> {
		let tls = ClientTlsConfig::new()
			.ca_certificate(Certificate::from_pem(&config.ca_pem))
			.identity(Identity::from_pem(&config.client_cert_pem, config.client_key_pem.as_bytes()));
		let channel = Endpoint::from_shared(config.addr.clone())
			.map_err(|e| eyre::eyre!("SCOPE_LEDGER_ADDR {}: {e}", config.addr))?
			.timeout(CALL_TIMEOUT)
			.connect_timeout(CALL_TIMEOUT)
			.tls_config(tls)
			.map_err(|e| eyre::eyre!("the ScopeLedger TLS files: {e}"))?
			.connect_lazy();
		Ok(Self::over_channel(channel))
	}

	/// A client over a channel already built: TLS by [`Self::connect`], or the tests' plain one
	/// to a fake ledger on a local port.
	pub fn over_channel(channel: Channel) -> Self {
		Self {
			ledger: ScopeLedgerServiceClient::new(channel.clone()),
			invoices: InvoiceServiceClient::new(channel.clone()),
			events: MoneyEventsClient::new(channel.clone()),
			tokens: Tokens::new(ScopeAuthServiceClient::new(channel)),
			opened: tokio::sync::OnceCell::new(),
		}
	}

	/// One call with a bearer good now. No token, no call: that is UNAVAILABLE. A call banking
	/// refuses as UNAUTHENTICATED drops the token it carried, for the next call to mint anew.
	async fn send<C, T, R, Fut>(&self, client: &C, message: T, call: impl FnOnce(C, Request<T>) -> Fut) -> Result<R, Status>
	where
		C: Clone,
		Fut: Future<Output = Result<tonic::Response<R>, Status>>, {
		let (bearer, generation) = self.tokens.bearer().await.map_err(|why| Status::unavailable(format!("no service token: {why}")))?;
		let mut request = Request::new(message);
		request.metadata_mut().insert("authorization", bearer);
		match call(client.clone(), request).await {
			Ok(response) => Ok(response.into_inner()),
			Err(status) => {
				if status.code() == Code::Unauthenticated {
					self.tokens.reject(generation).await;
				}
				Err(status)
			}
		}
	}
}

impl Ledger for ScopeLedger {
	async fn open_account(&self) -> Result<(), LedgerError> {
		self.opened
			.get_or_try_init(|| async {
				let message = pb::OpenAccountRequest {
					scope: rules::SCOPE.to_owned(),
					label: rules::ACCOUNT_LABEL.to_owned(),
					idempotency_key: rules::OPEN_ACCOUNT_KEY.to_owned(),
				};
				let account = self
					.send(&self.ledger, message, |mut c, r| async move { c.open_account(r).await })
					.await
					.map_err(|s| from_status("OpenAccount", &s))?;
				if account.scope != rules::SCOPE || account.label != rules::ACCOUNT_LABEL {
					return Err(LedgerError::Protocol(format!("OpenAccount answered ({}, {})", account.scope, account.label)));
				}
				tracing::info!(scope = account.scope, label = account.label, owner = account.owner_client, "invoice account open");
				Ok(())
			})
			.await
			.map(|&()| ())
	}

	async fn create_invoice(&self, request: &IssueRequest) -> Result<Issued, LedgerError> {
		let message = pb::CreateInvoiceRequest {
			scope: rules::SCOPE.to_owned(),
			to_label: rules::ACCOUNT_LABEL.to_owned(),
			amount: request.amount.clone(),
			memo: request.memo.clone(),
			expires_at: request.expires_at.as_second(),
			// Anyone signed in on evinvest.ltd may pay: the panel does not know its customers'
			// concierge accounts.
			payer_auth_subject: String::new(),
			idempotency_key: request.key.clone(),
			subject_ref: request.subject_ref.clone(),
		};
		let invoice = self
			.send(&self.invoices, message, |mut c, r| async move { c.create_invoice(r).await })
			.await
			.map_err(|s| from_status("CreateInvoice", &s))?;
		if invoice.subject_ref != request.subject_ref {
			return Err(LedgerError::Protocol(format!(
				"CreateInvoice answered for {} instead of {}",
				invoice.subject_ref, request.subject_ref
			)));
		}
		issued(invoice)
	}

	async fn get_invoice(&self, invoice_id: &str) -> Result<Issued, LedgerError> {
		let message = pb::GetInvoiceRequest {
			scope: rules::SCOPE.to_owned(),
			invoice_id: invoice_id.to_owned(),
		};
		let invoice = self
			.send(&self.invoices, message, |mut c, r| async move { c.get_invoice(r).await })
			.await
			.map_err(|s| from_status("GetInvoice", &s))?;
		issued(invoice)
	}

	async fn pull(&self, after_position: i64, limit: u32) -> Result<EventPage, LedgerError> {
		let message = pb::PullRequest {
			scope: rules::SCOPE.to_owned(),
			after_position,
			limit,
		};
		let page = self
			.send(&self.events, message, |mut c, r| async move { c.pull(r).await })
			.await
			.map_err(|s| from_status("Pull", &s))?;
		Ok(EventPage {
			events: page.events.into_iter().map(event).collect::<Result<_, _>>()?,
			next_position: page.next_position,
		})
	}
}

/// Confirms the invoices' payments every [`CONFIRM_EVERY`] until `shutdown`.
pub async fn run(panel: Panel, ledger: Arc<ScopeLedger>, shutdown: watch::Receiver<bool>) {
	crate::every(CONFIRM_EVERY, shutdown, "confirming invoices from banking's money events", || async {
		panel.confirm_invoices(&*ledger).await.map(|_| ())
	})
	.await;
}

/// A status as the port's error. Banking's reason is passed on only when it looks like one.
fn from_status(rpc: &str, status: &Status) -> LedgerError {
	let message = status.message();
	let reason = || {
		let machine = (1..=64).contains(&message.len()) && message.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
		if machine { message.to_owned() } else { format!("{rpc} refused") }
	};
	let said = || format!("{rpc}: {:?}: {}", status.code(), message.chars().take(200).collect::<String>());
	match status.code() {
		Code::FailedPrecondition | Code::PermissionDenied => LedgerError::Refused(reason()),
		Code::AlreadyExists => LedgerError::Conflict(reason()),
		Code::NotFound => LedgerError::NotFound,
		Code::InvalidArgument => LedgerError::Invalid(said()),
		Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled | Code::ResourceExhausted | Code::Aborted | Code::Internal | Code::Unknown | Code::Unauthenticated =>
			LedgerError::Unavailable(said()),
		Code::Ok | Code::OutOfRange | Code::DataLoss | Code::Unimplemented => LedgerError::Protocol(said()),
	}
}

fn at(rpc: &str, unix: i64) -> Result<Timestamp, LedgerError> {
	Timestamp::from_second(unix).map_err(|e| LedgerError::Protocol(format!("{rpc}: time {unix}: {e}")))
}

fn issued(i: pb::Invoice) -> Result<Issued, LedgerError> {
	let protocol = |what: String| LedgerError::Protocol(format!("invoice {}: {what}", i.invoice_id));
	if i.invoice_id.is_empty() || i.scope != rules::SCOPE {
		return Err(protocol(format!("in scope {:?}", i.scope)));
	}
	let state = match pb::InvoiceState::try_from(i.state) {
		Ok(pb::InvoiceState::Open) => InvoiceState::Open,
		Ok(pb::InvoiceState::Processing) => InvoiceState::Processing,
		Ok(pb::InvoiceState::Paid) => InvoiceState::Paid,
		Ok(pb::InvoiceState::Cancelled) => InvoiceState::Cancelled,
		Ok(pb::InvoiceState::Expired) => InvoiceState::Expired,
		Ok(pb::InvoiceState::Unspecified) | Err(_) => return Err(protocol(format!("state {}", i.state))),
	};
	let paid = match state {
		InvoiceState::Paid => {
			if i.paid_by_auth_subject.is_empty() || i.paid_at <= 0 {
				return Err(protocol("paid, but no payer or time".into()));
			}
			Some(Paid {
				at: at("Invoice.paid_at", i.paid_at)?,
				by: i.paid_by_auth_subject.clone(),
			})
		}
		InvoiceState::Creating | InvoiceState::Open | InvoiceState::Processing | InvoiceState::Cancelled | InvoiceState::Expired => None,
	};
	// The front end opens it for the payer: a web page, nothing else.
	let page = url::Url::parse(&i.pay_url).map_err(|e| protocol(format!("pay_url: {e}")))?;
	if !matches!(page.scheme(), "https" | "http") {
		return Err(protocol(format!("pay_url {}", i.pay_url)));
	}
	Ok(Issued {
		invoice_id: i.invoice_id,
		state,
		pay_url: i.pay_url,
		paid,
	})
}

fn event(e: pb::MoneyEvent) -> Result<MoneyEvent, LedgerError> {
	let protocol = |what: &str| LedgerError::Protocol(format!("money event {} at {}: {what}", e.event_id, e.position));
	if e.event_id.is_empty() || e.scope != rules::SCOPE {
		return Err(protocol(&format!("in scope {:?}", e.scope)));
	}
	let invoice_id = || if e.ref_id.is_empty() { Err(protocol("no invoice id")) } else { Ok(e.ref_id.clone()) };
	let kind = match Kind::try_from(e.kind) {
		Ok(Kind::InvoicePaid) => {
			if e.auth_subject.is_empty() {
				return Err(protocol("an invoice paid by nobody"));
			}
			EventKind::InvoicePaid {
				invoice_id: invoice_id()?,
				paid: Paid {
					at: at("MoneyEvent.occurred_at", e.occurred_at)?,
					by: e.auth_subject.clone(),
				},
			}
		}
		Ok(Kind::InvoiceCancelled) => EventKind::InvoiceCancelled { invoice_id: invoice_id()? },
		Ok(Kind::InvoiceExpired) => EventKind::InvoiceExpired { invoice_id: invoice_id()? },
		Ok(
			Kind::Unspecified
			| Kind::AccountOpened
			| Kind::CompartmentIn
			| Kind::CompartmentOut
			| Kind::HoldPlaced
			| Kind::HoldCaptured
			| Kind::HoldVoided
			| Kind::HoldExpired
			| Kind::Charged
			| Kind::PaidOut
			| Kind::Transferred
			| Kind::InvoiceCreated
			| Kind::MandateGranted
			| Kind::MandateRevoked
			| Kind::OperationRequested
			| Kind::OperationRejected
			| Kind::OperationFailed,
		) => EventKind::Other,
		// A kind newer than the vendored proto: banking ships kinds before every consumer knows
		// them, and a consumer is to keep its cursor moving past them.
		Err(_) => EventKind::Other,
	};
	Ok(MoneyEvent { event_id: e.event_id, kind })
}

// ── the service token ───────────────────────────────────────────────────────────────────────

/// A token's refresh margin is a fifth of the life banking granted, within these bounds.
const MAX_MARGIN_SECS: i64 = 60;
const MIN_MARGIN_SECS: i64 = 5;
/// A token arriving with less life than this is a clock or TTL gone wrong, not a token.
const MIN_LIFETIME_SECS: i64 = 5;
/// A held token with less left than this would lapse in flight.
const USABLE_FLOOR_SECS: i64 = 2;
/// banking lets a client mint once per 5 s; no mint starts sooner after the last.
const MIN_MINT_INTERVAL: Duration = Duration::from_secs(5);
const MINT_TIMEOUT: Duration = Duration::from_secs(5);

type Bearer = MetadataValue<Ascii>;

/// The `typ=service` bearer, cached and renewed (`scope.v1.ScopeAuthService`, `service_auth.proto`):
///
/// - more than its margin left: used as is;
/// - less than the margin, more than [`USABLE_FLOOR_SECS`]: a new one is minted, and if the mint
///   fails or is not due yet, the held one is used meanwhile;
/// - none, expired or refused: minted, or the last failure while the next mint is not due.
///
/// One mint at a time: callers wait on the lock while it runs, so a burst mints once. The token
/// is a credential: it never reaches an error, a log or `Debug`, and its header is sensitive.
struct Tokens {
	minter: ScopeAuthServiceClient<Channel>,
	state: tokio::sync::Mutex<TokenState>,
}

#[derive(Default)]
struct TokenState {
	held: Option<Held>,
	last_mint: Option<Instant>,
	/// Why the last mint failed; cleared by the next good one.
	failed: Option<String>,
	generations: u64,
}

struct Held {
	bearer: Bearer,
	expires_at: i64,
	margin: i64,
	generation: u64,
}

impl Tokens {
	fn new(minter: ScopeAuthServiceClient<Channel>) -> Self {
		Self {
			minter,
			state: tokio::sync::Mutex::new(TokenState::default()),
		}
	}

	/// A bearer good for one call, and the generation of the token it came from.
	async fn bearer(&self) -> Result<(Bearer, u64), String> {
		let mut state = self.state.lock().await;
		let now = Timestamp::now().as_second();
		let left = |h: &Held| h.expires_at.saturating_sub(now);
		if let Some(h) = state.held.as_ref().filter(|h| left(h) >= h.margin) {
			return Ok((h.bearer.clone(), h.generation));
		}
		let usable = state.held.as_ref().filter(|h| left(h) >= USABLE_FLOOR_SECS).map(|h| (h.bearer.clone(), h.generation));
		if state.last_mint.is_some_and(|at| at.elapsed() < MIN_MINT_INTERVAL) {
			return usable.ok_or_else(|| state.failed.clone().unwrap_or_else(|| "the next mint is not due yet".to_owned()));
		}
		state.last_mint = Some(Instant::now());
		match self.mint().await.and_then(|(token, expires_at)| accept(&token, expires_at, Timestamp::now().as_second())) {
			Ok((bearer, expires_at, margin)) => {
				state.generations += 1;
				let generation = state.generations;
				state.held = Some(Held {
					bearer: bearer.clone(),
					expires_at,
					margin,
					generation,
				});
				state.failed = None;
				Ok((bearer, generation))
			}
			Err(why) => {
				tracing::warn!(reason = why, "the ScopeLedger service token could not be minted");
				state.failed = Some(why.clone());
				usable.ok_or(why)
			}
		}
	}

	async fn mint(&self) -> Result<(Zeroizing<String>, i64), String> {
		let mut request = Request::new(MintServiceTokenRequest {});
		request.set_timeout(MINT_TIMEOUT);
		let minted = self
			.minter
			.clone()
			.mint_service_token(request)
			.await
			.map_err(|s| format!("MintServiceToken: {:?}: {}", s.code(), s.message().chars().take(200).collect::<String>()))?
			.into_inner();
		Ok((Zeroizing::new(minted.token), minted.expires_at))
	}

	/// banking refused a call carrying the token of `generation`: it is dropped, unless a newer
	/// one replaced it meanwhile.
	async fn reject(&self, generation: u64) {
		let mut state = self.state.lock().await;
		if state.held.as_ref().is_some_and(|h| h.generation == generation) {
			state.held = None;
			state.failed = Some("the ledger refused the service token".to_owned());
			tracing::warn!("the ScopeLedger refused the service token; the next call mints another");
		}
	}
}

/// A minted token as a header, its expiry, and its margin; or why it is no token.
fn accept(token: &str, expires_at: i64, now: i64) -> Result<(Bearer, i64, i64), String> {
	if token.is_empty() {
		return Err("MintServiceToken answered an empty token".to_owned());
	}
	let lifetime = expires_at.saturating_sub(now);
	if lifetime < MIN_LIFETIME_SECS {
		return Err(format!("MintServiceToken answered a token valid for {lifetime} s"));
	}
	let mut bearer: Bearer = format!("Bearer {token}")
		.parse()
		.map_err(|_| "MintServiceToken answered a token that is no header value".to_owned())?;
	bearer.set_sensitive(true);
	Ok((bearer, expires_at, (lifetime / 5).clamp(MIN_MARGIN_SECS, MAX_MARGIN_SECS)))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_token_is_a_sensitive_header_with_a_fifth_of_its_life_as_margin() {
		let (bearer, expires_at, margin) = accept("t0k3n", 1_000_300, 1_000_000).unwrap();
		assert!(bearer.is_sensitive());
		assert_eq!(bearer.to_str().unwrap(), "Bearer t0k3n");
		assert_eq!((expires_at, margin), (1_000_300, 60));
		assert_eq!(accept("t", 1_000_020, 1_000_000).unwrap().2, MIN_MARGIN_SECS, "a fifth of 20 s is below the floor");
		assert!(accept("", 1_000_300, 1_000_000).is_err());
		assert!(accept("t", 1_000_004, 1_000_000).is_err(), "a token about to lapse is refused");
		let e = accept("bad\ntoken", 1_000_300, 1_000_000).unwrap_err();
		assert!(!e.contains("bad"), "the token never reaches an error: {e}");
	}

	#[test]
	fn refusals_keep_banking_s_reason_and_nothing_else() {
		let s = |code, msg: &str| from_status("CreateInvoice", &Status::new(code, msg));
		assert_eq!(s(Code::PermissionDenied, "not_granted"), LedgerError::Refused("not_granted".into()));
		assert_eq!(s(Code::FailedPrecondition, "invoice_not_open"), LedgerError::Refused("invoice_not_open".into()));
		assert_eq!(s(Code::FailedPrecondition, "<script>"), LedgerError::Refused("CreateInvoice refused".into()));
		assert_eq!(s(Code::AlreadyExists, "idempotency_conflict"), LedgerError::Conflict("idempotency_conflict".into()));
		assert!(matches!(s(Code::InvalidArgument, "amount must be a decimal number"), LedgerError::Invalid(m) if m.contains("decimal")));
		assert!(matches!(s(Code::Unavailable, "down"), LedgerError::Unavailable(_)));
		assert!(matches!(s(Code::Unauthenticated, "token"), LedgerError::Unavailable(_)));
		assert!(matches!(s(Code::Unimplemented, ""), LedgerError::Protocol(_)));
	}

	fn invoice(state: pb::InvoiceState) -> pb::Invoice {
		pb::Invoice {
			invoice_id: "inv-1".into(),
			scope: "sa".into(),
			to_label: "sa:invoices".into(),
			amount: "12.5".into(),
			state: state.into(),
			pay_url: "https://evinvest.ltd/pay/inv-1".into(),
			..pb::Invoice::default()
		}
	}

	#[test]
	fn processing_is_not_paid_and_paid_names_its_payer() {
		let p = issued(invoice(pb::InvoiceState::Processing)).unwrap();
		assert_eq!((p.state, p.paid), (InvoiceState::Processing, None));
		assert!(issued(invoice(pb::InvoiceState::Paid)).is_err(), "paid without a payer is no answer");
		let paid = issued(pb::Invoice {
			paid_by_auth_subject: "subj-1".into(),
			paid_at: 1_790_000_000,
			..invoice(pb::InvoiceState::Paid)
		})
		.unwrap();
		assert_eq!(paid.paid.unwrap().by, "subj-1");
		assert!(issued(invoice(pb::InvoiceState::Unspecified)).is_err());
		assert!(
			issued(pb::Invoice {
				scope: "invest".into(),
				..invoice(pb::InvoiceState::Open)
			})
			.is_err()
		);
		assert!(
			issued(pb::Invoice {
				pay_url: "javascript:alert(1)".into(),
				..invoice(pb::InvoiceState::Open)
			})
			.is_err()
		);
	}

	#[test]
	fn other_kinds_and_unknown_kinds_are_skipped() {
		let e = |kind: i32| pb::MoneyEvent {
			event_id: "e-1".into(),
			position: 7,
			scope: "sa".into(),
			kind,
			ref_id: "inv-1".into(),
			..pb::MoneyEvent::default()
		};
		assert!(matches!(event(e(Kind::Charged.into())).unwrap().kind, EventKind::Other));
		assert!(matches!(event(e(999)).unwrap().kind, EventKind::Other), "a kind newer than the proto");
		assert!(event(e(Kind::InvoicePaid.into())).is_err(), "paid by nobody");
		let paid = event(pb::MoneyEvent {
			auth_subject: "subj-1".into(),
			occurred_at: 1_790_000_000,
			..e(Kind::InvoicePaid.into())
		})
		.unwrap();
		assert!(matches!(paid.kind, EventKind::InvoicePaid { ref invoice_id, .. } if invoice_id == "inv-1"));
	}
}

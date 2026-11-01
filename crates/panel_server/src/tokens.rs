//! People's tokens (`panel::tokens`): the caller's own under `/api/v1/tokens`, everyone's under
//! `/api/v1/admin/tokens` (`sa:admin:tokens:grant`), and what `serve` does with review_archive —
//! follows its charges and pushes it the snapshot, under a Conductor assertion.
//!
//! ```text
//! GET  /api/v1/tokens                  Tokens: the balance, renewed, and the kick-backs not claimed
//! GET  /api/v1/tokens/ledger?before=&limit=  Ledger: newest first, limit 1–200
//! GET  /api/v1/tokens/usage            Usage: 30 UTC days, oldest first
//! POST /api/v1/tokens/claim            → Tokens
//! GET  /api/v1/admin/tokens            TokenHolders
//! POST /api/v1/admin/tokens/{user_id}  BalanceChange (fresh) → Balance
//! ```
//!
//! Before the import from review_archive, every one of them answers 503, and `serve` waits.

use std::time::Duration;

use axum::{
	Extension, Json,
	body::{Body, to_bytes},
	extract::{
		Path, Query, State,
		rejection::{JsonRejection, QueryRejection},
	},
	http::{HeaderMap, HeaderValue, Method, StatusCode, header},
};
use jiff::Timestamp;
use panel::{
	Panel,
	tokens::{Change, ChargePage, Entry, Kickback, Owed},
};
use sa_auth::{Principal, Service};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
	api::{ApiError, ApiResult, body},
	forward::Services,
	http::{ApiRoute, Section},
	signin::{Caller, Freshness},
};

pub(crate) fn routes() -> Vec<ApiRoute> {
	use Freshness::{Cached, Fresh};
	use Method as M;
	let admin = Some(Section::Tokens);
	vec![
		ApiRoute::new(M::GET, "/tokens", None, Cached, tokens),
		ApiRoute::new(M::GET, "/tokens/ledger", None, Cached, ledger),
		ApiRoute::new(M::GET, "/tokens/usage", None, Cached, usage),
		ApiRoute::new(M::POST, "/tokens/claim", None, Cached, claim),
		ApiRoute::new(M::GET, "/admin/tokens", admin, Cached, holders),
		// Tokens must not be handed out on a permission revoked a moment ago.
		ApiRoute::new(M::POST, "/admin/tokens/{user_id}", admin, Fresh, change),
	]
}

/// What the caller is owed until they claim it: a charge kicked back (`service`, `key`,
/// `label`), or an admin's grant (`by`, `note`).
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Kickback")]
pub struct KickbackDto {
	#[ts(type = "number")]
	pub id: i64,
	/// RFC 3339.
	pub at: String,
	#[ts(type = "number")]
	pub amount: i64,
	pub service: Option<String>,
	pub key: Option<String>,
	pub label: Option<String>,
	/// The admin's email.
	pub by: Option<String>,
	pub note: Option<String>,
}

/// `GET /tokens`, `POST /tokens/claim`.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Tokens")]
pub struct TokensDto {
	#[ts(type = "number")]
	pub balance: i64,
	/// Renewal: added per day while under `cap`.
	#[ts(type = "number")]
	pub daily: i64,
	#[ts(type = "number")]
	pub cap: i64,
	pub kickbacks: Vec<KickbackDto>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "LedgerEntry")]
pub struct LedgerEntryDto {
	#[ts(type = "number")]
	pub id: i64,
	/// RFC 3339.
	pub at: String,
	#[ts(type = "number")]
	pub delta: i64,
	#[ts(type = "LedgerKind")]
	pub kind: &'static str,
	pub service: Option<String>,
	pub key: Option<String>,
	pub label: Option<String>,
	/// The admin's email.
	pub by: Option<String>,
	pub note: Option<String>,
}

/// `GET /tokens/ledger`: `next` is the `before` of the next page, `null` at the end.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Ledger")]
pub struct LedgerDto {
	pub entries: Vec<LedgerEntryDto>,
	#[ts(type = "number | null")]
	pub next: Option<i64>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "UsageDay")]
pub struct UsageDayDto {
	/// `YYYY-MM-DD`, UTC.
	pub day: String,
	#[ts(type = "number")]
	pub charges: i64,
	#[ts(type = "number")]
	pub tokens: i64,
}

/// `GET /tokens/usage`.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Usage")]
pub struct UsageDto {
	pub days: Vec<UsageDayDto>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "TokenHolder")]
pub struct TokenHolderDto {
	pub user_id: Uuid,
	pub email: String,
	pub name: String,
	#[ts(type = "number")]
	pub balance: i64,
}

/// `GET /admin/tokens`.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "TokenHolders")]
pub struct TokenHoldersDto {
	pub users: Vec<TokenHolderDto>,
}

/// `POST /admin/tokens/{user_id}`: set to 0 or more, add 1 or more (a sale, its reference in
/// the note), or grant 1 or more, owed until claimed, saying what for.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(untagged, deny_unknown_fields)]
#[ts(rename = "BalanceChange")]
pub enum BalanceChangeDto {
	Set {
		#[ts(type = "number")]
		set: i64,
		#[ts(optional)]
		note: Option<String>,
	},
	Add {
		#[ts(type = "number")]
		add: i64,
		#[ts(optional)]
		note: Option<String>,
	},
	Grant {
		#[ts(type = "number")]
		grant: i64,
		note: String,
	},
}

/// The answer to a balance change.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Balance")]
pub struct BalanceDto {
	#[ts(type = "number")]
	pub balance: i64,
}

impl From<Kickback> for KickbackDto {
	fn from(k: Kickback) -> Self {
		let (service, key, label, by, note) = match k.why {
			Owed::Charge(c) => (Some(c.service), Some(c.key), c.label, None, None),
			Owed::Grant { by, note } => (None, None, None, Some(by), Some(note)),
		};
		Self {
			id: k.id,
			at: k.at.to_string(),
			amount: k.amount,
			service,
			key,
			label,
			by,
			note,
		}
	}
}

impl From<(i64, Entry)> for LedgerEntryDto {
	fn from((id, e): (i64, Entry)) -> Self {
		let (service, key, label) = match e.charged {
			Some(c) => (Some(c.service), Some(c.key), c.label),
			None => (None, None, None),
		};
		Self {
			id,
			at: e.at.to_string(),
			delta: e.delta,
			kind: e.kind.as_str(),
			service,
			key,
			label,
			by: e.by,
			note: e.note,
		}
	}
}

impl From<BalanceChangeDto> for Change {
	fn from(b: BalanceChangeDto) -> Self {
		match b {
			BalanceChangeDto::Set { set, note } => Self::Set { n: set, note },
			BalanceChangeDto::Add { add, note } => Self::Add { n: add, note },
			BalanceChangeDto::Grant { grant, note } => Self::Grant { n: grant, note },
		}
	}
}

fn tokens_dto(panel: &Panel, (balance, kickbacks): (i64, Vec<Kickback>)) -> TokensDto {
	TokensDto {
		balance,
		daily: panel.renewal().daily,
		cap: panel.renewal().cap,
		kickbacks: kickbacks.into_iter().map(KickbackDto::from).collect(),
	}
}

async fn tokens(State(panel): State<Panel>, Extension(caller): Extension<Caller>) -> ApiResult<Json<TokensDto>> {
	let held = panel.tokens(caller.user_id, Timestamp::now()).await??;
	Ok(Json(tokens_dto(&panel, held)))
}

async fn claim(State(panel): State<Panel>, Extension(caller): Extension<Caller>) -> ApiResult<Json<TokensDto>> {
	let held = panel.claim(caller.user_id, Timestamp::now()).await??;
	tracing::info!(user_id = %caller.user_id, balance = held.0, "kick-backs claimed");
	Ok(Json(tokens_dto(&panel, held)))
}

/// The most entries a page of the ledger holds.
const LEDGER_PAGE: i64 = 200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerQuery {
	before: Option<i64>,
	limit: i64,
}

async fn ledger(State(panel): State<Panel>, Extension(caller): Extension<Caller>, q: Result<Query<LedgerQuery>, QueryRejection>) -> ApiResult<Json<LedgerDto>> {
	let Query(q) = q.map_err(|e| ApiError::BadRequest(e.body_text()))?;
	if !(1..=LEDGER_PAGE).contains(&q.limit) {
		return Err(ApiError::BadRequest(format!("limit is 1 to {LEDGER_PAGE}")));
	}
	let entries = panel.ledger(caller.user_id, q.before, q.limit).await??;
	let next = (entries.len() as i64 == q.limit).then(|| entries.last().map(|(id, _)| *id)).flatten();
	Ok(Json(LedgerDto {
		entries: entries.into_iter().map(LedgerEntryDto::from).collect(),
		next,
	}))
}

async fn usage(State(panel): State<Panel>, Extension(caller): Extension<Caller>) -> ApiResult<Json<UsageDto>> {
	let days = panel.usage(caller.user_id, Timestamp::now()).await??;
	Ok(Json(UsageDto {
		days: days
			.into_iter()
			.map(|d| UsageDayDto {
				day: d.day.to_string(),
				charges: d.charges,
				tokens: d.tokens,
			})
			.collect(),
	}))
}

async fn holders(State(panel): State<Panel>) -> ApiResult<Json<TokenHoldersDto>> {
	let holders = panel.holders(Timestamp::now()).await??;
	Ok(Json(TokenHoldersDto {
		users: holders
			.into_iter()
			.map(|(u, balance)| TokenHolderDto {
				user_id: u.user_id,
				email: u.email,
				name: u.name,
				balance,
			})
			.collect(),
	}))
}

async fn change(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path(raw): Path<String>, b: Result<Json<BalanceChangeDto>, JsonRejection>) -> ApiResult<Json<BalanceDto>> {
	let user: Uuid = raw.parse().map_err(|_| ApiError::NotFound)?;
	let balance = panel.change_balance(user, body(b)?.into(), &caller.email, Timestamp::now()).await??;
	Ok(Json(BalanceDto { balance }))
}

// ── review_archive, in `serve` ──────────────────────────────────────────────────────────

/// Charges asked of review_archive at once.
const CHARGES_PAGE: usize = 500;
/// A page is asked for this often while review_archive has no more.
const FOLLOW_EVERY: Duration = Duration::from_secs(10);
/// The snapshot goes out at least this often, renewals included.
const PUSH_EVERY: Duration = Duration::from_secs(60);
/// Before the import, or while review_archive answers 404 (not cut over yet), looked at again
/// this often.
const LOOK_AGAIN: Duration = Duration::from_secs(60);
/// After a failure: doubling from the first to the second.
const BACKOFF: (Duration, Duration) = (Duration::from_secs(1), Duration::from_secs(60));
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of review_archive's answer read: a page of charges, with room.
const ANSWER: usize = 16 * 1024 * 1024;

/// How a pass with review_archive went.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pass {
	/// That many charges taken; the snapshot pushed.
	Done(usize),
	/// Tokens are not open: the import has not run.
	Closed,
	/// review_archive answered 404: it is not cut over to the conductor's tokens yet.
	NotCutOver,
}

/// Follows review_archive's charges and pushes it the snapshot until `shutdown` turns true.
/// Nothing here ends `serve`.
pub async fn run(panel: Panel, services: Services, shutdown: watch::Receiver<bool>) {
	let follow = repeat("following review_archive's charges", FOLLOW_EVERY, std::future::pending, shutdown.clone(), || async {
		let Some(after) = panel.charges_after().await? else { return Ok(Pass::Closed) };
		let (status, answer) = call(&services, Method::GET, &format!("/api/conductor/charges?after={after}&limit={CHARGES_PAGE}"), None).await?;
		match status {
			StatusCode::NOT_FOUND => return Ok(Pass::NotCutOver),
			StatusCode::OK => {}
			other => eyre::bail!("review_archive answered its charges with {other}: {}", String::from_utf8_lossy(&answer)),
		}
		let page: ChargePage = serde_json::from_slice(&answer).map_err(|e| eyre::eyre!("review_archive's charges: {e}"))?;
		Ok(Pass::Done(panel.take_charges(page, after, Timestamp::now()).await?))
	});
	let push = repeat(
		"pushing review_archive the snapshot",
		PUSH_EVERY,
		|| panel.snapshot_due(),
		shutdown,
		|| async {
			let Some(snapshot) = panel.snapshot(Timestamp::now()).await? else { return Ok(Pass::Closed) };
			let body = serde_json::to_vec(&snapshot).expect("a snapshot serializes");
			let (status, answer) = call(&services, Method::PUT, "/api/conductor/gmbs", Some(body)).await?;
			match status {
				StatusCode::NOT_FOUND => Ok(Pass::NotCutOver),
				StatusCode::NO_CONTENT => Ok(Pass::Done(0)),
				other => eyre::bail!("review_archive refused the snapshot with {other}: {}", String::from_utf8_lossy(&answer)),
			}
		},
	);
	tokio::join!(follow, push);
}

/// `pass` until `shutdown`: again at once after a full page of charges, else after `every` or
/// as soon as `early` resolves; a closed or absent counterpart is said once and looked at again
/// after LOOK_AGAIN (or `early`); a failure is reported and tried again after a growing wait.
async fn repeat<F, Fut, E, EFut>(what: &'static str, every: Duration, early: E, mut shutdown: watch::Receiver<bool>, pass: F)
where
	F: Fn() -> Fut,
	Fut: Future<Output = eyre::Result<Pass>>,
	E: Fn() -> EFut,
	EFut: Future<Output = ()>, {
	let mut said: Option<Pass> = None;
	let mut backoff = BACKOFF.0;
	while !*shutdown.borrow() {
		let (wait, wakes) = match pass().await {
			Ok(Pass::Done(n)) => {
				said = None;
				backoff = BACKOFF.0;
				if n == CHARGES_PAGE { (Duration::ZERO, false) } else { (every, true) }
			}
			Ok(p) => {
				if said != Some(p) {
					tracing::info!(state = ?p, "{what}: waiting");
					said = Some(p);
				}
				(LOOK_AGAIN, true)
			}
			Err(e) => {
				crate::report(&e, what);
				let wait = backoff;
				backoff = (backoff * 2).min(BACKOFF.1);
				(wait, false)
			}
		};
		let early = async {
			if wakes {
				early().await;
			} else {
				std::future::pending::<()>().await;
			}
		};
		tokio::select! {
			() = tokio::time::sleep(wait) => {}
			() = early => {}
			_ = shutdown.changed() => return,
		}
	}
}

/// `method path` of review_archive as the conductor: its status and body.
async fn call(services: &Services, method: Method, path: &str, json: Option<Vec<u8>>) -> eyre::Result<(StatusCode, axum::body::Bytes)> {
	let mut headers = HeaderMap::new();
	let body = match json {
		Some(json) => {
			headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
			Body::from(json)
		}
		None => Body::empty(),
	};
	let sent = services.send(Service::ReviewArchive, method, path, headers, Some(Principal::Conductor), body);
	let res = tokio::time::timeout(CALL_TIMEOUT, sent)
		.await
		.map_err(|_| eyre::eyre!("review_archive did not answer {path} in time"))??;
	let status = res.status();
	let answer = to_bytes(res.into_body(), ANSWER)
		.await
		.map_err(|e| eyre::eyre!("reading review_archive's answer to {path}: {e}"))?;
	Ok((status, answer))
}

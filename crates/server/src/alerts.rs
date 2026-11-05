//! Alerts in `serve`: the services' feeds and concierge as the hub reads them, the hub's work, and
//! the `/api/v1/alerts` routes — every signed-in user's own destinations. What goes where is the
//! engine's (`sa_panel::alerts`, `sa_panel::telegram`).

use std::{collections::BTreeMap, time::Duration};

use axum::{
	Extension, Json,
	body::Body,
	extract::{FromRef, Path, State, rejection::JsonRejection},
	http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
	response::{IntoResponse, Response},
};
use jiff::Timestamp;
use sa_alerts::{Call, NotePage, Origin, Rule};
use sa_auth::{Principal, Service, User};
use sa_panel::{
	Panel, Refused,
	alerts::{Destination, Directory, DirectoryError, Feeds, Hub, Identity},
	telegram::Kind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use ts_rs::TS;

use crate::{
	api::{ApiError, ApiResult, body},
	concierge::Concierge,
	forward::{SITES_TIMEOUT, Services},
	http::{ApiRoute, ApiState},
	signin::{Caller, Freshness},
	telegram::{self, BotApi, BotName},
};

/// The hub with its Telegram bot, as `serve` runs it.
#[derive(Clone)]
pub struct Telegram {
	pub hub: Hub<BotApi, Concierge, Services>,
	pub name: BotName,
}

impl FromRef<ApiState> for Option<Telegram> {
	fn from_ref(s: &ApiState) -> Self {
		s.telegram.clone()
	}
}

// ── /api/v1/alerts ──────────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Vec<ApiRoute> {
	use Freshness::{Cached, Fresh};
	use Method as M;
	vec![
		ApiRoute::new(M::GET, "/alerts", None, Cached, alerts),
		// What a link token or a chat carries is concierge's answer of this moment.
		ApiRoute::new(M::POST, "/alerts/telegram/link", None, Fresh, link),
		ApiRoute::new(M::POST, "/alerts/telegram/chats", None, Fresh, add_chat),
		ApiRoute::new(M::PUT, "/alerts/destinations/{id}/rules", None, Cached, set_rules),
		ApiRoute::new(M::PUT, "/alerts/destinations/{id}/gmail", None, Cached, set_gmail),
		ApiRoute::new(M::POST, "/alerts/destinations/{id}/test", None, Cached, test),
		ApiRoute::new(M::DELETE, "/alerts/destinations/{id}", None, Cached, remove),
	]
}

/// `GET /alerts`, and what every change answers.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Alerts")]
pub struct AlertsDto {
	/// A bot is configured: Telegram destinations can be added.
	pub enabled: bool,
	/// The bot's `@username`, once known: what a channel adds as an admin.
	pub bot: Option<String>,
	/// The rules open to the caller: their permissions', and the GMB rules once they hold a GMB.
	#[ts(type = "Array<Rule>")]
	pub rules: Vec<&'static str>,
	pub destinations: Vec<DestinationDto>,
}

#[derive(Clone, Debug, Serialize, TS)]
#[ts(rename = "Destination")]
pub struct DestinationDto {
	#[ts(type = "number")]
	pub id: i64,
	/// `private`: the caller's own chat with the bot, the only one PII and buttons go to.
	#[ts(type = "\"private\" | \"channel\" | \"group\"")]
	pub kind: &'static str,
	/// A private chat's account, a channel's or group's title.
	pub title: Option<String>,
	#[ts(type = "number | null")]
	pub topic_id: Option<i64>,
	/// The GMB rules only for this gmail's GMBs.
	#[ts(type = "number | null")]
	pub gmail_id: Option<i64>,
	/// Why nothing reaches it: a test that arrives clears it.
	pub dead: Option<String>,
	/// Every rule open to the caller, on or off here.
	#[ts(type = "Partial<Record<Rule, boolean>>")]
	pub rules: BTreeMap<&'static str, bool>,
}

fn dto(t: Option<&Telegram>, alerts: sa_panel::alerts::Alerts) -> AlertsDto {
	let destination = |d: Destination| DestinationDto {
		id: d.id,
		kind: match d.kind {
			Kind::Private => "private",
			Kind::Channel => "channel",
			Kind::Group => "group",
		},
		title: d.title,
		topic_id: d.topic_id,
		gmail_id: d.gmail_id,
		dead: d.dead,
		rules: alerts
			.rules
			.iter()
			.map(|r| (r.as_str(), d.chosen.iter().find(|(c, _)| c == r).map_or(r.on_by_default(), |(_, on)| *on)))
			.collect(),
	};
	AlertsDto {
		enabled: t.is_some(),
		bot: t.and_then(|t| t.name.get()).map(|n| format!("@{n}")),
		rules: alerts.rules.iter().map(|r| r.as_str()).collect(),
		destinations: alerts.destinations.into_iter().map(destination).collect(),
	}
}

async fn alerts(State(panel): State<Panel>, State(t): State<Option<Telegram>>, Extension(caller): Extension<Caller>) -> ApiResult<Json<AlertsDto>> {
	Ok(Json(dto(t.as_ref(), panel.alerts(caller.user_id, &caller.permissions).await?)))
}

fn display(caller: &Caller) -> &str {
	if caller.preferred_name.trim().is_empty() { &caller.email } else { &caller.preferred_name }
}

fn bot(t: Option<Telegram>) -> ApiResult<Telegram> {
	t.ok_or_else(|| ApiError::Unavailable("Telegram is not configured".into()))
}

async fn link(State(panel): State<Panel>, State(t): State<Option<Telegram>>, Extension(caller): Extension<Caller>) -> ApiResult<Response> {
	let t = bot(t)?;
	let Some(name) = t.name.get() else {
		return Err(ApiError::Unavailable("the bot is starting, try again in a minute".into()));
	};
	let token = panel.telegram_link_token(caller.user_id, &caller.permissions, display(&caller), Timestamp::now()).await?;
	tracing::info!(user_id = %caller.user_id, "telegram: link token issued");
	Ok((StatusCode::CREATED, Json(json!({ "url": format!("https://t.me/{name}?start={}", token.as_str()) }))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddChat {
	/// `@channel`, `-100<id>`, or `<group>/<topic>` for a forum topic.
	chat: String,
	gmail_id: Option<i64>,
}

async fn add_chat(State(t): State<Option<Telegram>>, Extension(caller): Extension<Caller>, b: Result<Json<AddChat>, JsonRejection>) -> ApiResult<Response> {
	let t = bot(t)?;
	let b = body(b)?;
	t.hub
		.add_chat(caller.user_id, &caller.permissions, display(&caller), &b.chat, b.gmail_id, Timestamp::now())
		.await??;
	let alerts = t.hub.panel.alerts(caller.user_id, &caller.permissions).await?;
	Ok((StatusCode::CREATED, Json(dto(Some(&t), alerts))).into_response())
}

fn destination_id(raw: &str) -> ApiResult<i64> {
	raw.parse().map_err(|_| ApiError::NotFound)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesBody {
	/// `{"new_lead": true, "review_gone": false, …}`: the rules to change.
	rules: BTreeMap<String, bool>,
}

async fn set_rules(
	State(panel): State<Panel>,
	State(t): State<Option<Telegram>>,
	Extension(caller): Extension<Caller>,
	Path(id): Path<String>,
	b: Result<Json<RulesBody>, JsonRejection>,
) -> ApiResult<Json<AlertsDto>> {
	let id = destination_id(&id)?;
	let rules = body(b)?
		.rules
		.into_iter()
		.map(|(r, on)| Ok((r.parse::<Rule>().map_err(|e| ApiError::BadRequest(e.to_string()))?, on)))
		.collect::<ApiResult<Vec<_>>>()?;
	panel.alerts_set_rules(caller.user_id, &caller.permissions, id, &rules).await??;
	Ok(Json(dto(t.as_ref(), panel.alerts(caller.user_id, &caller.permissions).await?)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GmailBody {
	gmail_id: Option<i64>,
}

async fn set_gmail(
	State(panel): State<Panel>,
	State(t): State<Option<Telegram>>,
	Extension(caller): Extension<Caller>,
	Path(id): Path<String>,
	b: Result<Json<GmailBody>, JsonRejection>,
) -> ApiResult<Json<AlertsDto>> {
	let id = destination_id(&id)?;
	panel.alerts_set_gmail(caller.user_id, id, body(b)?.gmail_id).await??;
	Ok(Json(dto(t.as_ref(), panel.alerts(caller.user_id, &caller.permissions).await?)))
}

async fn test(State(t): State<Option<Telegram>>, Extension(caller): Extension<Caller>, Path(id): Path<String>) -> ApiResult<StatusCode> {
	let id = destination_id(&id)?;
	bot(t)?.hub.test(caller.user_id, id).await??;
	Ok(StatusCode::NO_CONTENT)
}

async fn remove(State(panel): State<Panel>, Extension(caller): Extension<Caller>, Path(id): Path<String>) -> ApiResult<StatusCode> {
	if !panel.alerts_remove(caller.user_id, destination_id(&id)?).await? {
		return Err(Refused::NotFound.into());
	}
	tracing::info!(user_id = %caller.user_id, destination = id, "alerts: a destination removed");
	Ok(StatusCode::NO_CONTENT)
}

// ── the hub's work in `serve` ───────────────────────────────────────────────────────────

/// How often each part of the work runs.
const DELIVER_EVERY: Duration = Duration::from_millis(500);
const FAN_OUT_EVERY: Duration = Duration::from_secs(2);
const RECHECK_EVERY: Duration = Duration::from_secs(60);
/// A service that publishes no feed yet (404) is asked again this often.
const LOOK_AGAIN: Duration = Duration::from_secs(60);

/// Runs the hub until `shutdown` turns true: the bot's poller (on the replica holding its lease),
/// the conductor's own rules, each service's feed, the delivery and the access checks, each on its
/// own schedule. A failing pass is reported and tried again at the next tick; nothing here ends
/// `serve`.
pub async fn run(t: Telegram, shutdown: watch::Receiver<bool>) {
	let hub = &t.hub;
	tokio::join!(
		telegram::poll(hub, &t.name, shutdown.clone()),
		crate::every(FAN_OUT_EVERY, shutdown.clone(), "alerts: the conductor's own", || async {
			hub.fan_out_own(Timestamp::now()).await.map(drop)
		}),
		feed(hub, Origin::Sites, shutdown.clone()),
		feed(hub, Origin::ReviewArchive, shutdown.clone()),
		crate::every(DELIVER_EVERY, shutdown.clone(), "alerts: telegram delivery", || async {
			hub.deliver(Timestamp::now()).await.map(drop)
		}),
		crate::every(RECHECK_EVERY, shutdown, "alerts: access check", || async { hub.recheck_access(Timestamp::now()).await.map(drop) }),
	);
}

/// Follows `service`'s feed: a page every [`FAN_OUT_EVERY`]; while it publishes none, said once
/// and asked again after [`LOOK_AGAIN`].
async fn feed(hub: &Hub<BotApi, Concierge, Services>, service: Origin, mut shutdown: watch::Receiver<bool>) {
	let mut absent = false;
	while !*shutdown.borrow() {
		let wait = match hub.take_feed(service, Timestamp::now()).await {
			Ok(Some(_)) => {
				absent = false;
				FAN_OUT_EVERY
			}
			Ok(None) => {
				if !absent {
					tracing::info!(%service, "alerts: no feed there yet");
					absent = true;
				}
				LOOK_AGAIN
			}
			Err(e) => {
				crate::report(&e, "alerts: a feed");
				FAN_OUT_EVERY
			}
		};
		tokio::select! {
			() = tokio::time::sleep(wait) => {}
			_ = shutdown.changed() => return,
		}
	}
}

impl Directory for Concierge {
	async fn me(&self, access: &str) -> Result<Identity, DirectoryError> {
		use crate::concierge::ConciergeError;
		match Concierge::me(self, access).await {
			Ok(me) => Ok(Identity {
				user_id: me.user_id,
				email: me.email,
				email_verified: me.email_verified,
				preferred_name: me.preferred_name,
				permissions: me.permissions,
			}),
			Err(ConciergeError::Refused(_)) => Err(DirectoryError::Refused),
			Err(ConciergeError::Unavailable(why)) => Err(DirectoryError::Unavailable(why)),
			Err(ConciergeError::Failed(why)) => Err(DirectoryError::Failed(why)),
		}
	}
}

fn service(origin: Origin) -> Service {
	match origin {
		Origin::Sites => Service::Sites,
		Origin::ReviewArchive => Service::ReviewArchive,
		Origin::Conductor => unreachable!("the conductor has no feed of its own"),
	}
}

impl Feeds for Services {
	async fn notes(&self, origin: Origin, after: i64, limit: usize) -> eyre::Result<Option<NotePage>> {
		let path = format!("/api/conductor/notifications?after={after}&limit={limit}");
		let (status, body) = self.conductor_fetch(service(origin), &path).await?;
		match status {
			StatusCode::NOT_FOUND => Ok(None),
			StatusCode::OK => serde_json::from_slice(&body).map(Some).map_err(|e| eyre::eyre!("{origin}'s notes: {e}")),
			other => eyre::bail!("{origin} answered its notes with {other}: {}", String::from_utf8_lossy(&body)),
		}
	}

	async fn received(&self) -> eyre::Result<BTreeMap<String, Timestamp>> {
		self.conductor_get("/api/conductor/sources/received").await
	}

	async fn photo(&self, origin: Origin, path: &str) -> eyre::Result<Vec<u8>> {
		let (status, body) = self.conductor_fetch(service(origin), path).await?;
		eyre::ensure!(status == StatusCode::OK, "{origin} answered {path} with {status}");
		Ok(body.into())
	}

	async fn press(&self, who: &Identity, call: &Call, idempotency_key: &str) -> eyre::Result<u16> {
		let method: Method = call.method.parse().map_err(|_| eyre::eyre!("a button's method {}", call.method))?;
		let mut headers = HeaderMap::new();
		headers.insert(
			HeaderName::from_static("idempotency-key"),
			HeaderValue::from_str(idempotency_key).expect("tg/<outbox>/<button id> is visible ASCII"),
		);
		let body = match &call.body {
			Value::Null => Body::empty(),
			body => {
				headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
				Body::from(serde_json::to_vec(body).expect("a JSON value serializes"))
			}
		};
		let user = User {
			sub: who.user_id.to_string(),
			email: who.email.clone(),
			email_verified: who.email_verified,
			name: who.preferred_name.clone(),
			permissions: who.permissions.iter().filter(|p| p.starts_with(Service::Sites.prefix())).collect(),
		};
		let sent = self.send(Service::Sites, method, &call.path, headers, Some(Principal::User(user)), body);
		let res = tokio::time::timeout(SITES_TIMEOUT, sent)
			.await
			.map_err(|_| eyre::eyre!("sites did not answer a press in time"))??;
		Ok(res.status().as_u16())
	}
}

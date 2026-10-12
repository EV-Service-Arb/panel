//! Signing in and the operator API end to end: the real router and a real SQLite file, with a
//! fake concierge speaking the vendored gRPC contract on a local port.

use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
};

use axum::{
	Router,
	body::{Body, to_bytes},
	http::{HeaderMap, Method, Request, StatusCode, header},
};
use jiff::{SignedDuration, Timestamp};
use panel::{
	session::pkce_challenge,
	testing::{TestDb, admin, event, operator, panel, sign},
};
use panel_contracts::concierge::v1::{
	self as pb,
	auth_service_server::{AuthService, AuthServiceServer},
	user_directory_server::{UserDirectory, UserDirectoryServer},
};
use panel_core::{event::SourceKind, ids::BrandId};
use panel_server::{
	concierge::{CLIENT_ID, Concierge},
	http,
	signin::{SignIn, SignInConfig},
	telegram,
};
use sa_auth::{PermissionSet, SA_ADMIN, SA_OPERATOR};
use serde_json::{Value, json};
use tonic::{Code, Request as GrpcRequest, Response as GrpcResponse, Status, transport::server::TcpIncoming};
use tower::ServiceExt;

const SECRET: &str = "the-panel-client-secret";
const PANEL: &str = "http://panel.test";
const CODE: &str = "code-1";

// ── the fake concierge ───────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Fake {
	/// The challenge the browser was sent to concierge with, as the test read it off the
	/// redirect: what concierge would have stored with the code.
	challenge: Option<String>,
	/// Who the next code is issued to, and how long its access token lives.
	next_user: Option<(String, SignedDuration)>,
	exchanges: u32,
	refreshes: u32,
	/// access token → the profile `GetMe` answers, or the refusal.
	profiles: HashMap<String, Result<pb::UserProfile, Code>>,
	/// refresh token → user id.
	refresh_tokens: HashMap<String, String>,
	/// How long `ExchangeCode` and `GetMe` take to answer.
	exchange_delay: std::time::Duration,
	me_delay: std::time::Duration,
	me_calls: u32,
	/// The next `GetMe` fails with this, once.
	me_fails_once: Option<Code>,
	/// Every catalog published, and whether the next is refused.
	published: Vec<pb::PublishCatalogRequest>,
	refuse_catalog: bool,
}

#[derive(Clone, Default)]
struct FakeConcierge(Arc<Mutex<Fake>>);

impl FakeConcierge {
	fn with<R>(&self, f: impl FnOnce(&mut Fake) -> R) -> R {
		f(&mut self.0.lock().unwrap())
	}
}

fn tokens(fake: &mut Fake, user_id: &str, access_ttl: SignedDuration) -> pb::ClientTokenResponse {
	let now = Timestamp::now();
	let n = fake.exchanges + fake.refreshes;
	let access = format!("access-{user_id}-{n}");
	let refresh = format!("refresh-{user_id}-{n}");
	let profile = fake.profiles.values().find_map(|p| p.as_ref().ok().filter(|p| p.user_id == user_id).cloned());
	if let Some(profile) = profile {
		fake.profiles.insert(access.clone(), Ok(profile));
	}
	fake.refresh_tokens.insert(refresh.clone(), user_id.to_owned());
	pb::ClientTokenResponse {
		access_token: access,
		access_expires_at: (now + access_ttl).as_second(),
		refresh_token: refresh,
		refresh_expires_at: (now + SignedDuration::from_hours(24 * 30)).as_second(),
		user_id: user_id.to_owned(),
	}
}

// The panel calls only the RPCs implemented here; the rest exist because the trait does.
#[tonic::async_trait]
impl AuthService for FakeConcierge {
	async fn exchange_code(&self, req: GrpcRequest<pb::ExchangeCodeRequest>) -> Result<GrpcResponse<pb::ClientTokenResponse>, Status> {
		let r = req.into_inner();
		tokio::time::sleep(self.with(|f| f.exchange_delay)).await;
		self.with(|f| {
			f.exchanges += 1;
			if r.client_id != CLIENT_ID || r.client_secret != SECRET {
				return Err(Status::unauthenticated("client"));
			}
			let pkce_ok = f.challenge.as_deref() == Some(pkce_challenge(&r.code_verifier).as_str());
			if r.code != CODE || r.redirect_uri != format!("{PANEL}/auth/callback") || !pkce_ok {
				return Err(Status::unauthenticated("code"));
			}
			let (user, ttl) = f.next_user.clone().ok_or_else(|| Status::permission_denied("policy"))?;
			Ok(GrpcResponse::new(tokens(f, &user, ttl)))
		})
	}

	async fn refresh_client_token(&self, req: GrpcRequest<pb::RefreshClientTokenRequest>) -> Result<GrpcResponse<pb::ClientTokenResponse>, Status> {
		let r = req.into_inner();
		self.with(|f| {
			f.refreshes += 1;
			if r.client_secret != SECRET {
				return Err(Status::unauthenticated("client"));
			}
			let user = f.refresh_tokens.remove(&r.refresh_token).ok_or_else(|| Status::unauthenticated("refresh"))?;
			Ok(GrpcResponse::new(tokens(f, &user, SignedDuration::from_mins(15))))
		})
	}

	async fn publish_catalog(&self, req: GrpcRequest<pb::PublishCatalogRequest>) -> Result<GrpcResponse<pb::PublishCatalogResponse>, Status> {
		let r = req.into_inner();
		self.with(|f| {
			if r.client_id != CLIENT_ID || r.client_secret != SECRET {
				return Err(Status::unauthenticated("client"));
			}
			if f.refuse_catalog {
				return Err(Status::failed_precondition("older than the stored catalog"));
			}
			f.published.push(r);
			Ok(GrpcResponse::new(pb::PublishCatalogResponse {}))
		})
	}

	async fn exchange(&self, _: GrpcRequest<pb::ExchangeRequest>) -> Result<GrpcResponse<pb::TokenResponse>, Status> {
		Err(Status::unimplemented("exchange"))
	}

	async fn refresh(&self, _: GrpcRequest<pb::RefreshRequest>) -> Result<GrpcResponse<pb::TokenResponse>, Status> {
		Err(Status::unimplemented("refresh"))
	}

	async fn logout(&self, _: GrpcRequest<pb::LogoutRequest>) -> Result<GrpcResponse<pb::LogoutResponse>, Status> {
		Err(Status::unimplemented("logout"))
	}

	async fn list_sessions(&self, _: GrpcRequest<pb::ListSessionsRequest>) -> Result<GrpcResponse<pb::ListSessionsResponse>, Status> {
		Err(Status::unimplemented("list_sessions"))
	}

	async fn revoke_session(&self, _: GrpcRequest<pb::RevokeSessionRequest>) -> Result<GrpcResponse<pb::RevokeSessionResponse>, Status> {
		Err(Status::unimplemented("revoke_session"))
	}

	async fn jwks(&self, _: GrpcRequest<pb::JwksRequest>) -> Result<GrpcResponse<pb::JwksResponse>, Status> {
		Err(Status::unimplemented("jwks"))
	}
}

#[tonic::async_trait]
impl UserDirectory for FakeConcierge {
	async fn get_me(&self, req: GrpcRequest<pb::GetMeRequest>) -> Result<GrpcResponse<pb::UserProfile>, Status> {
		let bearer = req
			.metadata()
			.get("authorization")
			.and_then(|v| v.to_str().ok())
			.and_then(|v| v.strip_prefix("Bearer "))
			.map(str::to_owned);
		tokio::time::sleep(self.with(|f| f.me_delay)).await;
		if let Some(code) = self.with(|f| {
			f.me_calls += 1;
			f.me_fails_once.take()
		}) {
			return Err(Status::new(code, "once"));
		}
		let answer = self.with(|f| bearer.and_then(|b| f.profiles.get(&b).cloned()));
		match answer {
			Some(Ok(p)) => Ok(GrpcResponse::new(p)),
			Some(Err(code)) => Err(Status::new(code, "refused")),
			None => Err(Status::unauthenticated("token")),
		}
	}

	async fn update_profile(&self, _: GrpcRequest<pb::UpdateProfileRequest>) -> Result<GrpcResponse<pb::UserProfile>, Status> {
		Err(Status::unimplemented("update_profile"))
	}

	async fn revoke_tokens(&self, _: GrpcRequest<pb::RevokeTokensRequest>) -> Result<GrpcResponse<pb::RevokeTokensResponse>, Status> {
		Err(Status::unimplemented("revoke_tokens"))
	}

	async fn disable_user(&self, _: GrpcRequest<pb::DisableUserRequest>) -> Result<GrpcResponse<pb::DisableUserResponse>, Status> {
		Err(Status::unimplemented("disable_user"))
	}

	async fn hold_user(&self, _: GrpcRequest<pb::HoldUserRequest>) -> Result<GrpcResponse<pb::HoldUserResponse>, Status> {
		Err(Status::unimplemented("hold_user"))
	}

	async fn reinstate_user(&self, _: GrpcRequest<pb::ReinstateUserRequest>) -> Result<GrpcResponse<pb::ReinstateUserResponse>, Status> {
		Err(Status::unimplemented("reinstate_user"))
	}

	async fn set_kyc_level(&self, _: GrpcRequest<pb::SetKycLevelRequest>) -> Result<GrpcResponse<pb::SetKycLevelResponse>, Status> {
		Err(Status::unimplemented("set_kyc_level"))
	}

	async fn list_users(&self, _: GrpcRequest<pb::ListUsersRequest>) -> Result<GrpcResponse<pb::ListUsersResponse>, Status> {
		Err(Status::unimplemented("list_users"))
	}

	async fn get_user(&self, _: GrpcRequest<pb::GetUserRequest>) -> Result<GrpcResponse<pb::UserProfile>, Status> {
		Err(Status::unimplemented("get_user"))
	}

	async fn set_role(&self, _: GrpcRequest<pb::SetRoleRequest>) -> Result<GrpcResponse<pb::SetRoleResponse>, Status> {
		Err(Status::unimplemented("set_role"))
	}

	async fn grant_permission(&self, _: GrpcRequest<pb::GrantPermissionRequest>) -> Result<GrpcResponse<pb::GrantPermissionResponse>, Status> {
		Err(Status::unimplemented("grant_permission"))
	}

	async fn revoke_permission(&self, _: GrpcRequest<pb::RevokePermissionRequest>) -> Result<GrpcResponse<pb::RevokePermissionResponse>, Status> {
		Err(Status::unimplemented("revoke_permission"))
	}

	async fn list_grants(&self, _: GrpcRequest<pb::ListGrantsRequest>) -> Result<GrpcResponse<pb::ListGrantsResponse>, Status> {
		Err(Status::unimplemented("list_grants"))
	}
}

/// Serves the fake on a free port for the life of the test; its address.
async fn serve_fake(fake: FakeConcierge) -> String {
	let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
	let addr = incoming.local_addr().unwrap();
	let server = tonic::transport::Server::builder()
		.add_service(AuthServiceServer::new(fake.clone()))
		.add_service(UserDirectoryServer::new(fake))
		.serve_with_incoming(incoming);
	// Dropped with the test's runtime; a failure shows up as UNAVAILABLE in the assertions.
	let _server = tokio::spawn(server);
	format!("http://{addr}")
}

/// `who@example.com`, holding `permissions` in `sa`.
fn profile(user_id: &str, who: &str, permissions: &[&str]) -> pb::UserProfile {
	pb::UserProfile {
		user_id: user_id.to_owned(),
		email: format!("{who}@example.com"),
		email_verified: true,
		preferred_name: "Ann".to_owned(),
		permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
		..Default::default()
	}
}

// ── a browser ────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Browser {
	jar: HashMap<String, String>,
}

struct Answer {
	status: StatusCode,
	headers: HeaderMap,
	body: Value,
}

impl Browser {
	async fn send(&mut self, app: &Router, method: Method, uri: &str, body: Option<Value>, csrf: bool) -> Answer {
		let mut req = Request::builder().method(method).uri(uri);
		let cookie: Vec<String> = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
		if !cookie.is_empty() {
			req = req.header(header::COOKIE, cookie.join("; "));
		}
		if csrf && let Some(t) = self.jar.get("sa_csrf") {
			req = req.header("x-sa-csrf", t);
		}
		let req = match body {
			Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
			None => req.body(Body::empty()),
		};
		let res = app.clone().oneshot(req.unwrap()).await.unwrap();
		for c in res.headers().get_all(header::SET_COOKIE) {
			let c = c.to_str().unwrap();
			let (pair, attrs) = c.split_once(';').unwrap_or((c, ""));
			let (k, v) = pair.split_once('=').unwrap();
			if attrs.contains("Max-Age=0") {
				self.jar.remove(k);
			} else {
				self.jar.insert(k.to_owned(), v.to_owned());
			}
		}
		let status = res.status();
		let headers = res.headers().clone();
		let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
		Answer {
			status,
			headers,
			body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
		}
	}

	async fn get(&mut self, app: &Router, uri: &str) -> Answer {
		self.send(app, Method::GET, uri, None, false).await
	}

	async fn post(&mut self, app: &Router, uri: &str, body: Value) -> Answer {
		self.send(app, Method::POST, uri, Some(body), true).await
	}

	/// `/auth/login` → (concierge) → `/auth/callback`; the callback's answer.
	async fn sign_in(&mut self, app: &Router, fake: &FakeConcierge, state_override: Option<&str>) -> Answer {
		self.sign_in_from(app, fake, "/auth/login", state_override).await
	}

	/// [`Self::sign_in`], starting at `login` (`/auth/login?…`).
	async fn sign_in_from(&mut self, app: &Router, fake: &FakeConcierge, login: &str, state_override: Option<&str>) -> Answer {
		let login = self.get(app, login).await;
		assert_eq!(login.status, StatusCode::FOUND);
		let location = login.headers[header::LOCATION].to_str().unwrap().to_owned();
		assert!(location.starts_with("http://concierge.test/api/auth/authorize?client_id=sa&"), "{location}");
		assert!(location.contains("redirect_uri=http%3A%2F%2Fpanel.test%2Fauth%2Fcallback"), "{location}");
		let param = |name: &str| location.split(['?', '&']).find_map(|p| p.strip_prefix(&format!("{name}="))).unwrap().to_owned();
		fake.with(|f| f.challenge = Some(param("code_challenge")));
		let state = state_override.map_or_else(|| param("state"), str::to_owned);
		self.get(app, &format!("/auth/callback?code={CODE}&state={state}")).await
	}
}

async fn setup(db: &TestDb) -> (Router, FakeConcierge, panel::Panel) {
	setup_with(db, http::Limits::default()).await
}

async fn setup_with(db: &TestDb, limits: http::Limits) -> (Router, FakeConcierge, panel::Panel) {
	let fake = FakeConcierge::default();
	let addr = serve_fake(fake.clone()).await;
	let panel = panel(db).await;
	let concierge = Concierge::new(&addr, SECRET).unwrap();
	let app = http::app_with(
		SignIn::new(
			panel.clone(),
			concierge,
			SignInConfig {
				panel_origin: format!("{PANEL}/"),
				concierge_origin: "http://concierge.test".to_owned(),
			},
		),
		limits,
	);
	(app, fake, panel)
}

const OPERATOR: &str = "0190a7c4-0000-7000-8000-000000000001";
const OUTSIDER: &str = "0190a7c4-0000-7000-8000-000000000002";
const ADMIN: &str = "0190a7c4-0000-7000-8000-000000000003";

// ── the tests ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_operator_signs_in_works_leads_and_signs_out() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15)));
	});
	let mut b = Browser::default();

	assert_eq!(b.get(&app, "/api/v1/me").await.status, StatusCode::UNAUTHORIZED, "no session yet");
	let cb = b.sign_in(&app, &fake, None).await;
	assert_eq!(cb.status, StatusCode::SEE_OTHER);
	assert_eq!(cb.headers[header::LOCATION], "/");
	assert_eq!(cb.headers[header::CACHE_CONTROL], "no-store");
	assert!(b.jar.contains_key("sa_session") && b.jar.contains_key("sa_csrf"));
	assert!(!b.jar.contains_key("sa_prelogin"), "the pre-login is single-use");

	let me = b.get(&app, "/api/v1/me").await;
	assert_eq!(me.status, StatusCode::OK, "{}", me.body);
	assert_eq!(
		me.body,
		json!({"user_id": OPERATOR, "email": "investor@example.com", "preferred_name": "Ann", "permissions": operator(), "dev_sign_in": false,
			"account_center": "http://concierge.test/cabinet/settings"})
	);
	assert_eq!(me.headers[header::CACHE_CONTROL], "no-store");

	let new_lead = json!({"brand": "aquafix", "location": "paris-11", "need": "a leaking tap", "phone": "+33 6 00 00 00 00"});
	let no_csrf = b.send(&app, Method::POST, "/api/v1/leads", Some(new_lead.clone()), false).await;
	assert_eq!(no_csrf.status, StatusCode::FORBIDDEN, "a write without the CSRF header");
	let created = b.post(&app, "/api/v1/leads", new_lead).await;
	assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
	let lead_id = created.body["lead_id"].as_str().unwrap().to_owned();
	assert!(lead_id.starts_with("p-"), "{lead_id}");

	let list = b.get(&app, "/api/v1/leads?stage=created").await;
	assert_eq!(list.status, StatusCode::OK, "{}", list.body);
	let row = &list.body["leads"][0];
	assert_eq!(row["lead_id"], lead_id.as_str());
	assert_eq!(row["manual"], true);
	assert_eq!(row["sla"]["overdue"], false);
	assert_eq!(row["pii"]["phone"], "+33 6 00 00 00 00", "an operator sees PII: {row}");

	let attempt = b.post(&app, &format!("/api/v1/leads/aquafix/{lead_id}/calls/attempt"), json!(null)).await;
	assert_eq!(attempt.status, StatusCode::CREATED, "{}", attempt.body);
	let attempt_id = attempt.body["attempt_id"].as_str().unwrap();
	let outcome = b
		.post(&app, &format!("/api/v1/leads/aquafix/{lead_id}/calls/{attempt_id}/outcome"), json!({"outcome": "answered"}))
		.await;
	assert_eq!(outcome.status, StatusCode::CREATED, "{}", outcome.body);
	let contacted = b
		.post(&app, &format!("/api/v1/leads/aquafix/{lead_id}/stage"), json!({"stage": "contacted", "channel": "phone"}))
		.await;
	assert_eq!(contacted.status, StatusCode::CREATED, "{}", contacted.body);
	let card = b.get(&app, &format!("/api/v1/leads/aquafix/{lead_id}")).await;
	assert_eq!(card.status, StatusCode::OK, "{}", card.body);
	assert_eq!(card.body["lead"]["stage"], "contacted", "{}", card.body);
	let types: Vec<&str> = card.body["events"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap()).collect();
	assert_eq!(types, ["lead.created", "call.attempted", "call.logged", "lead.contacted"]);

	let bad = b.post(&app, &format!("/api/v1/leads/aquafix/{lead_id}/stage"), json!({"stage": "teleported"})).await;
	assert_eq!(bad.status, StatusCode::BAD_REQUEST);
	assert!(bad.body["error"].is_string());
	assert_eq!(b.get(&app, "/api/v1/leads/aquafix/p-nope").await.status, StatusCode::NOT_FOUND);
	assert_eq!(b.get(&app, "/api/v1/sources").await.status, StatusCode::FORBIDDEN, "sources are an admin's");
	let funnel = b.get(&app, "/api/v1/funnel").await;
	assert_eq!(funnel.status, StatusCode::OK, "{}", funnel.body);
	assert_eq!(funnel.body["stages"][0]["reached"], 1);
	assert_eq!(funnel.body["stages"][0]["of_leads"]["percent"], Value::Null, "1 of 1 is too few for a percentage");

	let no_csrf = b.send(&app, Method::POST, "/auth/logout", None, false).await;
	assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
	let session = b.jar["sa_session"].clone();
	assert_eq!(b.send(&app, Method::POST, "/auth/logout", None, true).await.status, StatusCode::NO_CONTENT);
	assert!(b.jar.is_empty(), "{:?}", b.jar);
	b.jar.insert("sa_session".into(), session);
	assert_eq!(b.get(&app, "/api/v1/me").await.status, StatusCode::UNAUTHORIZED, "the session is gone server-side");
	assert_eq!(fake.with(|f| f.exchanges), 1);
}

#[tokio::test]
async fn the_screens_read_places_counts_slices_and_payments() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15)));
	});
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let mut ids = Vec::new();
	for location in ["paris-11", "paris-11", "lyon-2"] {
		let created = b.post(&app, "/api/v1/leads", json!({"brand": "aquafix", "location": location, "need": "a tap"})).await;
		assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
		ids.push(created.body["lead_id"].as_str().unwrap().to_owned());
	}
	for (billed, commission, currency) in [(12_000, 1_800, "EUR"), (3_000, 450, "EUR"), (500, 50, "GBP")] {
		let paid = b
			.post(
				&app,
				&format!("/api/v1/leads/aquafix/{}/payments", ids[0]),
				json!({"billed": billed, "commission": commission, "currency": currency}),
			)
			.await;
		assert_eq!(paid.status, StatusCode::CREATED, "{}", paid.body);
	}
	let list = b.get(&app, "/api/v1/leads").await;
	let created_at: Timestamp = list.body["leads"][2]["created_at"].as_str().unwrap().parse().unwrap();
	let today = created_at.to_zoned(jiff::tz::TimeZone::UTC).date();
	let tomorrow = today.tomorrow().unwrap();

	let places = b.get(&app, "/api/v1/places").await;
	assert_eq!(places.status, StatusCode::OK, "{}", places.body);
	let got: Vec<(&str, &str)> = places.body["places"]
		.as_array()
		.unwrap()
		.iter()
		.map(|p| (p["brand"].as_str().unwrap(), p["location"].as_str().unwrap()))
		.collect();
	assert_eq!(got, [("aquafix", "lyon-2"), ("aquafix", "paris-11")]);
	assert!(places.body["places"][0]["last_lead_at"].is_string());

	let counts = b.get(&app, "/api/v1/leads/counts?brand=aquafix&location=paris-11").await;
	assert_eq!(counts.status, StatusCode::OK, "{}", counts.body);
	assert_eq!(counts.body["stages"]["created"], 1);
	assert_eq!(counts.body["stages"]["paid"], 1);
	assert_eq!(counts.body["stages"]["lost"], 0, "every stage, zero included");
	assert_eq!((counts.body["total"].clone(), counts.body["overdue"].clone()), (json!(2), json!(0)));
	assert_eq!(b.get(&app, "/api/v1/leads/counts?stage=paid").await.status, StatusCode::BAD_REQUEST);

	let whole = b.get(&app, "/api/v1/funnel").await;
	assert_eq!(whole.status, StatusCode::OK, "{}", whole.body);
	assert_eq!(whole.body["stages"][0]["reached"], 3);
	assert_eq!(
		whole.body["payments"],
		json!([
			{"currency": "EUR", "billed": 15_000, "commission": 2_250, "count": 2},
			{"currency": "GBP", "billed": 500, "commission": 50, "count": 1}
		]),
		"summed per currency, never converted"
	);
	assert!(whole.body.get("locations").is_none());

	let sliced = b.get(&app, &format!("/api/v1/funnel?by=location&from={today}&to={today}&brand=aquafix")).await;
	assert_eq!(sliced.status, StatusCode::OK, "{}", sliced.body);
	assert_eq!(sliced.body["by"], "location");
	let rows = sliced.body["locations"].as_array().unwrap();
	assert_eq!(rows.len(), 2);
	assert_eq!((rows[0]["location"].as_str(), rows[1]["location"].as_str()), (Some("lyon-2"), Some("paris-11")));
	assert_eq!(rows[1]["stages"][0]["reached"], 2);
	assert_eq!(rows[1]["stages"][5]["of_leads"], json!({"n": 1, "of": 2, "percent": null, "small_sample": true}));
	assert_eq!(rows[1]["payments"][0]["billed"], 15_000);
	assert_eq!(rows[0]["payments"], json!([]));
	assert_eq!(sliced.body["min_sample"], 30);
	assert_eq!(b.get(&app, "/api/v1/funnel?by=brand").await.status, StatusCode::BAD_REQUEST);

	let window = |from: &str, to: &str| format!("/api/v1/leads?created_from={from}&created_to={to}");
	assert_eq!(b.get(&app, &window(&today.to_string(), &today.to_string())).await.body["leads"].as_array().unwrap().len(), 3);
	assert_eq!(b.get(&app, &window(&tomorrow.to_string(), &tomorrow.to_string())).await.body["leads"], json!([]));
	let backwards = b.get(&app, &window(&tomorrow.to_string(), &today.to_string())).await;
	assert_eq!(backwards.status, StatusCode::BAD_REQUEST);
	assert_eq!(backwards.body["error"], "created_from is after created_to");
	assert_eq!(b.get(&app, "/api/v1/leads?created_from=yesterday").await.status, StatusCode::BAD_REQUEST);
}

/// A lead the landing's antispam doubted comes back marked, in the list and on its card, and
/// the list is filtered on the mark.
#[tokio::test]
async fn suspect_leads_are_marked_and_filtered() {
	let db = TestDb::create().await;
	let (app, fake, panel) = setup(&db).await;
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15)));
	});
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;

	let secret = panel
		.add_source("aquafix-site", SourceKind::Site, [BrandId::parse("aquafix").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let now = Timestamp::now();
	let created = |lead: &str, minutes: i64, properties: Value| {
		event(
			"lead.created",
			now - SignedDuration::from_mins(minutes),
			"site",
			json!({"brandId": "aquafix", "locationId": "paris-11", "leadId": lead}),
			properties,
		)
	};
	let events = [
		created("L-1", 3, json!({"channel": "form"})),
		created("L-2", 2, json!({"channel": "form", "suspect": "rate_limited"})),
		created("L-3", 1, json!({"channel": "callback", "suspect": "too_fast"})),
	];
	let got = panel.ingest(sign("aquafix-site", &secret, &events, now).batch(), now).await.unwrap();
	assert!(got.iter().all(|v| v.outcome == panel::Outcome::Accepted { unregistered: false }), "{got:?}");

	async fn listed(b: &mut Browser, app: &Router, query: &str) -> Vec<(String, Value)> {
		let r = b.get(app, &format!("/api/v1/leads{query}")).await;
		assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.body);
		r.body["leads"]
			.as_array()
			.unwrap()
			.iter()
			.map(|l| (l["lead_id"].as_str().unwrap().to_owned(), l["suspect"].clone()))
			.collect()
	}
	let all = listed(&mut b, &app, "").await;
	assert_eq!(
		all,
		[("L-3".to_owned(), json!("too_fast")), ("L-2".to_owned(), json!("rate_limited")), ("L-1".to_owned(), Value::Null)],
		"every lead by default, the field always present"
	);
	assert_eq!(listed(&mut b, &app, "?suspect=only").await, all[..2]);
	assert_eq!(listed(&mut b, &app, "?suspect=exclude").await, all[2..]);
	assert_eq!(listed(&mut b, &app, "?suspect=only&stage=created").await.len(), 2, "with the other filters");
	let bad = b.get(&app, "/api/v1/leads?suspect=maybe").await;
	assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);

	let card = b.get(&app, "/api/v1/leads/aquafix/L-2").await;
	assert_eq!(card.status, StatusCode::OK, "{}", card.body);
	assert_eq!(card.body["lead"]["suspect"], "rate_limited");
	let card = b.get(&app, "/api/v1/leads/aquafix/L-1").await;
	assert_eq!(card.body["lead"]["suspect"], Value::Null);
}

/// Messenger leads through the operator API: one taken by hand from a WhatsApp conversation,
/// one from the landing with its ref; the list filtered by channel and found by ref (in any
/// case); an operator says a customer wrote, and the card shows when and on which.
#[tokio::test]
async fn messenger_leads_over_the_operator_api() {
	let db = TestDb::create().await;
	let (app, fake, panel) = setup(&db).await;
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15)));
	});
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let secret = panel
		.add_source("aquafix-site", SourceKind::Site, [BrandId::parse("aquafix").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let now = Timestamp::now();
	let landing = [panel::testing::messenger_lead(now - SignedDuration::from_mins(2), "aquafix", "L-1", "telegram", "AQ-7K3F")];
	panel.ingest(sign("aquafix-site", &secret, &landing, now).batch(), now).await.unwrap();

	let by_hand = json!({"brand": "aquafix", "location": "royat", "need": "a leak", "channel": "whatsapp"});
	let created = b.post(&app, "/api/v1/leads", by_hand).await;
	assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
	let wa = created.body["lead_id"].as_str().unwrap().to_owned();
	for (channel, why) in [("form", "the landing's"), ("sms", "not a channel")] {
		let r = b
			.post(&app, "/api/v1/leads", json!({"brand": "aquafix", "location": "royat", "need": "x", "channel": channel}))
			.await;
		assert_eq!(
			(r.status, r.body),
			(StatusCode::BAD_REQUEST, json!({"error": "channel is one of phone_inbound, whatsapp, telegram"})),
			"{why}"
		);
	}

	let ids = |r: &Value| r["leads"].as_array().unwrap().iter().map(|l| l["lead_id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
	let r = b.get(&app, "/api/v1/leads?channel=whatsapp").await;
	assert_eq!((r.status, ids(&r.body)), (StatusCode::OK, vec![wa.clone()]), "{}", r.body);
	let r = b.get(&app, "/api/v1/leads?message_ref=aq-7k3f").await;
	assert_eq!(ids(&r.body), ["L-1"], "{}", r.body);
	let row = &r.body["leads"][0];
	assert_eq!((&row["channel"], &row["message_ref"], &row["messaged_at"]), (&json!("telegram"), &json!("AQ-7K3F"), &Value::Null));
	assert_eq!(b.get(&app, "/api/v1/leads?channel=pigeon").await.status, StatusCode::BAD_REQUEST);
	let pasted = b.get(&app, "/api/v1/leads?message_ref=R%C3%A9f.%20aq%207k3f").await;
	assert_eq!(ids(&pasted.body), ["L-1"], "pasted with its label: {}", pasted.body);
	assert_eq!(b.get(&app, "/api/v1/leads?message_ref=AQ-7K3U").await.status, StatusCode::BAD_REQUEST);

	let wrote = b.post(&app, "/api/v1/leads/aquafix/L-1/messaged", json!({"channel": "telegram"})).await;
	assert_eq!(wrote.status, StatusCode::CREATED, "{}", wrote.body);
	let bad = b.post(&app, "/api/v1/leads/aquafix/L-1/messaged", json!({"channel": "phone"})).await;
	assert_eq!((bad.status, bad.body), (StatusCode::BAD_REQUEST, json!({"error": "channel is one of whatsapp, telegram"})));
	assert_eq!(
		b.post(&app, "/api/v1/leads/aquafix/L-404/messaged", json!({"channel": "whatsapp"})).await.status,
		StatusCode::NOT_FOUND
	);
	let card = b.get(&app, "/api/v1/leads/aquafix/L-1").await;
	assert_eq!(card.body["lead"]["messaged_channel"], "telegram");
	assert!(card.body["lead"]["messaged_at"].is_string(), "{}", card.body);
	assert_eq!(card.body["lead"]["stage"], "created");
	let types: Vec<&str> = card.body["events"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap()).collect();
	assert_eq!(types, ["lead.created", "lead.messaged"]);
}

/// A lead's flow and price, as the landings send them, on the list and the card, and the
/// list filtered by flow.
#[tokio::test]
async fn leads_carry_their_flow_and_price() {
	let db = TestDb::create().await;
	let (app, fake, panel) = setup(&db).await;
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15)));
	});
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let secret = panel
		.add_source("aquafix-site", SourceKind::Site, [BrandId::parse("aquafix").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let now = Timestamp::now();
	let created = |lead: &str, minutes: i64, properties: Value| {
		event(
			"lead.created",
			now - SignedDuration::from_mins(minutes),
			"site",
			json!({"brandId": "aquafix", "locationId": "royat", "leadId": lead}),
			properties,
		)
	};
	let events = [
		created("L-1", 4, json!({"channel": "form"})),
		created("L-2", 3, json!({"channel": "form", "flow": "quote"})),
		created(
			"L-3",
			2,
			json!({"channel": "form", "flow": "estimate", "quotedCents": 12900, "pricingValidFrom": "2026-10-01", "estimateInputs": {"zone": "a", "bedrooms": "2"}}),
		),
		created("L-4", 1, json!({"channel": "callback", "flow": "fixed", "quotedCents": 8000, "pricingValidFrom": "2026-09-15"})),
	];
	let got = panel.ingest(sign("aquafix-site", &secret, &events, now).batch(), now).await.unwrap();
	assert!(got.iter().all(|v| v.outcome == panel::Outcome::Accepted { unregistered: false }), "{got:?}");

	let fields = |l: &Value| json!({"flow": l["flow"], "quoted_cents": l["quoted_cents"], "pricing_valid_from": l["pricing_valid_from"], "estimate_inputs": l["estimate_inputs"]});
	let all = b.get(&app, "/api/v1/leads").await;
	assert_eq!(all.status, StatusCode::OK, "{}", all.body);
	let leads = all.body["leads"].as_array().unwrap();
	assert_eq!(
		leads.iter().map(fields).collect::<Vec<_>>(),
		[
			json!({"flow": "fixed", "quoted_cents": 8000, "pricing_valid_from": "2026-09-15", "estimate_inputs": null}),
			json!({"flow": "estimate", "quoted_cents": 12900, "pricing_valid_from": "2026-10-01", "estimate_inputs": {"bedrooms": "2", "zone": "a"}}),
			json!({"flow": "quote", "quoted_cents": null, "pricing_valid_from": null, "estimate_inputs": null}),
			json!({"flow": null, "quoted_cents": null, "pricing_valid_from": null, "estimate_inputs": null}),
		],
		"the fields always present, null when the lead said nothing"
	);
	for (flow, want) in [("estimate", vec!["L-3"]), ("fixed", vec!["L-4"]), ("quote", vec!["L-2"])] {
		let r = b.get(&app, &format!("/api/v1/leads?flow={flow}")).await;
		assert_eq!(r.status, StatusCode::OK, "{}", r.body);
		let ids: Vec<&str> = r.body["leads"].as_array().unwrap().iter().map(|l| l["lead_id"].as_str().unwrap()).collect();
		assert_eq!(ids, want, "{flow}");
	}
	let bad = b.get(&app, "/api/v1/leads?flow=subscription").await;
	assert_eq!((bad.status, bad.body), (StatusCode::BAD_REQUEST, json!({"error": "flow is not one of quote, estimate, fixed"})));

	let card = b.get(&app, "/api/v1/leads/aquafix/L-3").await;
	assert_eq!(card.status, StatusCode::OK, "{}", card.body);
	assert_eq!(fields(&card.body["lead"])["quoted_cents"], 12900);
	assert_eq!(card.body["lead"]["estimate_inputs"], json!({"bedrooms": "2", "zone": "a"}));
}

#[tokio::test]
async fn the_retired_counts_are_still_taken_and_shown_nowhere() {
	let db = TestDb::create().await;
	let (app, fake, panel) = setup(&db).await;
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15)));
	});
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;

	// What the retired import journaled up to v0.3, sent through a key of kind posthog: still
	// registered, so a journal holding them rebuilds, and nothing on any screen.
	let secret = panel
		.add_source("posthog-test", SourceKind::Posthog, [BrandId::parse("aquafix").unwrap()].into())
		.await
		.unwrap()
		.unwrap()
		.secret
		.to_string();
	let now = Timestamp::now();
	let day = now.to_zoned(jiff::tz::TimeZone::UTC).date();
	let at = day.to_zoned(jiff::tz::TimeZone::UTC).unwrap().timestamp();
	let events = [
		event(
			"site.metrics",
			at,
			"posthog",
			json!({"brandId": "aquafix", "locationId": "lyon-2"}),
			json!({"day": day.to_string(), "source": "gbp", "visits": 30, "revision": 1}),
		),
		event(
			"experiment.metrics",
			at,
			"posthog",
			json!({"brandId": "aquafix"}),
			json!({"day": day.to_string(), "experiment": "hero", "variant": "a", "exposures": 40, "revision": 1}),
		),
	];
	let got = panel.ingest(sign("posthog-test", &secret, &events, now).batch(), now).await.unwrap();
	assert!(got.iter().all(|v| v.outcome == panel::Outcome::Accepted { unregistered: false }), "{got:?}");
	let rebuilt = panel.rebuild_projections().await.unwrap();
	assert_eq!((rebuilt.registered, rebuilt.invalid), (2, 0));

	let funnel = b.get(&app, "/api/v1/funnel").await;
	assert_eq!(funnel.status, StatusCode::OK, "{}", funnel.body);
	assert!(funnel.body.get("aggregate").is_none() && funnel.body.get("aggregate_source").is_none(), "{}", funnel.body);
	let sliced = b.get(&app, "/api/v1/funnel?by=location").await;
	assert_eq!(sliced.body["locations"], json!([]), "visits make no row any more");
	assert_eq!(b.get(&app, "/api/v1/places").await.body["places"], json!([]), "nor a place");
}

#[tokio::test]
async fn a_forged_callback_never_presents_the_code() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	fake.with(|f| f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15))));

	let mut b = Browser::default();
	let cb = b.sign_in(&app, &fake, Some("0000")).await;
	assert_eq!(cb.status, StatusCode::BAD_REQUEST);
	assert!(!b.jar.contains_key("sa_session"));

	let mut stranger = Browser::default();
	let cb = stranger.get(&app, &format!("/auth/callback?code={CODE}&state=abc")).await;
	assert_eq!(cb.status, StatusCode::BAD_REQUEST, "no pre-login cookie at all");
	assert_eq!(fake.with(|f| f.exchanges), 0);

	let cb = stranger.get(&app, "/auth/callback?error=access_denied").await;
	assert_eq!(cb.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn every_account_signs_in_and_its_permissions_open_the_sections() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;

	// An account holding nothing here signs in, and is shown nothing.
	fake.with(|f| {
		f.profiles.insert("seed-outsider".into(), Ok(profile(OUTSIDER, "investor", &[])));
		f.next_user = Some((OUTSIDER.into(), SignedDuration::from_mins(15)));
	});
	let mut outsider = Browser::default();
	assert_eq!(outsider.sign_in(&app, &fake, None).await.status, StatusCode::SEE_OTHER);
	let me = outsider.get(&app, "/api/v1/me").await;
	assert_eq!((me.status, &me.body["permissions"]), (StatusCode::OK, &json!([])), "{}", me.body);
	assert_eq!(outsider.get(&app, "/api/v1/leads").await.status, StatusCode::FORBIDDEN);

	fake.with(|f| {
		f.profiles.insert("seed-admin".into(), Ok(profile(ADMIN, "admin", SA_ADMIN.members)));
		f.next_user = Some((ADMIN.into(), SignedDuration::from_mins(15)));
	});
	let mut admin = Browser::default();
	admin.sign_in(&app, &fake, None).await;
	assert_eq!(
		admin.get(&app, "/api/v1/me").await.body["permissions"],
		json!(SA_ADMIN.members.iter().copied().collect::<PermissionSet>())
	);
	let added = admin.post(&app, "/api/v1/sources", json!({"key_id": "vifnet-site", "kind": "site", "brands": ["vifnet"]})).await;
	assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
	assert_eq!(added.body["secret"].as_str().unwrap().len(), 64);
	let listed = admin.get(&app, "/api/v1/sources").await;
	assert_eq!(listed.body["sources"][0]["key_id"], "vifnet-site");
	assert!(listed.body["sources"][0].get("secret").is_none(), "a secret is shown once");
	let again = admin.post(&app, "/api/v1/sources", json!({"key_id": "vifnet-site", "kind": "site", "brands": ["vifnet"]})).await;
	assert_eq!(again.status, StatusCode::CONFLICT);
	assert_eq!(admin.send(&app, Method::DELETE, "/api/v1/sources/vifnet-site", None, true).await.status, StatusCode::NO_CONTENT);

	// concierge revokes the operator (signed out of evinvest.ltd): GetMe refuses, and the
	// session is closed here too.
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15)));
	});
	let mut op = Browser::default();
	op.sign_in(&app, &fake, None).await;
	fake.with(|f| {
		for p in f.profiles.values_mut() {
			if p.as_ref().is_ok_and(|p| p.user_id == OPERATOR) {
				*p = Err(Code::Unauthenticated);
			}
		}
	});
	let me = op.get(&app, "/api/v1/me").await;
	assert_eq!(me.status, StatusCode::UNAUTHORIZED, "{}", me.body);
	assert!(!op.jar.contains_key("sa_session"), "the cookie is cleared");
}

#[tokio::test]
async fn a_stale_access_token_is_rotated_once() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	fake.with(|f| {
		f.profiles.insert("seed-operator".into(), Ok(profile(OPERATOR, "investor", SA_OPERATOR.members)));
		// Inside the refresh margin from the start.
		f.next_user = Some((OPERATOR.into(), SignedDuration::from_secs(5)));
	});
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let me = b.get(&app, "/api/v1/me").await;
	assert_eq!(me.status, StatusCode::OK, "{}", me.body);
	assert_eq!(fake.with(|f| f.refreshes), 1);
	assert_eq!(b.get(&app, "/api/v1/me").await.status, StatusCode::OK);
	assert_eq!(fake.with(|f| f.refreshes), 1, "the rotated token is fresh: no second refresh");
}

#[tokio::test]
async fn concierge_down_is_a_503_not_a_sign_out() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	// Nothing listens there.
	let concierge = Concierge::new("http://127.0.0.1:9", SECRET).unwrap();
	let app = http::app(SignIn::new(
		panel.clone(),
		concierge,
		SignInConfig {
			panel_origin: PANEL.to_owned(),
			concierge_origin: "http://concierge.test".to_owned(),
		},
	));
	let now = Timestamp::now();
	let tokens = panel::session::Tokens {
		access: "a".to_owned().into(),
		refresh: "r".to_owned().into(),
		access_expires_at: now + SignedDuration::from_mins(15),
		refresh_expires_at: now + SignedDuration::from_hours(24),
	};
	let opened = panel.open_session(OPERATOR.parse().unwrap(), &tokens, now).await.unwrap();
	let mut b = Browser::default();
	b.jar.insert("sa_session".into(), opened.cookie.to_string());
	let me = b.get(&app, "/api/v1/me").await;
	assert_eq!(me.status, StatusCode::SERVICE_UNAVAILABLE, "{}", me.body);
	assert!(b.jar.contains_key("sa_session"), "the session survives concierge being down");
}

/// Seeds the fake with a user and makes the next code theirs.
fn user(fake: &FakeConcierge, id: &str, who: &str, permissions: &[&str]) {
	fake.with(|f| {
		f.profiles.insert(format!("seed-{id}"), Ok(profile(id, who, permissions)));
		f.next_user = Some((id.into(), SignedDuration::from_mins(15)));
	});
}

// ── security review of #2 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_callback_is_redeemed_once() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	let mut b = Browser::default();
	let login = b.get(&app, "/auth/login").await;
	let location = login.headers[header::LOCATION].to_str().unwrap().to_owned();
	let param = |name: &str| location.split(['?', '&']).find_map(|p| p.strip_prefix(&format!("{name}="))).unwrap().to_owned();
	fake.with(|f| f.challenge = Some(param("code_challenge")));
	let prelogin = b.jar["sa_prelogin"].clone();
	let callback = format!("/auth/callback?code={CODE}&state={}", param("state"));
	assert_eq!(b.get(&app, &callback).await.status, StatusCode::SEE_OTHER);

	// The same URL and the same pre-login cookie again, as a replay would have them.
	let mut replay = Browser::default();
	replay.jar.insert("sa_prelogin".into(), prelogin);
	let again = replay.get(&app, &callback).await;
	assert_eq!(again.status, StatusCode::BAD_REQUEST);
	assert!(!replay.jar.contains_key("sa_session"));
	assert_eq!(fake.with(|f| f.exchanges), 1, "the code is presented once");
	let csp = again.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
	assert_eq!(csp, "default-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'");
	assert_eq!(again.headers["x-content-type-options"], "nosniff");
}

#[tokio::test]
async fn auth_and_api_shed_and_time_out() {
	let db = TestDb::create().await;
	let limits = http::Limits {
		auth_concurrent: 1,
		api_timeout: std::time::Duration::from_millis(300),
		..Default::default()
	};
	let (app, fake, _) = setup_with(&db, limits).await;
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	fake.with(|f| f.exchange_delay = std::time::Duration::from_millis(500));

	// One slow callback holds /auth's only slot: the next /auth request is shed, not queued.
	let mut slow = Browser::default();
	let mut other = Browser::default();
	let (signed_in, shed) = tokio::join!(slow.sign_in(&app, &fake, None), async {
		tokio::time::sleep(std::time::Duration::from_millis(150)).await;
		other.get(&app, "/auth/login").await
	});
	assert_eq!(signed_in.status, StatusCode::SEE_OTHER);
	assert_eq!(shed.status, StatusCode::SERVICE_UNAVAILABLE);
	assert_eq!(shed.headers["x-content-type-options"], "nosniff");

	// GetMe slower than /api/v1's budget: 503, and the session stays.
	fake.with(|f| f.me_delay = std::time::Duration::from_millis(400));
	let me = slow.get(&app, "/api/v1/me").await;
	assert_eq!(me.status, StatusCode::SERVICE_UNAVAILABLE, "{}", me.body);
	fake.with(|f| f.me_delay = std::time::Duration::ZERO);
	assert_eq!(slow.get(&app, "/api/v1/me").await.status, StatusCode::OK);
}

#[tokio::test]
async fn key_changes_ask_concierge_afresh() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	user(&fake, ADMIN, "investor", SA_ADMIN.members);
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	assert_eq!(b.get(&app, "/api/v1/me").await.body["permissions"], json!(admin()));

	let panel_key = b.post(&app, "/api/v1/sources", json!({"key_id": "hand", "kind": "panel", "brands": ["aquafix"]})).await;
	assert_eq!(panel_key.status, StatusCode::BAD_REQUEST, "{}", panel_key.body);
	let site = b.post(&app, "/api/v1/sources", json!({"key_id": "aquafix-site", "kind": "site", "brands": ["aquafix"]})).await;
	assert_eq!(site.status, StatusCode::CREATED, "{}", site.body);

	// The permission is taken away at concierge. Reads ride the cache for up to a minute; minting
	// and revoking keys do not.
	fake.with(|f| {
		for p in f.profiles.values_mut().flatten() {
			if p.user_id == ADMIN {
				*p = profile(ADMIN, "investor", &[]);
			}
		}
	});
	assert_eq!(b.get(&app, "/api/v1/sources").await.status, StatusCode::OK, "cached");
	let minted = b.post(&app, "/api/v1/sources", json!({"key_id": "vifnet-site", "kind": "site", "brands": ["vifnet"]})).await;
	assert_eq!(minted.status, StatusCode::FORBIDDEN, "{}", minted.body);
	let revoked = b.send(&app, Method::DELETE, "/api/v1/sources/aquafix-site", None, true).await;
	assert_eq!(revoked.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn signing_in_again_and_out_closes_the_old_sessions() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);

	let mut laptop = Browser::default();
	laptop.sign_in(&app, &fake, None).await;
	let first = laptop.jar["sa_session"].clone();
	fake.with(|f| f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15))));
	laptop.sign_in(&app, &fake, None).await;
	assert_ne!(laptop.jar["sa_session"], first);
	let mut stale = Browser::default();
	stale.jar.insert("sa_session".into(), first);
	assert_eq!(stale.get(&app, "/api/v1/me").await.status, StatusCode::UNAUTHORIZED, "the replaced session is closed");

	let mut phone = Browser::default();
	fake.with(|f| f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15))));
	phone.sign_in(&app, &fake, None).await;
	assert_eq!(phone.get(&app, "/api/v1/me").await.status, StatusCode::OK, "cached now");
	assert_eq!(laptop.send(&app, Method::POST, "/auth/logout", None, true).await.status, StatusCode::NO_CONTENT);
	assert_eq!(phone.get(&app, "/api/v1/me").await.status, StatusCode::UNAUTHORIZED, "signed out everywhere");
}

#[tokio::test]
async fn money_is_bounded_and_writes_are_idempotent() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	fake.with(|f| f.me_fails_once = Some(Code::DeadlineExceeded));
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	assert_eq!(b.get(&app, "/api/v1/me").await.status, StatusCode::OK, "a GetMe timeout is asked once more");
	assert_eq!(fake.with(|f| f.me_calls), 2);

	let new_lead = json!({"brand": "aquafix", "location": "paris-11", "need": "a leaking tap"});
	let with_key = |b: &mut Browser, uri: String, body: Value, key: &'static str| {
		let app = app.clone();
		let cookie: Vec<String> = b.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
		let csrf = b.jar["sa_csrf"].clone();
		async move {
			let req = Request::post(uri)
				.header(header::COOKIE, cookie.join("; "))
				.header("x-sa-csrf", csrf)
				.header("idempotency-key", key)
				.header(header::CONTENT_TYPE, "application/json")
				.body(Body::from(body.to_string()))
				.unwrap();
			let res = app.oneshot(req).await.unwrap();
			let status = res.status();
			let body: Value = serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap_or(Value::Null);
			(status, body)
		}
	};
	let (s1, first) = with_key(&mut b, "/api/v1/leads".into(), new_lead.clone(), "lead-1").await;
	let (s2, again) = with_key(&mut b, "/api/v1/leads".into(), new_lead.clone(), "lead-1").await;
	assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::OK), "{first} {again}");
	assert_eq!(first, again, "the retry is answered with the first lead");
	let lead = first["lead_id"].as_str().unwrap().to_owned();
	let (_, other) = with_key(&mut b, "/api/v1/leads".into(), new_lead, "lead-2").await;
	assert_ne!(other["lead_id"], lead.as_str(), "another key, another lead");

	b.post(&app, &format!("/api/v1/leads/aquafix/{lead}/stage"), json!({"stage": "won"})).await;
	let pay = |billed: i64, currency: &str| json!({"billed": billed, "commission": 0, "currency": currency});
	let too_much = b.post(&app, &format!("/api/v1/leads/aquafix/{lead}/payments"), pay(10_000_000_001, "EUR")).await;
	assert_eq!(too_much.status, StatusCode::BAD_REQUEST, "{}", too_much.body);
	let yen = b.post(&app, &format!("/api/v1/leads/aquafix/{lead}/payments"), pay(100, "JPY")).await;
	assert_eq!(yen.status, StatusCode::BAD_REQUEST, "{}", yen.body);
	let quote = b
		.post(
			&app,
			&format!("/api/v1/leads/aquafix/{lead}/stage"),
			json!({"stage": "quoted", "amount": -10_000_000_001_i64, "currency": "EUR"}),
		)
		.await;
	assert_eq!(quote.status, StatusCode::BAD_REQUEST, "{}", quote.body);

	let uri = format!("/api/v1/leads/aquafix/{lead}/payments");
	let (p1, paid) = with_key(&mut b, uri.clone(), pay(12_000, "EUR"), "pay-1").await;
	let (p2, paid_again) = with_key(&mut b, uri, pay(12_000, "EUR"), "pay-1").await;
	assert_eq!((p1, p2), (StatusCode::CREATED, StatusCode::OK), "{paid} {paid_again}");
	assert_eq!(paid["event_id"], paid_again["event_id"]);
	let card = b.get(&app, &format!("/api/v1/leads/aquafix/{lead}")).await;
	let payments = card.body["events"].as_array().unwrap().iter().filter(|e| e["type"] == "payment.received").count();
	assert_eq!(payments, 1, "the retry journaled nothing");
	assert_eq!(card.headers["x-content-type-options"], "nosniff");
}

// ── Telegram in the profile ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_profile_links_telegram_and_chooses_rules() {
	let db = TestDb::create().await;
	let fake = FakeConcierge::default();
	let addr = serve_fake(fake.clone()).await;
	let panel = panel(&db).await;
	let sign_in = SignIn::new(
		panel.clone(),
		Concierge::new(&addr, SECRET).unwrap(),
		SignInConfig {
			panel_origin: PANEL.to_owned(),
			concierge_origin: "http://concierge.test".to_owned(),
		},
	);
	let app = http::app_with_telegram(sign_in.clone(), http::Limits::default(), telegram::BotName::on(Some("@evinvest_sa_bot".into())));
	let off = http::app_with(sign_in, http::Limits::default());
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	// A link whose access was last confirmed long ago, and denied: the gate's GetMe renews it.
	let pool = db.pool().await;
	sqlx::query("INSERT INTO telegram_links (user_id, chat_id, linked_at, permissions, permissions_checked_at, display_name) VALUES ($1, 7, $2, NULL, $3, 'x')")
		.bind(uuid::Uuid::parse_str(OPERATOR).unwrap())
		.bind(jiff::Timestamp::now().as_microsecond())
		.bind("2026-01-01T00:00:00Z".parse::<jiff::Timestamp>().unwrap().as_microsecond())
		.execute(&pool)
		.await
		.unwrap();
	let mut b = Browser::default();
	assert_eq!(b.get(&app, "/api/v1/telegram").await.status, StatusCode::UNAUTHORIZED);
	b.sign_in(&app, &fake, None).await;

	let got = b.get(&app, "/api/v1/telegram").await;
	assert_eq!(got.status, StatusCode::OK, "{}", got.body);
	assert_eq!(
		got.body,
		json!({"enabled": true, "linked": true, "blocked": false, "account": null, "rules": {"new_lead": true, "contact_overdue": true, "booked": true}}),
		"an operator's rules, at their defaults"
	);
	let (permissions, name): (Option<String>, String) = sqlx::query_as("SELECT permissions, display_name FROM telegram_links").fetch_one(&pool).await.unwrap();
	let permissions: PermissionSet = serde_json::from_str(&permissions.unwrap()).unwrap();
	assert_eq!((permissions, name.as_str()), (operator(), "Ann"), "the gate's GetMe confirmed the link");

	assert_eq!(b.send(&app, Method::POST, "/api/v1/telegram/link", None, false).await.status, StatusCode::FORBIDDEN, "CSRF");
	let seen: Option<i64> = sqlx::query_scalar("SELECT last_seen_at FROM sessions").fetch_one(&pool).await.unwrap();
	assert!(seen.is_some(), "the gate marks the session used");
	let asked = fake.with(|f| f.me_calls);
	let link = b.send(&app, Method::POST, "/api/v1/telegram/link", None, true).await;
	assert_eq!(fake.with(|f| f.me_calls), asked + 1, "a link token asks concierge afresh");
	assert_eq!(link.status, StatusCode::CREATED, "{}", link.body);
	let url = link.body["url"].as_str().unwrap();
	let token = url.strip_prefix("https://t.me/evinvest_sa_bot?start=").unwrap();
	assert!(token.len() == 43 && token.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'), "{url}");

	let muted = b.send(&app, Method::PUT, "/api/v1/telegram/rules", Some(json!({"rules": {"new_lead": false}})), true).await;
	assert_eq!(muted.status, StatusCode::OK, "{}", muted.body);
	assert_eq!(muted.body["rules"], json!({"new_lead": false, "contact_overdue": true, "booked": true}));
	let admins = b
		.send(&app, Method::PUT, "/api/v1/telegram/rules", Some(json!({"rules": {"payment_received": true}})), true)
		.await;
	assert_eq!(admins.status, StatusCode::BAD_REQUEST, "payments are the admins'");
	let unknown = b.send(&app, Method::PUT, "/api/v1/telegram/rules", Some(json!({"rules": {"review_low": true}})), true).await;
	assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "not a rule yet");

	assert_eq!(b.send(&app, Method::DELETE, "/api/v1/telegram/link", None, true).await.status, StatusCode::NO_CONTENT);
	assert_eq!(b.send(&app, Method::DELETE, "/api/v1/telegram/link", None, true).await.status, StatusCode::NOT_FOUND);
	assert_eq!(b.get(&app, "/api/v1/telegram").await.body["linked"], false);

	// Without a bot the profile says so, and there is nothing to link.
	assert_eq!(b.get(&off, "/api/v1/telegram").await.body["enabled"], false);
	assert_eq!(b.send(&off, Method::POST, "/api/v1/telegram/link", None, true).await.status, StatusCode::SERVICE_UNAVAILABLE);
}

// ── a place's live settings ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_admin_edits_a_places_settings_and_the_sites_read_them() {
	let db = TestDb::create().await;
	let (app, fake, panel) = setup(&db).await;
	user(&fake, ADMIN, "investor", SA_ADMIN.members);
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let mut site = Browser::default();
	let live = "/api/internal/brands/aquafix/locations/royat?locale=fr";
	let settings = "/api/v1/places/aquafix/royat/settings";

	let unknown = site.get(&app, live).await;
	assert_eq!((unknown.status, unknown.body), (StatusCode::OK, json!({})), "an unknown place is never a 404");
	for odd in ["/api/internal/brands/Aquafix/locations/royat", "/api/internal/brands/aquafix/locations/+33612345678"] {
		assert_eq!(site.get(&app, odd).await.body, json!({}), "{odd}");
	}

	let empty = b.get(&app, settings).await;
	assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
	assert_eq!(
		empty.body,
		json!({"brand": "aquafix", "slug": "royat", "withdrawn": false, "settings": {}, "updated_at": null, "updated_by": null, "can_edit": true})
	);

	let wanted = json!({
		"phone": "+33423500640",
		"hours": [{"days": ["Monday", "Tuesday"], "opens": "08:00", "closes": "19:00"}],
		"serviceArea": ["Royat", "Chamalières"],
	});
	let invalid = put(&mut b, &app, settings, json!({"settings": {"phone": "0423500640", "fax": "x"}, "expected_updated_at": null})).await;
	assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", invalid.body);
	assert_eq!(invalid.body["error"], "invalid");
	assert_eq!(invalid.body["fields"]["phone"], "must be E.164, e.g. +33612345678");
	let rows = json!({"settings": {"hours": [{"days": ["Monday"], "opens": "8:00", "closes": "19:00"}], "serviceArea": ["Royat", " "]}});
	let nested = put(&mut b, &app, settings, rows).await;
	assert_eq!(nested.status, StatusCode::UNPROCESSABLE_ENTITY);
	let keys: Vec<&String> = nested.body["fields"].as_object().unwrap().keys().collect();
	assert_eq!(keys, ["hours[0].opens", "serviceArea[1]"]);
	assert!(invalid.body["fields"]["fax"].is_string());
	let no_csrf = b.send(&app, Method::PUT, settings, Some(json!({"settings": wanted, "expected_updated_at": null})), false).await;
	assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);

	let saved = put(&mut b, &app, settings, json!({"settings": wanted, "expected_updated_at": null})).await;
	assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
	assert_eq!(saved.body["settings"], wanted);
	assert_eq!(saved.body["updated_by"], "investor@example.com");
	let updated_at = saved.body["updated_at"].as_str().unwrap().to_owned();
	let stale = put(&mut b, &app, settings, json!({"settings": {}, "expected_updated_at": null})).await;
	assert_eq!((stale.status, stale.body), (StatusCode::CONFLICT, json!({"error": "conflict"})));
	assert_eq!(site.get(&app, live).await.body, wanted, "what the site reads: only the fields set");

	let cleared = put(&mut b, &app, settings, json!({"settings": {"phone": "+33612345678"}, "expected_updated_at": updated_at})).await;
	assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
	let history = b.get(&app, &format!("{settings}/history")).await;
	assert_eq!(history.status, StatusCode::OK, "{}", history.body);
	let changes = history.body["changes"].as_array().unwrap();
	assert_eq!(changes.len(), 2);
	assert_eq!((changes[0]["kind"].as_str(), changes[0]["by"].as_str()), (Some("set"), Some("investor@example.com")));
	assert_eq!((changes[0]["before"].clone(), changes[0]["after"].clone()), (wanted.clone(), json!({"phone": "+33612345678"})));

	let id = changes[0]["id"].as_str().unwrap();
	let stale = b.post(&app, &format!("{settings}/revert/{id}"), json!({"expected_updated_at": updated_at})).await;
	assert_eq!((stale.status, stale.body), (StatusCode::CONFLICT, json!({"error": "conflict"})), "as a PUT");
	let current = cleared.body["updated_at"].clone();
	let reverted = b.post(&app, &format!("{settings}/revert/{id}"), json!({"expected_updated_at": current})).await;
	assert_eq!(reverted.status, StatusCode::OK, "{}", reverted.body);
	assert_eq!(reverted.body["settings"], wanted);
	assert_eq!(
		b.post(&app, &format!("{settings}/revert/0190a7c4-0000-7000-8000-0000000000ff"), json!(null)).await.status,
		StatusCode::NOT_FOUND
	);

	let withdrawn = b.post(&app, "/api/v1/places/aquafix/royat/withdraw", json!(null)).await;
	assert_eq!((withdrawn.status, withdrawn.body["withdrawn"].clone()), (StatusCode::OK, json!(true)));
	let gone = site.get(&app, live).await;
	assert_eq!((gone.status, gone.body), (StatusCode::NOT_FOUND, json!({"error": "not_found"})));
	assert_eq!(b.post(&app, "/api/v1/places/aquafix/royat/restore", json!(null)).await.status, StatusCode::OK);
	assert_eq!(site.get(&app, live).await.body, wanted);

	let added = b.post(&app, "/api/v1/places", json!({"brand": "aquafix", "slug": "vichy"})).await;
	assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
	assert_eq!(added.body["settings"], json!({}));
	let again = b.post(&app, "/api/v1/places", json!({"brand": "aquafix", "slug": "vichy"})).await;
	assert_eq!((again.status, again.body), (StatusCode::CONFLICT, json!({"error": "exists"})));
	let known = b.post(&app, "/api/v1/places", json!({"brand": "aquafix", "slug": "royat"})).await;
	assert_eq!(known.status, StatusCode::CONFLICT, "registered by its first edit");
	let places = b.get(&app, "/api/v1/places").await;
	assert_eq!(
		places.body["places"],
		json!([
			{"brand": "aquafix", "location": "royat", "last_lead_at": null, "has_settings": true, "withdrawn": false},
			{"brand": "aquafix", "location": "vichy", "last_lead_at": null, "has_settings": false, "withdrawn": false},
		])
	);

	// Without the sign-in configured, the sites still read.
	let bare = http::router(panel);
	assert_eq!(Browser::default().get(&bare, live).await.body, wanted);
}

#[tokio::test]
async fn an_operator_reads_a_places_settings_and_changes_nothing() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let read = b.get(&app, "/api/v1/places/aquafix/royat/settings").await;
	assert_eq!(read.status, StatusCode::OK, "{}", read.body);
	assert_eq!(read.body["can_edit"], false);
	assert_eq!(b.get(&app, "/api/v1/places/aquafix/royat/settings/history").await.body, json!({"changes": []}));
	let body = json!({"settings": {"phone": "+33612345678"}, "expected_updated_at": null});
	assert_eq!(put(&mut b, &app, "/api/v1/places/aquafix/royat/settings", body).await.status, StatusCode::FORBIDDEN);
	for uri in ["/api/v1/places/aquafix/royat/withdraw", "/api/v1/places/aquafix/royat/restore"] {
		assert_eq!(b.post(&app, uri, json!(null)).await.status, StatusCode::FORBIDDEN, "{uri}");
	}
	let register = b.post(&app, "/api/v1/places", json!({"brand": "aquafix", "slug": "vichy"})).await;
	assert_eq!(register.status, StatusCode::FORBIDDEN);
}

async fn put(b: &mut Browser, app: &Router, uri: &str, body: Value) -> Answer {
	b.send(app, Method::PUT, uri, Some(body), true).await
}

// ── permissions ──────────────────────────────────────────────────────────────────────────

/// A signed-in caller holding nothing in `sa` is shown nothing: every `/api/v1` route but `/me`,
/// the access requests and the profile's Telegram ones answers 403, and so does the live socket; with every
/// permission, none of them does.
#[tokio::test]
async fn no_permission_opens_no_section() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let app = |permissions: PermissionSet, email: &str| {
		let who = panel_server::concierge::DevIdentity {
			permissions,
			email: email.to_owned(),
		};
		let config = SignInConfig {
			panel_origin: PANEL.to_owned(),
			concierge_origin: PANEL.to_owned(),
		};
		http::app_with_telegram(SignIn::new(panel.clone(), Concierge::dev(who), config), http::Limits::default(), telegram::BotName::off())
	};
	let routes = http::api_routes();
	let open: Vec<&str> = routes.iter().filter(|r| r.section.is_none()).map(|r| r.path).collect();
	assert_eq!(open, ["/me", "/access/requests", "/access/requests/mine"], "every other route sits under a section");
	for (permissions, email, holds) in [(PermissionSet::from_iter(Vec::<String>::new()), "nobody", false), (admin(), "admin", true)] {
		let app = app(permissions, &format!("{email}@localhost"));
		let mut b = Browser::default();
		let login = b.get(&app, "/auth/login").await;
		let callback = login.headers[header::LOCATION].to_str().unwrap().strip_prefix(PANEL).unwrap().to_owned();
		assert_eq!(b.get(&app, &callback).await.status, StatusCode::SEE_OTHER);
		for r in &routes {
			let path: Vec<String> = r.path.split('/').map(|seg| if seg.starts_with('{') { "x".to_owned() } else { seg.to_owned() }).collect();
			let uri = format!("/api/v1{}", path.join("/"));
			let got = b.send(&app, r.method.clone(), &uri, Some(json!({})), true).await;
			let forbidden = got.status == StatusCode::FORBIDDEN;
			match (r.section, holds) {
				(None, _) => assert!(!forbidden, "{} {uri}: {}", r.method, got.body),
				(Some(_), false) => assert!(forbidden, "{} {uri}: {} {}", r.method, got.status, got.body),
				(Some(_), true) => assert!(!forbidden, "{} {uri}: {}", r.method, got.body),
			}
		}
		assert_eq!(b.get(&app, "/api/v1/telegram").await.status, StatusCode::OK, "the profile is everyone's");
		let cookie: Vec<String> = b.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
		let live = Request::get("/api/v1/live")
			.header(header::ORIGIN, PANEL)
			.header(header::COOKIE, cookie.join("; "))
			.body(Body::empty())
			.unwrap();
		let live = app.clone().oneshot(live).await.unwrap().status();
		assert_eq!(live == StatusCode::FORBIDDEN, !holds, "the live socket: {live}");
	}
}

/// `/auth/login?return_to=` lands the browser back on a path of this origin, and refuses
/// anything a browser could read as another origin.
#[tokio::test]
async fn a_sign_in_returns_to_a_path_of_this_origin_only() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	for evil in ["%2F%2Fevil.com", "https%3A%2F%2Fevil.com", "%2F%5Cevil.com", "evil.com", "%2Fa%0Ab", "%2Fa%20b"] {
		let got = Browser::default().get(&app, &format!("/auth/login?return_to={evil}")).await;
		assert_eq!(got.status, StatusCode::BAD_REQUEST, "{evil}");
		assert!(got.headers.get(header::LOCATION).is_none(), "{evil}");
	}
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	let cb = Browser::default()
		.sign_in_from(&app, &fake, "/auth/login?return_to=%2Freview_archive%3Ftab%3Dtargets", None)
		.await;
	assert_eq!(
		(cb.status, cb.headers[header::LOCATION].to_str().unwrap()),
		(StatusCode::SEE_OTHER, "/review_archive?tab=targets")
	);
	fake.with(|f| f.next_user = Some((OPERATOR.into(), SignedDuration::from_mins(15))));
	let cb = Browser::default().sign_in(&app, &fake, None).await;
	assert_eq!(cb.headers[header::LOCATION], "/", "none given");
}

/// `serve` publishes the `sa` catalog, versioned by PANEL_BUILD_EPOCH, before it serves; a
/// catalog concierge refuses fails the boot.
#[tokio::test]
async fn serve_publishes_the_catalog_first() {
	let db = TestDb::create().await;
	let fake = FakeConcierge::default();
	let addr = serve_fake(fake.clone()).await;
	let path = db.path().display().to_string();
	let serve = || {
		tokio::process::Command::new(env!("CARGO_BIN_EXE_panel"))
			.args(["serve", "--bind", "127.0.0.1:0"])
			.env_clear()
			.envs([
				("APP_ENV", "development"),
				("PANEL_DB_PATH", path.as_str()),
				("PANEL_DATA_KEY", &"0".repeat(64)),
				("PANEL_PUBLIC_ORIGIN", PANEL),
				("CONCIERGE_PUBLIC_ORIGIN", "http://concierge.test"),
				("CONCIERGE_GRPC_ADDR", addr.as_str()),
				("RP_CLIENT_SECRET_SA", SECRET),
				("PANEL_BUILD_EPOCH", "1791100000"),
			])
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null())
			.kill_on_drop(true)
			.spawn()
			.unwrap()
	};
	let mut panel = serve();
	let published = tokio::time::timeout(std::time::Duration::from_secs(20), async {
		loop {
			if let Some(p) = fake.with(|f| f.published.first().cloned()) {
				return p;
			}
			tokio::time::sleep(std::time::Duration::from_millis(50)).await;
		}
	})
	.await
	.expect("serve publishes at boot");
	panel.kill().await.unwrap();
	let catalog = sa_auth::Catalog::collect("sa", 1_791_100_000);
	assert_eq!(published.version, catalog.version);
	assert_eq!(published.permissions, catalog.permissions.iter().cloned().collect::<Vec<_>>());
	let admin = published.aliases.iter().find(|a| a.name == "sa:admin").unwrap();
	assert_eq!(admin.delegates, ["sa:operator"], "an admin grants operators");

	fake.with(|f| f.refuse_catalog = true);
	let status = tokio::time::timeout(std::time::Duration::from_secs(20), serve().wait())
		.await
		.expect("a refused catalog ends the boot")
		.unwrap();
	assert!(!status.success(), "{status}");
}

#[tokio::test]
async fn switching_account_asks_concierge_for_the_chooser_and_leaves_the_browser_as_the_other() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let first_session = b.jar["sa_session"].clone();

	let login = b.get(&app, "/auth/login?prompt=select_account&return_to=%2Freview_archive").await;
	let location = login.headers[header::LOCATION].to_str().unwrap();
	assert!(location.ends_with("&prompt=select_account"), "{location}");

	user(&fake, ADMIN, "admin", SA_ADMIN.members);
	let cb = b.sign_in_from(&app, &fake, "/auth/login?prompt=select_account&return_to=%2Freview_archive", None).await;
	assert_eq!((cb.status, cb.headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/review_archive"));
	assert_eq!(b.get(&app, "/api/v1/me").await.body["user_id"], ADMIN);

	let mut stale = Browser::default();
	stale.jar.insert("sa_session".into(), first_session);
	assert_eq!(stale.get(&app, "/api/v1/me").await.status, StatusCode::UNAUTHORIZED, "the first account's session is closed");

	assert_eq!(b.get(&app, "/auth/login?prompt=login").await.status, StatusCode::BAD_REQUEST, "no other prompt is passed on");
}

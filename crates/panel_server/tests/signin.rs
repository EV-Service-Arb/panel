//! Signing in and the conductor's own API end to end: the real router and a real SQLite file, with a
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
	testing::{TestDb, admin, operator, panel},
};
use panel_contracts::concierge::v1::{
	self as pb,
	auth_service_server::{AuthService, AuthServiceServer},
	user_directory_server::{UserDirectory, UserDirectoryServer},
};
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
	let app = http::app(
		SignIn::new(
			panel.clone(),
			concierge,
			SignInConfig {
				panel_origin: format!("{PANEL}/"),
				concierge_origin: "http://concierge.test".to_owned(),
			},
		),
		None,
		None,
		limits,
		telegram::BotName::off(),
	);
	(app, fake, panel)
}

const OPERATOR: &str = "0190a7c4-0000-7000-8000-000000000001";
const OUTSIDER: &str = "0190a7c4-0000-7000-8000-000000000002";
const ADMIN: &str = "0190a7c4-0000-7000-8000-000000000003";

// ── the tests ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_operator_signs_in_and_signs_out() {
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
		json!({"user_id": OPERATOR, "email": "investor@example.com", "preferred_name": "Ann", "permissions": operator(), "dev_sign_in": false})
	);
	assert_eq!(me.headers[header::CACHE_CONTROL], "no-store");

	let no_csrf = b.send(&app, Method::POST, "/api/v1/access/requests", Some(json!({"need": "sa:admin"})), false).await;
	assert_eq!(no_csrf.status, StatusCode::FORBIDDEN, "a write without the CSRF header");
	let asked = b.post(&app, "/api/v1/access/requests", json!({"need": "sa:admin"})).await;
	assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.body);
	assert_eq!(b.get(&app, "/api/v1/sources").await.status, StatusCode::FORBIDDEN, "sources are an admin's");

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
	assert_eq!(outsider.get(&app, "/api/v1/sources").await.status, StatusCode::FORBIDDEN);

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
	let app = http::app(
		SignIn::new(
			panel.clone(),
			concierge,
			SignInConfig {
				panel_origin: PANEL.to_owned(),
				concierge_origin: "http://concierge.test".to_owned(),
			},
		),
		None,
		None,
		http::Limits::default(),
		telegram::BotName::off(),
	);
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
async fn a_getme_timeout_is_asked_once_more() {
	let db = TestDb::create().await;
	let (app, fake, _) = setup(&db).await;
	user(&fake, OPERATOR, "investor", SA_OPERATOR.members);
	fake.with(|f| f.me_fails_once = Some(Code::DeadlineExceeded));
	let mut b = Browser::default();
	b.sign_in(&app, &fake, None).await;
	let me = b.get(&app, "/api/v1/me").await;
	assert_eq!(me.status, StatusCode::OK, "a GetMe timeout is asked once more");
	assert_eq!(fake.with(|f| f.me_calls), 2);
	assert_eq!(me.headers["x-content-type-options"], "nosniff");
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
	let app = http::app(sign_in.clone(), None, None, http::Limits::default(), telegram::BotName::on(Some("@evinvest_sa_bot".into())));
	let off = http::app(sign_in, None, None, http::Limits::default(), telegram::BotName::off());
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

// ── permissions ──────────────────────────────────────────────────────────────────────────

/// A signed-in caller holding nothing in `sa` is shown nothing: every `/api/v1` route but `/me`,
/// the access requests and the profile's Telegram ones answers 403; with every permission, none
/// of them does. The live socket opens for both: what it tells is judged per message.
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
		http::app(
			SignIn::new(panel.clone(), Concierge::dev(who), config),
			None,
			None,
			http::Limits::default(),
			telegram::BotName::off(),
		)
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
		assert_ne!(live, StatusCode::FORBIDDEN, "the live socket");
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

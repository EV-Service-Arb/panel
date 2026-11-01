//! For tests only (feature `testing`): what the HTTP tests share: a stand-in for the services behind the conductor, the conductor
//! signed in through the development identity, and a browser keeping its cookies.

use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
};

use axum::{
	Router,
	body::{Body, Bytes, to_bytes},
	extract::Request,
	http::{HeaderMap, Method, StatusCode, header},
	middleware::{self, Next},
	response::IntoResponse,
};
use panel::Panel;
use sa_auth::{Assertion, Keys, PermissionSet, Service, Signer};
use tower::ServiceExt;

use crate::{
	concierge::{Concierge, DEV_CODE, DevIdentity},
	forward::{Forward, Services, Upstreams},
	grafana::Grafana,
	http,
	signin::{SignIn, SignInConfig},
	telegram::BotName,
};

pub const ORIGIN: &str = "http://127.0.0.1:59120";
pub const SEED: &str = "k1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// What a service behind the conductor was sent.
#[derive(Clone, Debug)]
pub struct Seen {
	pub method: Method,
	pub path_and_query: String,
	pub headers: HeaderMap,
	pub body: Bytes,
}

impl Seen {
	/// Its assertion, verified as the service would: for `service`, this method and path, and
	/// for a source's, this body.
	pub fn assertion(&self, service: Service, body: Option<&[u8]>) -> Assertion {
		let keys: Keys = SEED.parse::<Signer>().unwrap().public().parse().unwrap();
		let token = self.headers[sa_auth::HEADER].to_str().unwrap();
		let path = self.path_and_query.split('?').next().unwrap();
		sa_auth::verify(&keys, token, service, self.method.as_str(), path, body, jiff::Timestamp::now().as_second()).expect("the conductor's assertion")
	}
}

pub type Log = Arc<Mutex<Vec<Seen>>>;

/// A service at a loopback address: `routes`, and anything else answered with a cookie of its
/// own and a compressed, cacheable body. Every request is logged first.
pub async fn upstream(routes: Router) -> (String, Log) {
	let seen: Log = Arc::default();
	let log = seen.clone();
	let app = routes
		.fallback(|| async {
			(
				[
					(header::SET_COOKIE, "upstream=1; Path=/"),
					(header::HeaderName::from_static("x-upstream"), "yes"),
					(header::CONTENT_ENCODING, "gzip"),
					(header::CACHE_CONTROL, "public, max-age=31536000"),
				],
				"ok",
			)
				.into_response()
		})
		.layer(middleware::from_fn(move |req: Request, next: Next| {
			let log = log.clone();
			async move {
				let (parts, body) = req.into_parts();
				let body = to_bytes(body, usize::MAX).await.unwrap();
				log.lock().unwrap().push(Seen {
					method: parts.method.clone(),
					path_and_query: parts.uri.path_and_query().unwrap().to_string(),
					headers: parts.headers.clone(),
					body: body.clone(),
				});
				next.run(Request::from_parts(parts, Body::from(body))).await
			}
		}));
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let addr = listener.local_addr().unwrap();
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	(format!("http://{addr}"), seen)
}

/// The services, all at `base`.
pub fn services(base: &str) -> Services {
	Services::new(Upstreams {
		review_archive: base.parse().unwrap(),
		playbook: base.parse().unwrap(),
		sites: base.parse().unwrap(),
		signer: SEED.parse().unwrap(),
	})
}

/// The conductor on `panel`, signing in whoever asks as `email` holding `permissions`, with every
/// service at `base`.
pub fn app_with(panel: Panel, permissions: PermissionSet, email: &str, base: &str, limits: http::Limits) -> Router {
	let (sign_in, forward) = conductor(panel, permissions, email, base);
	http::app(sign_in, Some(forward), None, limits, BotName::off())
}

/// What a viewer's browser and Grafana reach: the conductor of [`app_with`] with Grafana on `dir`,
/// Grafana's loopback listener, and Grafana itself, to run.
pub fn app_with_grafana(panel: Panel, permissions: PermissionSet, email: &str, base: &str, dir: &std::path::Path) -> (Router, Router, Grafana) {
	let (sign_in, forward) = conductor(panel, permissions, email, base);
	let grafana = Grafana::new(forward.clone(), dir.to_owned());
	let app = http::app(sign_in, Some(forward), Some(&grafana), http::Limits::default(), BotName::off());
	(app, grafana.loopback(), grafana)
}

fn conductor(panel: Panel, permissions: PermissionSet, email: &str, base: &str) -> (SignIn, Forward) {
	let who = DevIdentity {
		permissions,
		email: email.to_owned(),
	};
	let config = SignInConfig {
		panel_origin: ORIGIN.to_owned(),
		concierge_origin: ORIGIN.to_owned(),
	};
	let sign_in = SignIn::new(panel, Concierge::dev(who), config);
	let forward = Forward::new(sign_in.clone(), services(base));
	(sign_in, forward)
}

pub fn app(panel: Panel, permissions: PermissionSet, base: &str) -> Router {
	app_with(panel, permissions, "dev@localhost", base, http::Limits::default())
}

#[derive(Default)]
pub struct Browser {
	pub jar: HashMap<String, String>,
}

impl Browser {
	pub async fn send_body(&mut self, app: &Router, method: Method, uri: &str, extra: &[(&str, &str)], csrf: bool, body: Body) -> (StatusCode, HeaderMap, Bytes) {
		let mut req = Request::builder().method(method).uri(uri);
		let cookie: Vec<String> = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
		if !cookie.is_empty() {
			req = req.header(header::COOKIE, cookie.join("; "));
		}
		if csrf && let Some(t) = self.jar.get("sa_csrf") {
			req = req.header("x-sa-csrf", t);
		}
		for (k, v) in extra {
			req = req.header(*k, *v);
		}
		let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
		for c in res.headers().get_all(header::SET_COOKIE) {
			let (pair, _) = c.to_str().unwrap().split_once(';').unwrap();
			let (k, v) = pair.split_once('=').unwrap();
			self.jar.insert(k.to_owned(), v.to_owned());
		}
		let (status, headers) = (res.status(), res.headers().clone());
		let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
		(status, headers, body)
	}

	pub async fn send(&mut self, app: &Router, method: Method, uri: &str, extra: &[(&str, &str)], csrf: bool) -> (StatusCode, HeaderMap) {
		let (status, headers, _) = self.send_body(app, method, uri, extra, csrf, Body::empty()).await;
		(status, headers)
	}

	pub async fn sign_in(&mut self, app: &Router) {
		let (_, headers) = self.send(app, Method::GET, "/auth/login", &[], false).await;
		let location = headers[header::LOCATION].to_str().unwrap().strip_prefix(ORIGIN).unwrap().to_owned();
		assert!(location.starts_with(&format!("/auth/callback?code={DEV_CODE}")));
		self.send(app, Method::GET, &location, &[], false).await;
		assert!(self.jar.contains_key("sa_session"));
	}

	/// The `Cookie` header this browser sends.
	pub fn cookie(&self) -> String {
		self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
	}
}

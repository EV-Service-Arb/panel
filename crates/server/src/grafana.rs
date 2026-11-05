//! Grafana, the conductor's (docs/ARCHITECTURE.md, Grafana): one process `serve` runs on a unix
//! socket in its data dir, which holds no identity: the conductor vouches for each viewer.
//!
//! ```text
//! /grafana/*             → Grafana, `/grafana` stripped                        session (a POST is a read)
//! /grafana/api/ds/query  → the same, a viewer token in every query on a service's datasource
//! Grafana → 127.0.0.1:<any>/api/<service>/*, GET + the token → the forward, as that viewer
//! the services in DASHBOARDS → GET /dashboards → <dir>/dashboards/<service>/<uid>.json
//! ```

use std::{
	collections::{HashMap, HashSet},
	future::Future,
	net::SocketAddr,
	path::{Path, PathBuf},
	pin::Pin,
	sync::{Arc, Mutex},
	task::{Context, Poll},
	time::Duration,
};

use axum::{
	Router,
	body::{Body, to_bytes},
	extract::{Request, State},
	http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
	response::Response,
	routing::{any, post},
};
use eyre::WrapErr;
use hyper_util::{
	client::legacy::Client,
	rt::{TokioExecutor, TokioIo},
};
use sa_auth::{Principal, Service};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::{
	forward::{self, DASHBOARDS, Forward, json_error},
	signin::{self, Caller, Denied, Freshness},
};

/// Carries a viewer's token on Grafana's fetches.
pub const VIEWER_HEADER: &str = "x-sa-viewer";

const INFINITY: &str = "yesoreyeram-infinity-datasource";

/// How often each service is asked for its dashboards again: a conditional GET.
const PULL_EVERY: Duration = Duration::from_secs(60);
const PULL_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of a service's dashboards read.
const DASHBOARDS_ANSWER: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct Grafana {
	forward: Forward,
	dir: Arc<Path>,
	client: Client<Socket, Body>,
	viewers: Arc<Viewers>,
}

impl Grafana {
	/// Grafana on `dir`: its socket, its data, its provisioning and the services' dashboards.
	pub fn new(forward: Forward, dir: PathBuf) -> Self {
		let socket = Socket(dir.join("grafana.sock").into());
		Self {
			forward,
			dir: dir.into(),
			client: Client::builder(TokioExecutor::new()).build(socket),
			viewers: Arc::default(),
		}
	}

	pub fn routes(&self) -> Router {
		Router::new()
			.route("/grafana/api/ds/query", post(query))
			.route("/grafana/", any(pass))
			.route("/grafana/{*rest}", any(pass))
			.with_state(self.clone())
	}

	/// What Grafana's datasources reach: a service's API, for the viewer a token stands for.
	pub(crate) fn loopback(&self) -> Router {
		Router::new().route("/api/{*rest}", any(as_viewer)).with_state(self.clone())
	}

	/// Grafana, until `stop`: provisioned for a loopback listener of its own, with the services'
	/// dashboards. One that does not start or exits is reported, and `/grafana/` answers 502: the
	/// conductor goes on without it.
	pub async fn run(self, server: &Path, root_url: &str, stop: watch::Receiver<bool>) {
		let listener = match self.provision().await {
			Ok(l) => l,
			Err(e) => return crate::report(&e.wrap_err("grafana not provisioned: /grafana/ answers 502"), "grafana"),
		};
		let etags = Mutex::default();
		self.pull(&etags).await;
		let mut child = match self.start(server, root_url) {
			Ok(c) => c,
			Err(e) => return crate::report(&e.wrap_err("grafana did not start: /grafana/ answers 502"), "grafana"),
		};
		let group = Group(rustix::process::Pid::from_raw(child.id().expect("not waited on yet").try_into().expect("a pid fits an i32")).expect("a child's pid is positive"));
		let (mut wait_stop, mut loopback_stop) = (stop.clone(), stop.clone());
		let process = async {
			tokio::select! {
				status = child.wait() => {
					let e = match status {
						Ok(s) => eyre::eyre!("grafana exited ({s}): /grafana/ answers 502"),
						Err(e) => eyre::Report::new(e).wrap_err("waiting on grafana"),
					};
					crate::report(&e, "grafana");
				}
				_ = wait_stop.wait_for(|stopped| *stopped) => {
					group.signal(rustix::process::Signal::TERM); // Grafana stops its plugin servers on it
					if tokio::time::timeout(Duration::from_secs(10), child.wait()).await.is_err() {
						tracing::warn!("grafana took over 10 s to stop: killed");
					}
				}
			}
		};
		let loopback = async {
			let served = axum::serve(listener, self.loopback())
				.with_graceful_shutdown(async move {
					// a dropped sender is the server gone: stopping too
					let _stopped = loopback_stop.wait_for(|stopped| *stopped).await;
				})
				.await;
			if let Err(e) = served {
				crate::report(&eyre::Report::new(e).wrap_err("grafana's loopback listener"), "grafana");
			}
		};
		let pulls = crate::every(PULL_EVERY, stop, "pulling the dashboards", || async {
			self.pull(&etags).await;
			Ok(())
		});
		tokio::join!(process, loopback, pulls);
	}

	/// The loopback listener, and Grafana's provisioning for it: one Infinity datasource per
	/// service's API, its uid the service's name, and the dashboards' directory.
	async fn provision(&self) -> eyre::Result<tokio::net::TcpListener> {
		let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
			.await
			.wrap_err("binding grafana's loopback listener")?;
		let at: SocketAddr = listener.local_addr().wrap_err("the loopback listener's address")?;
		let root = self.dir.join("provisioning");
		// every section Grafana reads, or it logs an error for each missing
		for section in ["datasources", "dashboards", "alerting", "plugins"] {
			std::fs::create_dir_all(root.join(section)).wrap_err_with(|| format!("creating {}", root.join(section).display()))?;
		}
		let datasources: Vec<Value> = forward::apis()
			.map(|(service, prefix)| {
				let name = forward::name(service);
				// Infinity joins the url and a query's url as strings: the queries' start with `/`
				json!({"name": name, "uid": name, "type": INFINITY, "url": format!("http://{at}{prefix}"), "jsonData": {"allowedHosts": [format!("http://{at}{prefix}/")]}})
			})
			.collect();
		let dashboards = self.dir.join("dashboards");
		std::fs::create_dir_all(&dashboards).wrap_err_with(|| format!("creating {}", dashboards.display()))?;
		// JSON is YAML
		for (file, provisioned) in [
			("datasources/services.yaml", json!({"apiVersion": 1, "prune": true, "datasources": datasources})),
			(
				"dashboards/services.yaml",
				json!({"apiVersion": 1, "providers": [{"name": "services", "type": "file", "allowUiUpdates": false, "options": {"path": dashboards, "foldersFromFilesStructure": true}}]}),
			),
		] {
			let path = root.join(file);
			std::fs::write(&path, provisioned.to_string()).wrap_err_with(|| format!("writing {}", path.display()))?;
		}
		Ok(listener)
	}

	fn start(&self, server: &Path, root_url: &str) -> eyre::Result<tokio::process::Child> {
		let socket = self.dir.join("grafana.sock");
		// a Grafana killed with its `serve` leaves its socket behind, and the next one cannot bind it
		match std::os::unix::net::UnixStream::connect(&socket) {
			Ok(_) => eyre::bail!("a Grafana another serve left running answers on {}", socket.display()),
			Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => std::fs::remove_file(&socket).wrap_err_with(|| format!("removing the stale {}", socket.display()))?,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
			Err(e) => return Err(eyre::Report::new(e).wrap_err(format!("probing {}", socket.display()))),
		}
		tokio::process::Command::new(server)
			.env("GF_SERVER_PROTOCOL", "socket")
			.env("GF_SERVER_SOCKET", &socket)
			.env("GF_PATHS_DATA", &*self.dir)
			.env("GF_PATHS_PROVISIONING", self.dir.join("provisioning"))
			.env("GF_SERVER_ROOT_URL", root_url)
			.env("GF_SERVER_SERVE_FROM_SUB_PATH", "false") // `/grafana` is stripped on the way in
			// no one signs in to it: a viewer is who `/grafana/api/ds/query` vouched for
			.env("GF_AUTH_ANONYMOUS_ENABLED", "true")
			.env("GF_AUTH_ANONYMOUS_ORG_ROLE", "Viewer")
			.env("GF_AUTH_BASIC_ENABLED", "false")
			.env("GF_AUTH_DISABLE_LOGIN_FORM", "true")
			.env("GF_AUTH_DISABLE_LOGIN", "true")
			.env("GF_SECURITY_DISABLE_INITIAL_ADMIN_CREATION", "true")
			.process_group(0)
			.spawn()
			.wrap_err_with(|| format!("starting {}", server.display()))
	}

	/// Each service's dashboards, a failure reported per service: what it served before stays.
	async fn pull(&self, etags: &Mutex<HashMap<String, HeaderValue>>) {
		for service in DASHBOARDS {
			if let Err(e) = self.pull_one(service, etags).await {
				crate::report(&e.wrap_err(format!("{}'s dashboards kept as they were", forward::name(service))), "pulling the dashboards");
			}
		}
	}

	async fn pull_one(&self, service: Service, etags: &Mutex<HashMap<String, HeaderValue>>) -> eyre::Result<()> {
		let name = forward::name(service);
		let mut headers = HeaderMap::new();
		if let Some(etag) = etags.lock().expect("held for a lookup only").get(&name) {
			headers.insert(header::IF_NONE_MATCH, etag.clone());
		}
		let call = self
			.forward
			.services()
			.send(service, Method::GET, "/dashboards", headers, Some(Principal::Conductor), Body::empty());
		let res = tokio::time::timeout(PULL_TIMEOUT, call)
			.await
			.map_err(|_| eyre::eyre!("{name} did not answer GET /dashboards in time"))??;
		match res.status() {
			StatusCode::NOT_MODIFIED => return Ok(()),
			StatusCode::OK => {}
			s => eyre::bail!("{name} answered GET /dashboards with {s}"),
		}
		let etag = res.headers().get(header::ETAG).cloned();
		let body = to_bytes(res.into_body(), DASHBOARDS_ANSWER).await.map_err(|e| eyre::eyre!("reading {name}'s dashboards: {e}"))?;
		let served: Vec<serde_json::Map<String, Value>> = serde_json::from_slice(&body).wrap_err_with(|| format!("{name}'s dashboards"))?;
		let root = self.dir.join("dashboards");
		let mine = root.join(&name);
		let mut theirs = HashSet::new();
		for dir in std::fs::read_dir(&root).wrap_err_with(|| format!("reading {}", root.display()))? {
			let dir = dir.wrap_err_with(|| format!("reading {}", root.display()))?.path();
			if dir != mine {
				theirs.extend(uids(&dir)?);
			}
		}
		let mut files = HashMap::new();
		for dashboard in served {
			let uid = dashboard
				.get("uid")
				.and_then(Value::as_str)
				.ok_or_else(|| eyre::eyre!("a dashboard of {name} has no uid"))?
				.to_owned();
			// Grafana's own rule for a uid, which also keeps it a file name
			eyre::ensure!(
				(1..=40).contains(&uid.len()) && uid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
				"{name} serves a dashboard uid Grafana refuses: {uid:?}"
			);
			eyre::ensure!(!theirs.contains(&uid), "{name} serves dashboard {uid}, which another service serves too");
			let json = serde_json::to_vec_pretty(&dashboard).expect("a JSON object serializes");
			eyre::ensure!(files.insert(uid.clone(), json).is_none(), "{name} serves dashboard {uid} twice");
		}
		std::fs::create_dir_all(&mine).wrap_err_with(|| format!("creating {}", mine.display()))?;
		for (uid, json) in &files {
			// renamed into place: Grafana never reads half a file
			let (part, path) = (mine.join(format!(".{uid}.json.part")), mine.join(format!("{uid}.json")));
			std::fs::write(&part, json).wrap_err_with(|| format!("writing {}", part.display()))?;
			std::fs::rename(&part, &path).wrap_err_with(|| format!("moving {} into place", path.display()))?;
		}
		for gone in uids(&mine)?.difference(&files.keys().cloned().collect()) {
			let path = mine.join(format!("{gone}.json"));
			std::fs::remove_file(&path).wrap_err_with(|| format!("removing {}", path.display()))?;
		}
		if let Some(etag) = etag {
			etags.lock().expect("held for a lookup only").insert(name, etag);
		}
		Ok(())
	}
}

/// The uids of the dashboards in `dir`: its `<uid>.json` files.
fn uids(dir: &Path) -> eyre::Result<HashSet<String>> {
	let mut uids = HashSet::new();
	for file in std::fs::read_dir(dir).wrap_err_with(|| format!("reading {}", dir.display()))? {
		let file = file.wrap_err_with(|| format!("reading {}", dir.display()))?.file_name();
		let file = file.to_str().ok_or_else(|| eyre::eyre!("{} holds a file not named in UTF-8", dir.display()))?;
		if let Some(uid) = file.strip_suffix(".json").filter(|uid| !uid.starts_with('.')) {
			uids.insert(uid.to_owned());
		}
	}
	Ok(uids)
}

/// Who the viewer is, by the session, as for a read.
async fn viewer(g: &Grafana, headers: &HeaderMap) -> Result<Caller, Denied> {
	signin::check(g.forward.sign_in(), headers, Freshness::Cached).await
}

async fn pass(State(g): State<Grafana>, req: Request) -> Response {
	if let Err(denied) = viewer(&g, req.headers()).await {
		return denied.into_response(g.forward.sign_in());
	}
	let (parts, body) = req.into_parts();
	g.send(parts.method, &parts.uri, parts.headers, body).await
}

/// A viewer's query: every target on a service's datasource carries a token that stands for the
/// viewer on that service's API until Grafana answers.
async fn query(State(g): State<Grafana>, req: Request) -> Response {
	let caller = match viewer(&g, req.headers()).await {
		Ok(c) => c,
		Err(denied) => return denied.into_response(g.forward.sign_in()),
	};
	let (parts, body) = req.into_parts();
	let mut query: Value = match to_bytes(body, 1 << 20)
		.await
		.map_err(|e| e.to_string())
		.and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
	{
		Ok(q) => q,
		Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("a Grafana query: {e}")),
	};
	let Some(targets) = query.get_mut("queries").and_then(Value::as_array_mut) else {
		return json_error(StatusCode::BAD_REQUEST, "a Grafana query without `queries`");
	};
	let viewer = g.viewers.mint(caller);
	for t in targets.iter_mut().filter(|t| serde_json::from_value::<Service>(t["datasource"]["uid"].clone()).is_ok()) {
		let headers = t
			.as_object_mut()
			.expect("has a datasource")
			.entry("url_options")
			.or_insert_with(|| json!({}))
			.as_object_mut()
			.and_then(|o| o.entry("headers").or_insert_with(|| json!([])).as_array_mut());
		let Some(headers) = headers else {
			return json_error(StatusCode::BAD_REQUEST, "a Grafana query's `url_options.headers` is not a list");
		};
		headers.retain(|h| !h["key"].as_str().is_some_and(|k| k.eq_ignore_ascii_case(VIEWER_HEADER)));
		headers.push(json!({"key": VIEWER_HEADER, "value": viewer.token}));
	}
	let mut headers = parts.headers;
	headers.remove(header::CONTENT_LENGTH);
	let answer = g
		.send(parts.method, &parts.uri, headers, Body::from(serde_json::to_vec(&query).expect("a Value serializes")))
		.await;
	drop(viewer); // Grafana answers a query only once its fetches are done
	answer
}

async fn as_viewer(State(g): State<Grafana>, req: Request) -> Response {
	let token = req.headers().get(VIEWER_HEADER).and_then(|t| t.to_str().ok());
	match token.and_then(|t| g.viewers.get(t)) {
		Some(viewer) if req.method() == Method::GET => g.forward.as_viewer(viewer, req).await,
		_ => json_error(StatusCode::UNAUTHORIZED, "a viewer's token, on a GET"),
	}
}

impl Grafana {
	/// A request to Grafana, `/grafana` stripped and no one's credentials on it; its answer streams.
	async fn send(&self, method: Method, uri: &Uri, mut headers: HeaderMap, body: Body) -> Response {
		let path = uri.path_and_query().expect("routed by its path").as_str();
		let path = path.strip_prefix("/grafana").expect("routed under /grafana/");
		forward::strip(&mut headers, &forward::DROPPED_UP);
		headers.remove(header::HOST);
		let mut req = Request::new(body);
		*req.method_mut() = method;
		*req.uri_mut() = format!("http://grafana{path}").parse().expect("a path and query that arrived in a URI stay one");
		*req.headers_mut() = headers;
		match self.client.request(req).await {
			Ok(res) => {
				let (mut parts, body) = res.into_parts();
				forward::strip(&mut parts.headers, &forward::DROPPED_DOWN);
				Response::from_parts(parts, Body::new(body))
			}
			Err(e) => {
				tracing::warn!(error = %e, "grafana did not answer");
				json_error(StatusCode::BAD_GATEWAY, "grafana did not answer")
			}
		}
	}
}

/// Grafana holds no identity: each query a viewer's browser sends it has a token written into
/// it, standing for that viewer on GETs, until Grafana answers the query.
#[derive(Default)]
struct Viewers(Mutex<HashMap<String, Caller>>);

/// A [`Viewers`] token, forgotten when dropped.
struct Viewer<'a> {
	viewers: &'a Viewers,
	token: String,
}

impl Viewers {
	fn mint(&self, caller: Caller) -> Viewer<'_> {
		let mut raw = [0u8; 32];
		getrandom::fill(&mut raw).expect("the OS has randomness");
		let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
		let fresh = self.0.lock().expect("held for a lookup only").insert(token.clone(), caller).is_none();
		assert!(fresh, "256 random bits do not repeat");
		Viewer { viewers: self, token }
	}

	fn get(&self, token: &str) -> Option<Caller> {
		self.0.lock().expect("held for a lookup only").get(token).cloned()
	}
}

impl Drop for Viewer<'_> {
	fn drop(&mut self) {
		let minted = self.viewers.0.lock().expect("held for a lookup only").remove(&self.token);
		assert!(minted.is_some(), "only its Viewer removes a token");
	}
}

/// Grafana's process group: it and the plugin servers it starts, which outlive a Grafana killed
/// alone. Killed when dropped, however [`Grafana::run`] ends.
struct Group(rustix::process::Pid);

impl Group {
	fn signal(&self, signal: rustix::process::Signal) {
		match rustix::process::kill_process_group(self.0, signal) {
			Ok(()) | Err(rustix::io::Errno::SRCH) => {} // SRCH: every one of them has exited
			Err(e) => panic!("signalling our own child's group {:?}: {e}", self.0),
		}
	}
}

impl Drop for Group {
	fn drop(&mut self) {
		self.signal(rustix::process::Signal::KILL);
	}
}

/// Connects to Grafana's socket, whatever the URI.
#[derive(Clone)]
struct Socket(Arc<Path>);

impl tower::Service<Uri> for Socket {
	type Error = std::io::Error;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
	type Response = TokioIo<tokio::net::UnixStream>;

	fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, _: Uri) -> Self::Future {
		let path = self.0.clone();
		Box::pin(async move { Ok(TokioIo::new(tokio::net::UnixStream::connect(&*path).await?)) })
	}
}

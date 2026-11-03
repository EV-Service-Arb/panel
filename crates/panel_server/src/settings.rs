//! The environment: secrets and the deployment profile. Where to listen is a flag.

use panel::{gmbs::Places, seal::DataKey};
use panel_core::{notify::Locale, tokens::Renewal};
use sa_auth::{Catalog, PermissionSet};

ev_lib::settings! {
	/// Each secret is needed only by what uses it; a missing one fails that, with an error
	/// naming it, rather than the boot — except in production, where the only thing this
	/// binary runs is `serve`, which needs both.
	pub struct Settings {
		/// The panel's SQLite file, e.g. `/data/panel.db` (the image's, on the pod's volume,
		/// replicated by litestream). Created if missing and migrated on open, by whichever
		/// command opens it. A path, not a secret.
		#[required_in("production")]
		panel_db_path: Option<String>,
		/// 64 hex characters: the key PII and the sources' HMAC secrets are sealed with
		/// (`panel gen-data-key` makes one). Losing it loses both; changing it strands them.
		#[secret]
		#[required_in("production")]
		panel_data_key: Option<String>,
		/// Unset: errors are logged, not reported.
		#[secret]
		sentry_dsn: Option<String>,
		/// Where browsers reach the panel, e.g. `https://sa.evinvest.ltd`. The sign-in's
		/// redirect_uri is `<this>/auth/callback`, registered at concierge byte for byte; an
		/// `https` origin makes the cookies `__Host-` and `Secure`.
		#[required_in("production")]
		panel_public_origin: Option<String>,
		/// Where browsers reach concierge's `/api/auth/authorize`, e.g. `https://evinvest.ltd`.
		#[required_in("production")]
		concierge_public_origin: Option<String>,
		/// concierge's gRPC, from inside the cluster, e.g. `http://concierge:55670`.
		#[required_in("production")]
		concierge_grpc_addr: Option<String>,
		/// The panel's client secret at concierge (client id `sa`); concierge holds its hash.
		#[secret]
		#[required_in("production")]
		rp_client_secret_sa: Option<String>,
		/// Unix seconds of the commit the binary was built from (the image sets it): the
		/// version of the `sa` catalog `serve` publishes to concierge. Needed with the sign-in.
		panel_build_epoch: Option<u64>,
		/// The conductor's Ed25519 key for the assertions it signs to the services behind it,
		/// `<kid>:<base64 seed>` (`sa_auth`). With the three URLs below, or none of them.
		#[secret]
		#[required_in("production")]
		panel_assertion_key: Option<String>,
		/// review_archive, from inside the cluster, e.g. `http://review-archive:59110`.
		#[required_in("production")]
		panel_review_archive_url: Option<String>,
		/// playbook, from inside the cluster, e.g. `http://playbook-web.personal:59082`.
		#[required_in("production")]
		panel_playbook_url: Option<String>,
		/// sites_panel, from inside the cluster, e.g. `http://sites-panel:59121`: ingest, `/api/sites`,
		/// and the hubs' feeds.
		#[required_in("production")]
		panel_sites_url: Option<String>,
		/// `1` lets the pages load the platform's MFE bundles from `https://evinvest.ltd`
		/// (their directories in `script-src`, the origin in `connect-src`); `0` or unset, the
		/// policy is as before. The front end always loads them from there, so production sets `1`.
		panel_csp_platform_mfe: Option<String>,
		/// The conductor's bot (`@evinvest_sa_bot`; not `telegram_token_main`). Unset: no Telegram
		/// notifications, and `serve` says so. Needs the forward configured too: sites' notes
		/// are what it sends.
		#[secret]
		telegram_bot_token: Option<String>,
		/// The bot's username, for the `t.me/<bot>` links. Unset: asked of `getMe` at start.
		telegram_bot_username: Option<String>,
		/// The language of the bot's messages: `ru` or `en`.
		telegram_locale: String = "ru",
		/// Tokens added to a balance per day, while it is under TOKENS_CAP.
		tokens_daily: i64 = "15",
		/// Where renewal stops; tokens added or granted above it stay.
		tokens_cap: i64 = "500",
		/// A person's first balance, in TOKENS_CAPs (e.g. `1`). Needed by every command but
		/// `migrate`: there is no first balance by default.
		#[required_in("production")]
		tokens_start_cap_multiple: Option<f64>,
		/// The Places API key GMBs are named and addressed with. Unset: a GMB is added by its
		/// place id alone, unnamed.
		#[secret]
		google_maps_key: Option<String>,
		/// Grafana's server (the image's `grafanaServer` wrapper): `serve` runs it, at `/grafana/`.
		/// With PANEL_GRAFANA_DIR, or neither; needs the forward and a public origin.
		panel_grafana_server: Option<String>,
		/// Grafana's directory, e.g. `/data/grafana`: its socket, its data, its provisioning and
		/// the services' dashboards.
		panel_grafana_dir: Option<String>,
		/// The front end's static export (`frontend/` built, its `out/`), served on the same
		/// origin behind the API's routes; the image sets it. Unset: `serve` answers the API
		/// alone, and says so.
		panel_web_dir: Option<String>,
		/// Development only: an alias (`sa:operator`, `sa:admin`), a comma-separated list of
		/// `sa` permissions, or `none`. Signs whoever opens `/auth/login` in as a made-up user
		/// holding those, without concierge. Refused at start in any profile but
		/// `development`, beside any concierge variable, and unless PANEL_PUBLIC_ORIGIN is
		/// `http://localhost[:port]` or `http://127.0.0.1[:port]`.
		panel_dev_sign_in: Option<String>,
		/// The dev user's email, whichever permissions `PANEL_DEV_SIGN_IN` gives them.
		panel_dev_sign_in_email: Option<String>,
		app_env: String = "development",
	}
}

impl Settings {
	pub fn db_path(&self) -> eyre::Result<&std::path::Path> {
		let path = self.panel_db_path.as_deref().map(str::trim).filter(|p| !p.is_empty());
		Ok(std::path::Path::new(path.ok_or_else(|| eyre::eyre!("PANEL_DB_PATH must be set"))?))
	}

	pub fn data_key(&self) -> eyre::Result<DataKey> {
		let hex = self.panel_data_key.as_deref().ok_or_else(|| eyre::eyre!("PANEL_DATA_KEY must be set"))?;
		Ok(DataKey::from_hex(hex)?)
	}

	pub fn renewal(&self) -> eyre::Result<Renewal> {
		let multiple = self.tokens_start_cap_multiple.ok_or_else(|| eyre::eyre!("TOKENS_START_CAP_MULTIPLE must be set"))?;
		Ok(Renewal::new(self.tokens_daily, self.tokens_cap, multiple)?)
	}

	pub fn places(&self) -> eyre::Result<Places> {
		Places::new(self.google_maps_key.as_deref().map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned))
	}
}

/// The Telegram bot, when it is configured.
pub struct TelegramSettings {
	pub token: String,
	pub username: Option<String>,
	pub locale: Locale,
}

impl std::fmt::Debug for TelegramSettings {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("TelegramSettings")
			.field("username", &self.username)
			.field("locale", &self.locale)
			.finish_non_exhaustive()
	}
}

impl Settings {
	/// The front end's files, when `PANEL_WEB_DIR` names them.
	pub fn web(&self) -> eyre::Result<Option<panel_server::web::Files>> {
		let Some(dir) = self.panel_web_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) else {
			return Ok(None);
		};
		Ok(Some(panel_server::web::Files::new(std::path::Path::new(dir))?.platform_mfe(self.csp_platform_mfe()?)))
	}

	/// `PANEL_CSP_PLATFORM_MFE`: `0` (also unset or blank) or `1`; anything else fails the boot
	/// rather than silently leaving the policy narrower or wider than meant.
	pub fn csp_platform_mfe(&self) -> eyre::Result<bool> {
		match self.panel_csp_platform_mfe.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
			None | Some("0") => Ok(false),
			Some("1") => Ok(true),
			Some(other) => eyre::bail!("PANEL_CSP_PLATFORM_MFE must be `0` or `1`, not `{other}`"),
		}
	}

	/// `None` without `TELEGRAM_BOT_TOKEN`: the bot is off.
	pub fn telegram(&self) -> eyre::Result<Option<TelegramSettings>> {
		let Some(token) = self.telegram_bot_token.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
			return Ok(None);
		};
		Ok(Some(TelegramSettings {
			token: token.to_owned(),
			username: self.telegram_bot_username.as_deref().map(str::trim).filter(|u| !u.is_empty()).map(str::to_owned),
			locale: self.telegram_locale.parse()?,
		}))
	}
}

/// What signing in needs, all of it or none.
pub struct SignInSettings {
	pub panel_origin: String,
	pub concierge_origin: String,
	pub concierge_grpc: String,
	pub client_secret: String,
	/// The version of the catalog published at boot.
	pub build_epoch: u64,
}

impl std::fmt::Debug for SignInSettings {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SignInSettings")
			.field("panel_origin", &self.panel_origin)
			.field("concierge_origin", &self.concierge_origin)
			.field("concierge_grpc", &self.concierge_grpc)
			.field("build_epoch", &self.build_epoch)
			.finish_non_exhaustive()
	}
}

impl Settings {
	/// `None` when none of the sign-in variables is set: `serve` then answers ingest alone.
	/// Some but not all is a mistake, named.
	pub fn sign_in(&self) -> eyre::Result<Option<SignInSettings>> {
		let vars = [
			("PANEL_PUBLIC_ORIGIN", &self.panel_public_origin),
			("CONCIERGE_PUBLIC_ORIGIN", &self.concierge_public_origin),
			("CONCIERGE_GRPC_ADDR", &self.concierge_grpc_addr),
			("RP_CLIENT_SECRET_SA", &self.rp_client_secret_sa),
		];
		let set = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
		let missing: Vec<&str> = vars.iter().filter(|(_, v)| set(v).is_none()).map(|(name, _)| *name).collect();
		if missing.len() == vars.len() {
			return Ok(None);
		}
		eyre::ensure!(missing.is_empty(), "signing in needs {} too", missing.join(", "));
		let build_epoch = self
			.panel_build_epoch
			.ok_or_else(|| eyre::eyre!("signing in needs PANEL_BUILD_EPOCH too: the unix seconds of the build's commit, the version of the catalog published to concierge"))?;
		let get = |v: &Option<String>| set(v).unwrap_or_default();
		if self.app_env == "production" {
			bare_https_origin("PANEL_PUBLIC_ORIGIN", &get(&self.panel_public_origin))?;
			bare_https_origin("CONCIERGE_PUBLIC_ORIGIN", &get(&self.concierge_public_origin))?;
		}
		Ok(Some(SignInSettings {
			panel_origin: get(&self.panel_public_origin),
			concierge_origin: get(&self.concierge_public_origin),
			concierge_grpc: get(&self.concierge_grpc_addr),
			client_secret: get(&self.rp_client_secret_sa),
			build_epoch,
		}))
	}
}

impl Settings {
	/// The services behind the conductor (docs/ARCHITECTURE.md, Forward): `None` when none of their
	/// variables is set; some but not all is a mistake, named.
	pub fn forward(&self) -> eyre::Result<Option<panel_server::forward::Upstreams>> {
		let vars = [
			("PANEL_ASSERTION_KEY", &self.panel_assertion_key),
			("PANEL_REVIEW_ARCHIVE_URL", &self.panel_review_archive_url),
			("PANEL_PLAYBOOK_URL", &self.panel_playbook_url),
			("PANEL_SITES_URL", &self.panel_sites_url),
		];
		let missing: Vec<&str> = vars.iter().filter(|(_, v)| v.as_deref().is_none_or(|v| v.trim().is_empty())).map(|(name, _)| *name).collect();
		if missing.len() == vars.len() {
			return Ok(None);
		}
		eyre::ensure!(missing.is_empty(), "the forward needs {} too", missing.join(", "));
		let uri = |name: &str, v: &Option<String>| bare_http_origin(name, v.as_deref().expect("checked above"));
		Ok(Some(panel_server::forward::Upstreams {
			review_archive: uri("PANEL_REVIEW_ARCHIVE_URL", &self.panel_review_archive_url)?,
			playbook: uri("PANEL_PLAYBOOK_URL", &self.panel_playbook_url)?,
			sites: uri("PANEL_SITES_URL", &self.panel_sites_url)?,
			signer: self
				.panel_assertion_key
				.as_deref()
				.expect("checked above")
				.trim()
				.parse()
				.map_err(|e| eyre::eyre!("PANEL_ASSERTION_KEY: {e}"))?,
		}))
	}
}

/// Grafana, when it is configured.
pub struct GrafanaSettings {
	pub server: std::path::PathBuf,
	pub dir: std::path::PathBuf,
}

impl Settings {
	/// `None` when neither variable is set; one without the other is a mistake, named.
	pub fn grafana(&self) -> eyre::Result<Option<GrafanaSettings>> {
		let set = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
		match (set(&self.panel_grafana_server), set(&self.panel_grafana_dir)) {
			(None, None) => Ok(None),
			(Some(server), Some(dir)) => Ok(Some(GrafanaSettings { server, dir })),
			(Some(_), None) => eyre::bail!("PANEL_GRAFANA_SERVER needs PANEL_GRAFANA_DIR too"),
			(None, Some(_)) => eyre::bail!("PANEL_GRAFANA_DIR needs PANEL_GRAFANA_SERVER too"),
		}
	}
}

/// An in-cluster peer's address: `http://host[:port]`, nothing after it.
fn bare_http_origin(name: &str, raw: &str) -> eyre::Result<axum::http::Uri> {
	let raw = raw.trim();
	let uri: axum::http::Uri = raw.parse().map_err(|e| eyre::eyre!("{name} is not a URL: {e}"))?;
	eyre::ensure!(
		uri.scheme_str() == Some("http") && uri.path() == "/" && uri.query().is_none(),
		"{name} must be a bare http:// origin, not {raw}"
	);
	Ok(uri)
}

/// `PANEL_DEV_SIGN_IN`, checked: who everyone signs in as, and where.
#[derive(Debug)]
pub struct DevSignIn {
	pub permissions: PermissionSet,
	pub email: String,
	pub panel_origin: String,
}

impl Settings {
	/// `None` without `PANEL_DEV_SIGN_IN`. With it, refused unless this is development on the
	/// developer's own machine — the way concierge refuses `RP_DEV_REDIRECT_URIS`: anyone who
	/// reaches `/auth/login` is let in, so it must never be reachable from anywhere else.
	pub fn dev_sign_in(&self) -> eyre::Result<Option<DevSignIn>> {
		let set = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
		let Some(who) = set(&self.panel_dev_sign_in) else {
			return Ok(None);
		};
		eyre::ensure!(
			self.app_env == ev_lib::settings::DEFAULT_PROFILE,
			"PANEL_DEV_SIGN_IN must never be set outside development (APP_ENV={}): it signs anyone in without concierge",
			self.app_env
		);
		let concierge: Vec<&str> = [
			("CONCIERGE_PUBLIC_ORIGIN", &self.concierge_public_origin),
			("CONCIERGE_GRPC_ADDR", &self.concierge_grpc_addr),
			("RP_CLIENT_SECRET_SA", &self.rp_client_secret_sa),
		]
		.into_iter()
		.filter(|(_, v)| set(v).is_some())
		.map(|(name, _)| name)
		.collect();
		eyre::ensure!(concierge.is_empty(), "PANEL_DEV_SIGN_IN replaces the concierge sign-in: unset {}", concierge.join(", "));
		let permissions = dev_permissions(&who)?;
		let origin = set(&self.panel_public_origin).ok_or_else(|| eyre::eyre!("PANEL_DEV_SIGN_IN needs PANEL_PUBLIC_ORIGIN, e.g. http://$PANEL_BIND"))?;
		loopback_http_origin(&origin)?;
		let email = set(&self.panel_dev_sign_in_email).ok_or_else(|| eyre::eyre!("PANEL_DEV_SIGN_IN needs PANEL_DEV_SIGN_IN_EMAIL: whom it signs in as, whatever the permissions"))?;
		Ok(Some(DevSignIn {
			permissions,
			email,
			panel_origin: origin,
		}))
	}
}

/// `PANEL_DEV_SIGN_IN`'s set, resolved against the `sa` catalog.
fn dev_permissions(raw: &str) -> eyre::Result<PermissionSet> {
	if raw == "none" {
		return Ok(PermissionSet::from_iter(Vec::<String>::new()));
	}
	let catalog = Catalog::collect("sa", 0);
	if let Some(members) = catalog.aliases.get(raw) {
		return Ok(members.iter().cloned().collect());
	}
	let listed: Vec<&str> = raw.split(',').map(str::trim).collect();
	if let Some(unknown) = listed.iter().find(|p| !catalog.permissions.contains(**p)) {
		let aliases: Vec<&str> = catalog.aliases.keys().map(String::as_str).collect();
		eyre::bail!("PANEL_DEV_SIGN_IN: `{unknown}` is neither `none`, an alias ({}), nor an sa permission", aliases.join(", "));
	}
	Ok(listed.into_iter().collect())
}

/// `http://localhost[:port]` or `http://127.0.0.1[:port]`, nothing after it: an origin only
/// this machine's browser reaches.
fn loopback_http_origin(raw: &str) -> eyre::Result<()> {
	let refused = || eyre::eyre!("PANEL_DEV_SIGN_IN: PANEL_PUBLIC_ORIGIN must be http://localhost[:port] or http://127.0.0.1[:port], not {raw:?}");
	let url = url::Url::parse(raw).map_err(|_| refused())?;
	let loopback = matches!(url.host(), Some(url::Host::Domain("localhost") | url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)));
	let bare = url.path() == "/" && url.query().is_none() && url.fragment().is_none() && url.username().is_empty() && url.password().is_none();
	eyre::ensure!(url.scheme() == "http" && loopback && bare, refused());
	Ok(())
}

/// In production an origin the browser is sent to or told about is `https://host[:port]`,
/// nothing after it: plain http would drop the `__Host-`/`Secure` cookies, and a path would
/// end up inside the redirect URI concierge compares byte for byte.
fn bare_https_origin(var: &str, raw: &str) -> eyre::Result<()> {
	let url = url::Url::parse(raw).map_err(|e| eyre::eyre!("{var} is not a URL: {e}"))?;
	eyre::ensure!(url.scheme() == "https", "{var} must be https in production");
	eyre::ensure!(url.host_str().is_some_and(|h| !h.is_empty()), "{var} has no host");
	eyre::ensure!(
		url.path() == "/" && url.query().is_none() && url.fragment().is_none() && url.username().is_empty() && url.password().is_none(),
		"{var} must be an origin alone, e.g. https://sa.evinvest.ltd"
	);
	Ok(())
}

/// `--print-required-vars[=PROFILE]` (default `production`): the variables a deploy into
/// that profile must provide, one per line, for the gitops preflight. Read before clap,
/// which would otherwise demand a subcommand.
pub fn print_required_vars_for() -> Option<String> {
	const FLAG: &str = "--print-required-vars";
	let mut args = std::env::args().skip(1);
	let arg = args.next()?;
	match arg.split_once('=') {
		Some((FLAG, profile)) => Some(profile.to_owned()),
		Some(_) => None,
		None if arg == FLAG => Some(args.next().unwrap_or_else(|| "production".to_owned())),
		None => None,
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use super::*;

	/// The env surface is the deploy contract (the image env and the cluster Secret use these
	/// names); a rename here must be deliberate.
	#[test]
	fn env_surface_matches_the_deploy_contract() {
		assert_eq!(
			Settings::var_names(),
			[
				"PANEL_DB_PATH",
				"PANEL_DATA_KEY",
				"SENTRY_DSN",
				"PANEL_PUBLIC_ORIGIN",
				"CONCIERGE_PUBLIC_ORIGIN",
				"CONCIERGE_GRPC_ADDR",
				"RP_CLIENT_SECRET_SA",
				"PANEL_BUILD_EPOCH",
				"PANEL_ASSERTION_KEY",
				"PANEL_REVIEW_ARCHIVE_URL",
				"PANEL_PLAYBOOK_URL",
				"PANEL_SITES_URL",
				"PANEL_CSP_PLATFORM_MFE",
				"TELEGRAM_BOT_TOKEN",
				"TELEGRAM_BOT_USERNAME",
				"TELEGRAM_LOCALE",
				"TOKENS_DAILY",
				"TOKENS_CAP",
				"TOKENS_START_CAP_MULTIPLE",
				"GOOGLE_MAPS_KEY",
				"PANEL_GRAFANA_SERVER",
				"PANEL_GRAFANA_DIR",
				"PANEL_WEB_DIR",
				"PANEL_DEV_SIGN_IN",
				"PANEL_DEV_SIGN_IN_EMAIL",
				"APP_ENV"
			]
		);
		assert_eq!(
			Settings::required_var_names("production"),
			[
				"PANEL_DB_PATH",
				"PANEL_DATA_KEY",
				"PANEL_PUBLIC_ORIGIN",
				"CONCIERGE_PUBLIC_ORIGIN",
				"CONCIERGE_GRPC_ADDR",
				"RP_CLIENT_SECRET_SA",
				"PANEL_ASSERTION_KEY",
				"PANEL_REVIEW_ARCHIVE_URL",
				"PANEL_PLAYBOOK_URL",
				"PANEL_SITES_URL",
				"TOKENS_START_CAP_MULTIPLE"
			]
		);
		assert!(Settings::required_var_names("development").is_empty());
	}

	fn from(vars: &[(&str, &str)]) -> Result<Settings, ev_lib::settings::SettingsError> {
		let map: HashMap<String, String> = vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
		Settings::from_source(|k| map.get(k).cloned())
	}

	#[test]
	fn secrets_are_lazy_and_never_print() {
		let s = from(&[]).unwrap();
		assert_eq!(format!("{:#}", s.data_key().unwrap_err()), "PANEL_DATA_KEY must be set");
		assert!(from(&[("PANEL_DATA_KEY", "short")]).unwrap().data_key().is_err());
		assert!(from(&[("APP_ENV", "production")]).is_err());
		let key = "ab".repeat(32);
		let s = from(&[("PANEL_DATA_KEY", &key), ("PANEL_DB_PATH", "/data/panel.db")]).unwrap();
		assert!(s.data_key().is_ok());
		assert_eq!(s.db_path().unwrap(), std::path::Path::new("/data/panel.db"));
		assert_eq!(format!("{}", from(&[]).unwrap().db_path().unwrap_err()), "PANEL_DB_PATH must be set");
		let shown = format!("{s:?}");
		assert!(!shown.contains(&key), "{shown}");
	}

	#[test]
	fn sign_in_is_all_or_nothing() {
		assert!(from(&[]).unwrap().sign_in().unwrap().is_none());
		let partial = from(&[("PANEL_PUBLIC_ORIGIN", "https://sa.evinvest.ltd")]).unwrap().sign_in().unwrap_err();
		assert_eq!(format!("{partial}"), "signing in needs CONCIERGE_PUBLIC_ORIGIN, CONCIERGE_GRPC_ADDR, RP_CLIENT_SECRET_SA too");
		let secret = "s".repeat(40);
		let full = from(&[
			("PANEL_PUBLIC_ORIGIN", "https://sa.evinvest.ltd"),
			("CONCIERGE_PUBLIC_ORIGIN", "https://evinvest.ltd"),
			("CONCIERGE_GRPC_ADDR", "http://concierge:55670"),
			("RP_CLIENT_SECRET_SA", &secret),
		])
		.unwrap();
		let e = format!("{}", full.sign_in().unwrap_err());
		assert!(e.contains("PANEL_BUILD_EPOCH"), "{e}");
		let full = from(&[
			("PANEL_PUBLIC_ORIGIN", "https://sa.evinvest.ltd"),
			("CONCIERGE_PUBLIC_ORIGIN", "https://evinvest.ltd"),
			("CONCIERGE_GRPC_ADDR", "http://concierge:55670"),
			("RP_CLIENT_SECRET_SA", &secret),
			("PANEL_BUILD_EPOCH", "1791100000"),
		])
		.unwrap();
		assert_eq!(full.sign_in().unwrap().unwrap().build_epoch, 1_791_100_000);
		assert_eq!(full.sign_in().unwrap().unwrap().concierge_grpc, "http://concierge:55670");
		assert!(!format!("{full:?}").contains(&secret), "the client secret never prints");
	}

	#[test]
	fn telegram_is_off_without_a_token() {
		assert!(from(&[]).unwrap().telegram().unwrap().is_none());
		let token = "123456:secret-bot-token";
		let on = from(&[("TELEGRAM_BOT_TOKEN", token), ("TELEGRAM_BOT_USERNAME", "evinvest_sa_bot")]).unwrap();
		let tg = on.telegram().unwrap().unwrap();
		assert_eq!((tg.username.as_deref(), tg.locale), (Some("evinvest_sa_bot"), Locale::Ru));
		assert!(!format!("{on:?} {tg:?}").contains(token), "the token never prints");
		assert_eq!(
			from(&[("TELEGRAM_BOT_TOKEN", token), ("TELEGRAM_LOCALE", "en")]).unwrap().telegram().unwrap().unwrap().locale,
			Locale::En
		);
		assert!(from(&[("TELEGRAM_BOT_TOKEN", token), ("TELEGRAM_LOCALE", "de")]).unwrap().telegram().is_err());
	}

	#[test]
	fn production_origins_are_bare_https() {
		let key = "0".repeat(64);
		let secret = "s".repeat(40);
		let with = |panel: &str, concierge: &str| {
			from(&[
				("APP_ENV", "production"),
				("PANEL_DB_PATH", "/data/panel.db"),
				("PANEL_DATA_KEY", &key),
				("PANEL_PUBLIC_ORIGIN", panel),
				("CONCIERGE_PUBLIC_ORIGIN", concierge),
				("CONCIERGE_GRPC_ADDR", "http://concierge:55670"),
				("RP_CLIENT_SECRET_SA", &secret),
				("PANEL_BUILD_EPOCH", "1791100000"),
				("PANEL_ASSERTION_KEY", "k1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="),
				("PANEL_REVIEW_ARCHIVE_URL", "http://review-archive:59110"),
				("PANEL_PLAYBOOK_URL", "http://playbook-web.personal:59082"),
				("PANEL_SITES_URL", "http://sites-panel:59121"),
				("TOKENS_START_CAP_MULTIPLE", "1"),
			])
			.unwrap()
			.sign_in()
		};
		assert!(with("https://sa.evinvest.ltd", "https://evinvest.ltd/").is_ok());
		for (panel, why) in [
			("http://sa.evinvest.ltd", "must be https"),
			("https://sa.evinvest.ltd/panel", "an origin alone"),
			("https://sa.evinvest.ltd/?a=1", "an origin alone"),
			("https://sa.evinvest.ltd/#x", "an origin alone"),
			("https://u:p@sa.evinvest.ltd", "an origin alone"),
			("sa.evinvest.ltd", "not a URL"),
		] {
			let e = format!("{}", with(panel, "https://evinvest.ltd").unwrap_err());
			assert!(e.contains("PANEL_PUBLIC_ORIGIN") && e.contains(why), "{panel}: {e}");
		}
		let e = format!("{}", with("https://sa.evinvest.ltd", "http://evinvest.ltd").unwrap_err());
		assert!(e.contains("CONCIERGE_PUBLIC_ORIGIN"), "{e}");
		assert!(
			from(&[
				("PANEL_PUBLIC_ORIGIN", "http://localhost:59120"),
				("CONCIERGE_PUBLIC_ORIGIN", "http://localhost:3000"),
				("CONCIERGE_GRPC_ADDR", "http://localhost:55670"),
				("RP_CLIENT_SECRET_SA", &secret),
				("PANEL_BUILD_EPOCH", "1791100000"),
				("PANEL_ASSERTION_KEY", "k1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="),
				("PANEL_REVIEW_ARCHIVE_URL", "http://review-archive:59110"),
				("PANEL_PLAYBOOK_URL", "http://playbook-web.personal:59082"),
				("PANEL_SITES_URL", "http://sites-panel:59121"),
			])
			.unwrap()
			.sign_in()
			.is_ok(),
			"plain http is for development"
		);
	}

	#[test]
	fn dev_sign_in_is_development_on_loopback_only() {
		assert!(from(&[]).unwrap().dev_sign_in().unwrap().is_none());
		let dev = |vars: &[(&str, &str)]| {
			let mut vars = vars.to_vec();
			if !vars.iter().any(|(k, _)| *k == "PANEL_DEV_SIGN_IN_EMAIL") {
				vars.push(("PANEL_DEV_SIGN_IN_EMAIL", "dev@localhost"));
			}
			from(&vars).unwrap().dev_sign_in()
		};
		let on = dev(&[("PANEL_DEV_SIGN_IN", "sa:admin"), ("PANEL_PUBLIC_ORIGIN", "http://127.0.0.1:59120")]).unwrap().unwrap();
		assert_eq!(on.email, "dev@localhost");
		let e = format!(
			"{}",
			from(&[("PANEL_DEV_SIGN_IN", "sa:admin"), ("PANEL_PUBLIC_ORIGIN", "http://localhost")])
				.unwrap()
				.dev_sign_in()
				.unwrap_err()
		);
		assert!(e.contains("needs PANEL_DEV_SIGN_IN_EMAIL"), "no email of its own per permission set: {e}");
		let op = dev(&[
			("PANEL_DEV_SIGN_IN", "sa:operator"),
			("PANEL_DEV_SIGN_IN_EMAIL", "ann@example.com"),
			("PANEL_PUBLIC_ORIGIN", "http://localhost:3120"),
		])
		.unwrap()
		.unwrap();
		assert_eq!(op.email, "ann@example.com");
		assert_eq!(op.permissions, sa_auth::SA_OPERATOR.members.iter().copied().collect());
		let listed = dev(&[("PANEL_DEV_SIGN_IN", "sa:sites:work:read, sa:sites:work:leads:edit"), ("PANEL_PUBLIC_ORIGIN", "http://localhost")])
			.unwrap()
			.unwrap();
		assert_eq!(listed.permissions, ["sa:sites:work:leads:edit", "sa:sites:work:read"].into_iter().collect());
		assert!(dev(&[("PANEL_DEV_SIGN_IN", "sa:admin"), ("PANEL_PUBLIC_ORIGIN", "http://localhost")]).is_ok(), "port optional");
		let e = format!(
			"{}",
			dev(&[("PANEL_DEV_SIGN_IN", "sa:sites:work:read,sa:sites:work:nope"), ("PANEL_PUBLIC_ORIGIN", "http://localhost")]).unwrap_err()
		);
		assert!(e.contains("`sa:sites:work:nope`"), "the unknown one named: {e}");
		assert!(dev(&[("PANEL_DEV_SIGN_IN", "admin"), ("PANEL_PUBLIC_ORIGIN", "http://localhost")]).is_err(), "the roles are gone");
		let none = dev(&[("PANEL_DEV_SIGN_IN", "none"), ("PANEL_PUBLIC_ORIGIN", "http://localhost")]).unwrap().unwrap();
		assert_eq!(none.permissions.iter().count(), 0);

		for origin in [
			"https://sa.evinvest.ltd",
			"http://sa.evinvest.ltd",
			"https://localhost:59120",
			"http://localhost.evil.example:59120",
			"http://127.0.0.1.nip.io:59120",
			"http://10.0.0.5:59120",
			"http://0.0.0.0:59120",
			"http://127.0.0.1:59120/panel",
			"http://u:p@127.0.0.1:59120",
			"127.0.0.1:59120",
		] {
			let e = format!("{}", dev(&[("PANEL_DEV_SIGN_IN", "sa:admin"), ("PANEL_PUBLIC_ORIGIN", origin)]).unwrap_err());
			assert!(e.contains("PANEL_PUBLIC_ORIGIN must be http://localhost"), "{origin}: {e}");
		}
		let e = format!("{}", dev(&[("PANEL_DEV_SIGN_IN", "sa:admin")]).unwrap_err());
		assert!(e.contains("needs PANEL_PUBLIC_ORIGIN"), "{e}");
		let e = format!(
			"{}",
			dev(&[
				("PANEL_DEV_SIGN_IN", "sa:admin"),
				("PANEL_PUBLIC_ORIGIN", "http://127.0.0.1:59120"),
				("CONCIERGE_GRPC_ADDR", "http://localhost:55670")
			])
			.unwrap_err()
		);
		assert!(e.contains("unset CONCIERGE_GRPC_ADDR"), "{e}");
	}

	#[test]
	fn the_platform_csp_is_off_unless_asked_and_a_typo_fails_the_boot() {
		assert!(!from(&[]).unwrap().csp_platform_mfe().unwrap());
		assert!(!from(&[("PANEL_CSP_PLATFORM_MFE", " ")]).unwrap().csp_platform_mfe().unwrap());
		assert!(!from(&[("PANEL_CSP_PLATFORM_MFE", "0")]).unwrap().csp_platform_mfe().unwrap());
		assert!(from(&[("PANEL_CSP_PLATFORM_MFE", "1")]).unwrap().csp_platform_mfe().unwrap());
		let e = format!("{}", from(&[("PANEL_CSP_PLATFORM_MFE", "true")]).unwrap().csp_platform_mfe().unwrap_err());
		assert!(e.contains("PANEL_CSP_PLATFORM_MFE") && e.contains("true"), "{e}");
	}

	#[test]
	fn dev_sign_in_is_refused_in_production() {
		let key = "0".repeat(64);
		let secret = "s".repeat(40);
		let prod = |profile: &str| {
			from(&[
				("APP_ENV", profile),
				("PANEL_DB_PATH", "/data/panel.db"),
				("PANEL_DATA_KEY", &key),
				("PANEL_PUBLIC_ORIGIN", "http://127.0.0.1:59120"),
				("CONCIERGE_PUBLIC_ORIGIN", "https://evinvest.ltd"),
				("CONCIERGE_GRPC_ADDR", "http://concierge:55670"),
				("RP_CLIENT_SECRET_SA", &secret),
				("PANEL_ASSERTION_KEY", "k1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="),
				("PANEL_REVIEW_ARCHIVE_URL", "http://review-archive:59110"),
				("PANEL_PLAYBOOK_URL", "http://playbook-web.personal:59082"),
				("PANEL_SITES_URL", "http://sites-panel:59121"),
				("TOKENS_START_CAP_MULTIPLE", "1"),
				("PANEL_DEV_SIGN_IN", "sa:admin"),
			])
			.unwrap()
			.dev_sign_in()
		};
		for profile in ["production", "staging"] {
			let e = format!("{}", prod(profile).unwrap_err());
			assert!(e.contains("must never be set outside development"), "{profile}: {e}");
		}
	}
}

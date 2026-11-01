//! Composition root: settings, error monitoring and logs, then the CLI over the engine's
//! `Panel`.

// `ev_lib::settings!` expands once per field and recurses; the settings outgrew the default 128.
#![recursion_limit = "256"]

mod settings;

use std::{collections::BTreeSet, net::SocketAddr};

use clap::{Parser, Subcommand};
use ev_lib::error_monitoring;
use eyre::WrapErr;
use jiff::Timestamp;
use panel::{Panel, seal::DataKey, store::Store, telegram::Notifier, tokens::Change};
use panel_core::ids::{BrandId, SiteOrigin};
use panel_server::{
	DEFAULT_BIND,
	concierge::{Concierge, DevIdentity},
	forward::{Forward, Services},
	grafana::Grafana,
	http, live,
	signin::{SignIn, SignInConfig},
	telegram::{self, BotApi, BotName},
	tokens,
	web::{self, Files},
};
use sa_auth::{Catalog, SourceKind};

use crate::settings::Settings;

#[derive(Parser)]
#[command(
	name = "panel",
	version,
	about = "The Service-Arb conductor: the sign-in, the source keys, the forward to the services behind it, and its hubs"
)]
struct Cli {
	#[command(subcommand)]
	cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
	/// Create the database at PANEL_DB_PATH if need be and apply the migrations it lacks, then
	/// exit. Every command does as much on open, `serve` included; this one does nothing else.
	Migrate,
	/// The HTTP API.
	Serve {
		#[arg(long, default_value = DEFAULT_BIND)]
		bind: SocketAddr,
	},
	/// The sources that may write events, and their keys.
	#[command(subcommand)]
	Source(SourceCmd),
	/// A person's gmails.
	#[command(subcommand)]
	Gmail(GmailCmd),
	/// The GMBs a gmail manages.
	#[command(subcommand)]
	Gmb(GmbCmd),
	/// A person's tokens, as an admin changes them.
	#[command(subcommand)]
	Tokens(TokensCmd),
	/// What another service held before the conductor did.
	#[command(subcommand)]
	Import(ImportCmd),
	/// Print a fresh PANEL_DATA_KEY.
	GenDataKey,
}

#[derive(Subcommand)]
enum SourceCmd {
	/// Register a source; prints its secret, once.
	Add {
		/// Lowercase slug, e.g. aquafix-site.
		key_id: String,
		/// site | review_archive | gbp | posthog | sheet | telephony | bot (panel and booking are
		/// written without a key)
		#[arg(long)]
		kind: String,
		/// A brand it may write for; repeat for several.
		#[arg(long = "brand", required = true)]
		brands: Vec<String>,
		/// A site's origin, where its landing answers, e.g. https://aquafix.top: no path. Sites only.
		#[arg(long)]
		origin: Option<String>,
	},
	List,
	/// Give a site key issued before origins its origin; set once, never changed.
	SetOrigin {
		key_id: String,
		origin: String,
	},
	/// Refuse its key from now on; what it wrote stays.
	Revoke {
		key_id: String,
	},
}

#[derive(Subcommand)]
enum GmailCmd {
	/// Add a gmail to a person (by the email they sign in with).
	Add { email: String, gmail: String },
}

#[derive(Subcommand)]
enum GmbCmd {
	/// Add a GMB under a gmail: a place id, or a Maps URL (a name in it needs GOOGLE_MAPS_KEY).
	Add { gmail_id: i64, place: String },
}

#[derive(Subcommand)]
enum TokensCmd {
	/// Set a person's balance to n (0 or more).
	Set {
		email: String,
		n: i64,
		/// The admin's email, recorded.
		#[arg(long)]
		by: String,
		#[arg(long)]
		note: Option<String>,
	},
	/// Add n (1 or more): a sale, its reference in the note.
	Add {
		email: String,
		n: i64,
		#[arg(long)]
		by: String,
		#[arg(long)]
		note: Option<String>,
	},
	/// Owe a person n (1 or more) until they claim it.
	Grant {
		email: String,
		n: i64,
		#[arg(long)]
		by: String,
		/// What it is for.
		#[arg(long)]
		note: String,
	},
}

#[derive(Subcommand)]
enum ImportCmd {
	/// review_archive's people, gmails, GMBs and tokens, as its scripts/export_members.sql
	/// prints them; opens tokens and GMBs. A fresh install imports an empty document.
	ReviewArchive { file: std::path::PathBuf },
}

// Sentry must be initialised before the async runtime starts, hence no #[tokio::main].
fn main() -> eyre::Result<()> {
	color_eyre::install()?;

	// What the contract's `provides` must equal (the flake's `checks.provided-paths`).
	if std::env::args().nth(1).as_deref() == Some("--print-provided-paths") {
		for path in panel_server::PROVIDED_PATHS {
			println!("{path}");
		}
		return Ok(());
	}
	// The deploy contract, straight out of the image: the gitops preflight diffs it with the
	// cluster Secret's keys, so a missing variable is caught before the rollout.
	if let Some(profile) = settings::print_required_vars_for() {
		for var in Settings::required_var_names(&profile) {
			println!("{var}");
		}
		return Ok(());
	}
	// Exits 78 (EX_CONFIG) on a bad environment, before anything else is built.
	let settings = ev_lib::settings::or_exit(Settings::from_env());
	// Refused like a bad environment, whatever the command: in production, or off loopback,
	// it would let anyone in.
	let dev_sign_in = match settings.dev_sign_in() {
		Ok(d) => d,
		Err(e) => {
			eprintln!("{e:#}");
			std::process::exit(ev_lib::settings::EX_CONFIG);
		}
	};

	// Held for the life of main: dropping it flushes. A no-op without SENTRY_DSN.
	let _sentry = error_monitoring::init(&error_monitoring::Config {
		dsn: settings.sentry_dsn.clone(),
		environment: settings.app_env.clone(),
		release: error_monitoring::release_name!().map(|r| r.into_owned()),
		// the same name OTEL uses, so an issue and its trace agree on the service
		service: std::env::var("OTEL_SERVICE_NAME").ok().filter(|s| !s.trim().is_empty()),
		traces_sample_rate: error_monitoring::Config::traces_sample_rate_for(&settings.app_env),
	});
	let cli = Cli::parse();
	init_tracing()?;

	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.wrap_err("building the tokio runtime")?
		.block_on(run(cli, settings, dev_sign_in))
}

/// Logs go to stderr, so a CLI command's stdout stays its output.
fn init_tracing() -> eyre::Result<()> {
	use tracing_subscriber::{EnvFilter, fmt, prelude::*};

	let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(option_env!("LOG_DIRECTIVES").unwrap_or("info")))?;
	tracing_subscriber::registry()
		.with(filter)
		.with(fmt::layer().with_writer(std::io::stderr))
		.with(error_monitoring::tracing_layer())
		.init();
	Ok(())
}

async fn run(cli: Cli, settings: Settings, dev_sign_in: Option<settings::DevSignIn>) -> eyre::Result<()> {
	let connect = || async { eyre::Ok(Panel::new(Store::open(settings.db_path()?).await?, settings.data_key()?, settings.renewal()?, settings.places()?)) };
	match cli.cmd {
		Cmd::Migrate => {
			Store::open(settings.db_path()?).await?.pool().close().await;
			Ok(())
		}
		Cmd::Serve { bind } => {
			let identity = match dev_sign_in {
				Some(dev) => Identity::Dev(dev),
				None =>
					Identity::Concierge(settings.sign_in()?.ok_or_else(|| {
						eyre::eyre!("serve needs the sign-in: PANEL_PUBLIC_ORIGIN, CONCIERGE_PUBLIC_ORIGIN, CONCIERGE_GRPC_ADDR, RP_CLIENT_SECRET_SA (or PANEL_DEV_SIGN_IN)")
					})?),
			};
			let forward = settings.forward()?.map(Services::new);
			let telegram = settings.telegram()?;
			eyre::ensure!(
				telegram.is_none() || forward.is_some(),
				"TELEGRAM_BOT_TOKEN needs the forward (PANEL_ASSERTION_KEY and the services' URLs): the bot sends sites' notes"
			);
			let grafana = settings.grafana()?;
			eyre::ensure!(
				grafana.is_none() || forward.is_some(),
				"PANEL_GRAFANA_SERVER needs the forward (PANEL_ASSERTION_KEY and the services' URLs): Grafana reads the services through it"
			);
			serve(connect().await?, identity, forward, telegram, grafana, settings.web()?, bind).await
		}
		Cmd::Source(cmd) => source(&connect().await?, cmd).await,
		Cmd::Gmail(GmailCmd::Add { email, gmail }) => {
			let panel = connect().await?;
			let owner = panel.user_by_email(&email).await?;
			let added = panel.add_gmail(owner, &gmail, Timestamp::now()).await??;
			println!("gmail {}: {}", added.id, added.gmail);
			Ok(())
		}
		Cmd::Gmb(GmbCmd::Add { gmail_id, place }) => {
			let added = connect().await?.add_gmb(None, gmail_id, &place, Timestamp::now()).await??;
			println!("GMB {}: {} {}", added.id, added.place_id, added.name.as_deref().unwrap_or("(unnamed: no GOOGLE_MAPS_KEY)"));
			Ok(())
		}
		Cmd::Tokens(cmd) => {
			let (email, by, change) = match cmd {
				TokensCmd::Set { email, n, by, note } => (email, by, Change::Set { n, note }),
				TokensCmd::Add { email, n, by, note } => (email, by, Change::Add { n, note }),
				TokensCmd::Grant { email, n, by, note } => (email, by, Change::Grant { n, note }),
			};
			let panel = connect().await?;
			let user = panel.user_by_email(&email).await?;
			let balance = panel.change_balance(user, change, &by, Timestamp::now()).await??;
			println!("{email}: {balance}");
			Ok(())
		}
		Cmd::Import(ImportCmd::ReviewArchive { file }) => {
			let raw = std::fs::read(&file).wrap_err_with(|| format!("reading {}", file.display()))?;
			let export = serde_json::from_slice(&raw).wrap_err_with(|| format!("{} is not review_archive's export", file.display()))?;
			connect().await?.import_review_archive(export).await
		}
		Cmd::GenDataKey => {
			println!("{}", DataKey::generate_hex()?);
			Ok(())
		}
	}
}

async fn source(panel: &Panel, cmd: SourceCmd) -> eyre::Result<()> {
	match cmd {
		SourceCmd::Add { key_id, kind, brands, origin } => {
			let kind: SourceKind = kind.parse()?;
			let brands = brands.iter().map(|b| BrandId::parse(b)).collect::<Result<BTreeSet<_>, _>>()?;
			let origin = origin.as_deref().map(SiteOrigin::parse).transpose()?;
			let Some(added) = panel.add_source(&key_id, kind, brands, origin).await? else {
				eyre::bail!("a source {key_id} exists already");
			};
			println!("key id: {}\nsecret: {}\n(shown once; the source signs with it)", added.key_id, *added.secret);
			Ok(())
		}
		SourceCmd::List => {
			for s in panel.store().sources().await? {
				let brands: Vec<&str> = s.grant.brands.iter().map(BrandId::as_str).collect();
				let state = s.revoked_at.map_or_else(|| "active".to_owned(), |at| format!("revoked {at}"));
				let origin = s.origin.as_ref().map_or("-", SiteOrigin::as_str);
				println!("{:<24} {:<14} {:<32} {origin:<32} {state}", s.grant.key_id, s.grant.kind, brands.join(","));
			}
			Ok(())
		}
		SourceCmd::SetOrigin { key_id, origin } => panel.set_site_origin(&key_id, &SiteOrigin::parse(&origin)?).await,
		SourceCmd::Revoke { key_id } => {
			eyre::ensure!(panel.revoke_source(&key_id).await?, "no active source {key_id}");
			Ok(())
		}
	}
}

/// Who signs people in.
enum Identity {
	Concierge(settings::SignInSettings),
	/// `PANEL_DEV_SIGN_IN`, already refused outside development and off loopback.
	Dev(settings::DevSignIn),
}

async fn serve(
	panel: Panel,
	identity: Identity,
	services: Option<Services>,
	telegram: Option<settings::TelegramSettings>,
	grafana: Option<settings::GrafanaSettings>,
	files: Option<Files>,
	bind: SocketAddr,
) -> eyre::Result<()> {
	let (concierge, config) = match identity {
		Identity::Concierge(s) => {
			tracing::info!(panel_origin = s.panel_origin, "signing in through concierge");
			let concierge = Concierge::new(&s.concierge_grpc, &s.client_secret)?;
			// Before serving: a user's permissions name what this catalog defines.
			concierge
				.publish_catalog(&Catalog::collect("sa", s.build_epoch))
				.await
				.wrap_err_with(|| format!("publishing the sa catalog (version {}) to concierge", s.build_epoch))?;
			tracing::info!(version = s.build_epoch, "sa catalog published to concierge");
			let config = SignInConfig {
				panel_origin: s.panel_origin,
				concierge_origin: s.concierge_origin,
			};
			(concierge, config)
		}
		Identity::Dev(d) => {
			let who = DevIdentity {
				permissions: d.permissions,
				email: d.email,
			};
			tracing::warn!(
				permissions = ?who.permissions,
				email = who.email,
				user_id = %who.user_id(),
				panel_origin = d.panel_origin,
				"DEV SIGN-IN ON (PANEL_DEV_SIGN_IN): /auth/login signs anyone in as this user, no concierge — development only"
			);
			let config = SignInConfig {
				concierge_origin: d.panel_origin.clone(),
				panel_origin: d.panel_origin,
			};
			(Concierge::dev(who), config)
		}
	};
	let (stop, stopped) = tokio::sync::watch::channel(false);
	let bus = panel.bus().clone();
	let bot = match (telegram, &services) {
		(Some(tg), Some(services)) => {
			let name = BotName::on(tg.username);
			let notifier = Notifier {
				panel: panel.clone(),
				bot: BotApi::new(telegram::API_BASE, &tg.token)?,
				concierge: concierge.clone(),
				sites: services.clone(),
				locale: tg.locale,
				cabinet: config.concierge_origin.parse().wrap_err("the concierge origin is not a URL")?,
			};
			tracing::info!("telegram notifications on");
			Some((notifier, name))
		}
		(Some(_), None) => unreachable!("refused before serve"),
		(None, _) => {
			tracing::warn!("TELEGRAM_BOT_TOKEN unset: Telegram notifications off");
			None
		}
	};
	let bot_name = bot.as_ref().map_or_else(BotName::off, |(_, name)| name.clone());
	let engine = panel.clone();
	let grafana_root = format!("{}/grafana/", config.panel_origin.trim_end_matches('/'));
	let sign_in = SignIn::new(panel, concierge, config);
	let forward = match &services {
		Some(services) => Some(Forward::new(sign_in.clone(), services.clone())),
		None => {
			tracing::warn!("forward not configured: no ingest, no /api/sites, /api/review_archive or /playbook_mcp, no live updates from sites");
			None
		}
	};
	let grafana = match (grafana, &forward) {
		(Some(g), Some(forward)) => {
			tracing::info!(root_url = grafana_root, "grafana at /grafana/");
			Some((Grafana::new(forward.clone(), g.dir), g.server))
		}
		(Some(_), None) => unreachable!("refused before serve"),
		(None, _) => {
			tracing::warn!("PANEL_GRAFANA_SERVER unset: no Grafana, /grafana is a 404");
			None
		}
	};
	let app = http::app(sign_in, forward, grafana.as_ref().map(|(g, _)| g), http::Limits::default(), bot_name);
	let app = match files {
		Some(files) => web::serve(app, files),
		None => {
			tracing::warn!("PANEL_WEB_DIR unset: serving the API alone, no front end");
			app
		}
	};
	let listener = tokio::net::TcpListener::bind(bind).await.wrap_err_with(|| format!("binding {bind}"))?;
	tracing::info!(%bind, "serving");
	let server = async {
		let served = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await.wrap_err("HTTP server");
		// The live sockets outlive the graceful shutdown (an upgraded connection is no longer the
		// server's): told to close, and given a moment for their close frames to go out.
		bus.publish(panel::live::Signal::GoingAway);
		bus.drained(std::time::Duration::from_secs(2)).await;
		// The hubs stop with the server; with none running nobody listens, which is fine.
		let _sent = stop.send(true);
		served
	};
	let hubs = async {
		let bot = async {
			if let Some((notifier, name)) = bot {
				telegram::run(notifier, name, stopped.clone()).await;
			}
		};
		let feed = async {
			if let Some(services) = &services {
				live::follow(services.clone(), bus.clone(), stopped.clone()).await;
			}
		};
		let tokens = async {
			if let Some(services) = &services {
				tokens::run(engine, services.clone(), stopped.clone()).await;
			}
		};
		let grafana = async {
			if let Some((grafana, server)) = grafana {
				grafana.run(&server, &grafana_root, stopped.clone()).await;
			}
		};
		tokio::join!(bot, feed, tokens, grafana);
	};
	let (served, ()) = tokio::join!(server, hubs);
	served
}

async fn shutdown_signal() {
	let ctrl_c = async {
		if let Err(e) = tokio::signal::ctrl_c().await {
			tracing::error!(error = %e, "listening for Ctrl-C");
			std::future::pending::<()>().await;
		}
	};
	#[cfg(unix)]
	let term = async {
		match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
			Ok(mut s) => {
				s.recv().await;
			}
			Err(e) => {
				tracing::error!(error = %e, "listening for SIGTERM");
				std::future::pending::<()>().await;
			}
		}
	};
	#[cfg(not(unix))]
	let term = std::future::pending::<()>();
	tokio::select! {
		() = ctrl_c => {}
		() = term => {}
	}
	tracing::info!("shutting down");
}

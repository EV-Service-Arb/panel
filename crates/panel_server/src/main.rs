//! Composition root: settings, error monitoring and logs, then the CLI over the engine's
//! `Panel`.

// `ev_lib::settings!` expands once per field and recurses; the settings outgrew the default 128.
#![recursion_limit = "256"]

mod settings;

use std::{collections::BTreeSet, net::SocketAddr, sync::Arc};

use clap::{Parser, Subcommand};
use ev_lib::error_monitoring;
use eyre::WrapErr;
use panel::{
	Panel,
	place::{Expected, PlaceError, PlaceView},
	pricing::{PricingError, PricingView},
	seal::DataKey,
	store::Store,
	telegram::Notifier,
};
use panel_core::{
	event::SourceKind,
	ids::{BrandId, LocationId},
	place::{self, Editor},
};
use panel_server::{
	DEFAULT_BIND,
	capture::{self, CaptureApi},
	concierge::{Concierge, DevIdentity},
	google_calendar, http,
	scope_ledger::{self, ScopeLedger},
	signin::{SignIn, SignInConfig},
	telegram::{self, BotApi, BotName},
	web::{self, Files},
};
use sa_auth::Catalog;

use crate::settings::Settings;

#[derive(Parser)]
#[command(name = "panel", version, about = "The Service-Arb panel: sa.funnel.v1 ingest, the event journal and the funnel projections")]
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
	/// Rebuild leads, calls and payments from the journal, judging every event against the
	/// registry as it is now.
	RebuildProjections,
	/// The sources that may write events, and their keys.
	#[command(subcommand)]
	Source(SourceCmd),
	/// A place's live settings, what the sites lay over their baked config.
	#[command(subcommand)]
	Place(PlaceCmd),
	/// A brand's pricing, what the sites price estimates and fixed jobs by.
	#[command(subcommand)]
	Pricing(PricingCmd),
	/// Booking: a provider's calendar, pulled.
	#[command(subcommand)]
	Booking(BookingCmd),
	/// Print a fresh PANEL_DATA_KEY.
	GenDataKey,
}

#[derive(Subcommand)]
enum BookingCmd {
	/// The brand owner's consent to read their Google Calendar, on this machine: prints the
	/// URL to open, waits for Google's redirect on 127.0.0.1, and prints the refresh token
	/// once — for the brand's `GOOGLE_CALENDAR_REFRESH_TOKEN_<BRAND>` secret.
	GoogleAuthorize {
		brand: String,
		/// The loopback port Google redirects to (a Desktop OAuth client takes any).
		#[arg(long, default_value_t = 8765)]
		port: u16,
	},
	/// Pull the brand's Google Calendar now and journal its bookings — what `serve` does every
	/// GOOGLE_CALENDAR_SYNC_MINUTES.
	GoogleSync {
		brand: String,
		/// From nothing (the last 7 days), whatever the sync token.
		#[arg(long)]
		full: bool,
	},
}

#[derive(Subcommand)]
enum PlaceCmd {
	/// Make a place known to the panel, as adding it on the Locations screen does; nothing
	/// changes for one it knows. Journaled as `by = cli`.
	Register { brand: String, slug: String },
	/// Set some fields, clear others; the rest stay. Journaled as `by = cli`.
	Set {
		brand: String,
		slug: String,
		/// E.164, e.g. +33612345678.
		#[arg(long)]
		phone: Option<String>,
		#[arg(long)]
		whatsapp: Option<String>,
		/// Rows split by commas: 'Mo-Fr 08:00-19:00,Sa 09:00-12:00'.
		#[arg(long)]
		hours: Option<String>,
		/// Commune names split by commas: 'Royat,Chamalières'.
		#[arg(long = "service-area")]
		service_area: Option<String>,
		/// The place's Telegram bot, its username without the @: aquafix_devis_bot.
		#[arg(long)]
		telegram: Option<String>,
		/// Messengers switched on or off, split by commas: 'whatsapp=on,telegram=off'.
		#[arg(long)]
		messengers: Option<String>,
		/// A field to clear (the site's baked value takes over); repeat for several.
		#[arg(long = "clear", value_name = "FIELD")]
		clear: Vec<String>,
	},
	/// The settings as the sites get them, and who set them when.
	Show { brand: String, slug: String },
	/// Every change, newest first.
	History { brand: String, slug: String },
	/// Put back the settings a change found; journaled as a change of its own.
	Revert { brand: String, slug: String, id: uuid::Uuid },
	/// Take the place off the sites: they answer it as gone (404).
	Withdraw { brand: String, slug: String },
	/// Put a withdrawn place back.
	Restore { brand: String, slug: String },
}

#[derive(Subcommand)]
enum PricingCmd {
	/// The model as saved, its locales, and who saved it when.
	Show { brand: String },
	/// The last changes, newest first.
	History { brand: String },
	/// Save a model (kitstart's JSON, from a file or `-` for stdin), checked as the editor's is.
	/// Journaled as `by = cli`.
	Set { brand: String, file: std::path::PathBuf },
	/// Take the model off the sites: they go back to their baked one. Journaled as `by = cli`.
	Remove { brand: String },
	/// The locales the brand's sites speak, split by commas (`fr,en`, the default): every label
	/// of its model must be in each.
	Locales { brand: String, locales: String },
}

#[derive(Subcommand)]
enum SourceCmd {
	/// Register a source; prints its secret, once.
	Add {
		/// Lowercase slug, e.g. aquafix-site.
		key_id: String,
		/// site | review_archive | gbp | posthog | sheet | telephony | bot (panel and booking are
		/// the panel's own, with no key; bot is a messenger bot, docs/BOT-API.md)
		#[arg(long)]
		kind: String,
		/// A brand it may write for; repeat for several.
		#[arg(long = "brand", required = true)]
		brands: Vec<String>,
	},
	List,
	/// Refuse its key from now on; what it wrote stays.
	Revoke {
		key_id: String,
	},
}

// Sentry must be initialised before the async runtime starts, hence no #[tokio::main].
fn main() -> eyre::Result<()> {
	color_eyre::install()?;

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
	let connect = || async { eyre::Ok(Panel::new(Store::open(settings.db_path()?).await?, settings.data_key()?).with_posthog_project(settings.posthog_project()?)) };
	match cli.cmd {
		Cmd::Migrate => {
			Store::open(settings.db_path()?).await?.pool().close().await;
			Ok(())
		}
		Cmd::Serve { bind } => {
			let sign_in = match dev_sign_in {
				Some(dev) => Some(Identity::Dev(dev)),
				None => settings.sign_in()?.map(Identity::Concierge),
			};
			let telegram = settings.telegram()?;
			let front_end = Edge { files: settings.web()? };
			let capture = settings.capture()?.map(|c| CaptureApi::new(&c.host, &c.key)).transpose()?;
			let google = google(&settings)?;
			let ledger = settings.scope_ledger()?.map(|c| ScopeLedger::connect(&c)).transpose()?.map(Arc::new);
			let sign_in = match (sign_in, settings.forward()?) {
				(Some(identity), forward) => Some((identity, forward)),
				(None, None) => None,
				(None, Some(_)) => eyre::bail!("the forward vouches for signed-in callers: it needs the sign-in configured"),
			};
			let panel = connect().await?.with_capture(capture.is_some());
			let peers = Peers { capture, google, ledger };
			serve(panel, sign_in, telegram, peers, front_end, bind).await
		}
		Cmd::RebuildProjections => {
			let r = connect().await?.rebuild_projections().await?;
			println!(
				"{} events: {} registered, {} unregistered, {} invalid; {} leads",
				r.events, r.registered, r.unregistered, r.invalid, r.leads
			);
			Ok(())
		}
		Cmd::Source(cmd) => source(&connect().await?, cmd).await,
		Cmd::Place(cmd) => place(&connect().await?, cmd).await,
		Cmd::Pricing(cmd) => pricing(&connect().await?, cmd).await,
		Cmd::Booking(BookingCmd::GoogleAuthorize { brand, port }) => {
			let brand = BrandId::parse(&brand)?;
			let (id, secret) = settings
				.google_client()?
				.ok_or_else(|| eyre::eyre!("GOOGLE_OAUTH_CLIENT_ID and GOOGLE_OAUTH_CLIENT_SECRET must be set"))?;
			let consent = google_calendar::authorize(&id, &secret, port, google_calendar::TOKEN_URL).await?;
			let var = format!("{}{}", settings::GOOGLE_REFRESH_PREFIX, brand.as_str().to_ascii_uppercase());
			// Shown once, here only: it goes into the brand's secret (devops add-secrets).
			println!("{var}={}", *consent.refresh_token);
			Ok(())
		}
		Cmd::Booking(BookingCmd::GoogleSync { brand, full }) => {
			let brand = BrandId::parse(&brand)?;
			let (api, every) = google(&settings)?.ok_or_else(|| eyre::eyre!("no Google calendar is configured: GOOGLE_OAUTH_CLIENT_* and a GOOGLE_CALENDAR_REFRESH_TOKEN_<BRAND>"))?;
			eyre::ensure!(
				api.brands().contains(&brand),
				"{brand} has no {}{}",
				settings::GOOGLE_REFRESH_PREFIX,
				brand.as_str().to_ascii_uppercase()
			);
			let done = connect()
				.await?
				.sync_bookings(&api, &brand, uuid::Uuid::now_v7(), jiff::Timestamp::now(), every, true, full)
				.await?
				.ok_or_else(|| eyre::eyre!("the sync was not leased"))?;
			let i = done.ingested;
			println!(
				"{}: {} written ({} matched, {} without a lead), {} duplicate, {} unchanged, {} ignored, {} refused",
				if done.full { "full pull" } else { "since the last pull" },
				i.written,
				i.matched,
				i.unmatched,
				i.duplicate,
				i.unchanged,
				i.ignored,
				i.refused
			);
			Ok(())
		}
		Cmd::GenDataKey => {
			println!("{}", DataKey::generate_hex()?);
			Ok(())
		}
	}
}

async fn source(panel: &Panel, cmd: SourceCmd) -> eyre::Result<()> {
	match cmd {
		SourceCmd::Add { key_id, kind, brands } => {
			let kind: SourceKind = kind.parse()?;
			let brands = brands.iter().map(|b| BrandId::parse(b)).collect::<Result<BTreeSet<_>, _>>()?;
			let Some(added) = panel.add_source(&key_id, kind, brands).await? else {
				eyre::bail!("a source {key_id} exists already");
			};
			println!("key id: {}\nsecret: {}\n(shown once; the source signs with it)", added.key_id, *added.secret);
			Ok(())
		}
		SourceCmd::List => {
			for s in panel.store().sources().await? {
				let brands: Vec<&str> = s.grant.brands.iter().map(BrandId::as_str).collect();
				let state = s.revoked_at.map_or_else(|| "active".to_owned(), |at| format!("revoked {at}"));
				println!("{:<24} {:<14} {:<32} {state}", s.grant.key_id, s.grant.kind, brands.join(","));
			}
			Ok(())
		}
		SourceCmd::Revoke { key_id } => {
			eyre::ensure!(panel.revoke_source(&key_id).await?, "no active source {key_id}");
			Ok(())
		}
	}
}

/// A place's change refused, said field by field.
fn refused(e: PlaceError) -> eyre::Report {
	match e {
		PlaceError::Invalid(fields) => {
			let lines: Vec<String> = fields.iter().map(|(field, why)| format!("  {field}: {why}")).collect();
			eyre::eyre!("invalid settings:\n{}", lines.join("\n"))
		}
		PlaceError::NotFound => eyre::eyre!("no such change of this place"),
		PlaceError::Conflict => eyre::eyre!("the settings changed meanwhile; run it again"),
		PlaceError::Internal(e) => e,
	}
}

fn print_place(v: &PlaceView) -> eyre::Result<()> {
	let state = if v.withdrawn { "withdrawn: the sites answer 404" } else { "live" };
	match (v.updated_at, &v.updated_by) {
		(Some(at), Some(by)) => println!("{}/{}  {state}; set {at} by {by}", v.brand, v.slug),
		_ => println!("{}/{}  {state}; never set, the sites serve their baked config", v.brand, v.slug),
	}
	println!("{}", serde_json::to_string_pretty(&v.settings.as_json())?);
	Ok(())
}

async fn place(panel: &Panel, cmd: PlaceCmd) -> eyre::Result<()> {
	let ids = |brand: &str, slug: &str| eyre::Ok((BrandId::parse(brand)?, LocationId::parse(slug)?));
	let now = jiff::Timestamp::now();
	let cli = Editor::Cli;
	let view = match cmd {
		PlaceCmd::Register { brand, slug } => {
			let (brand, slug) = ids(&brand, &slug)?;
			let (view, added) = panel.register_place(&cli, &brand, &slug, now).await?;
			if !added {
				eprintln!("{brand}/{slug} was known already: nothing changed");
			}
			view
		}
		PlaceCmd::Set {
			brand,
			slug,
			phone,
			whatsapp,
			hours,
			service_area,
			telegram,
			messengers,
			clear,
		} => {
			let (brand, slug) = ids(&brand, &slug)?;
			let mut set = serde_json::Map::new();
			if let Some(p) = phone {
				set.insert("phone".into(), p.into());
			}
			if let Some(w) = whatsapp {
				set.insert("whatsapp".into(), w.into());
			}
			if let Some(h) = hours {
				set.insert("hours".into(), place::hours_from_spec(&h).map_err(|e| eyre::eyre!("--hours {e}"))?);
			}
			if let Some(a) = service_area {
				set.insert("serviceArea".into(), a.split(',').map(str::trim).collect::<Vec<_>>().into());
			}
			if let Some(t) = telegram {
				set.insert("telegram".into(), t.into());
			}
			if let Some(m) = messengers {
				set.insert("messengers".into(), place::messengers_from_spec(&m).map_err(|e| eyre::eyre!("--messengers {e}"))?);
			}
			eyre::ensure!(!set.is_empty() || !clear.is_empty(), "nothing to set: give a field, or --clear one");
			panel.patch_place(&cli, &brand, &slug, set, &clear, now).await.map_err(refused)?
		}
		PlaceCmd::Show { brand, slug } => {
			let (brand, slug) = ids(&brand, &slug)?;
			panel.place(&brand, &slug).await?
		}
		PlaceCmd::History { brand, slug } => {
			let (brand, slug) = ids(&brand, &slug)?;
			for c in panel.place_history(&brand, &slug).await? {
				println!("{}  {:<8}  {:<24}  {}", c.at, c.kind.as_str(), c.by, c.id);
				println!("  before {}\n  after  {}", c.before.as_json(), c.after.as_json());
			}
			return Ok(());
		}
		PlaceCmd::Revert { brand, slug, id } => {
			let (brand, slug) = ids(&brand, &slug)?;
			panel.revert_place(&cli, &brand, &slug, id, Expected::Any, now).await.map_err(refused)?
		}
		PlaceCmd::Withdraw { brand, slug } => {
			let (brand, slug) = ids(&brand, &slug)?;
			panel.withdraw_place(&cli, &brand, &slug, true, now).await.map_err(refused)?
		}
		PlaceCmd::Restore { brand, slug } => {
			let (brand, slug) = ids(&brand, &slug)?;
			panel.withdraw_place(&cli, &brand, &slug, false, now).await.map_err(refused)?
		}
	};
	print_place(&view)
}

fn print_pricing(v: &PricingView) -> eyre::Result<()> {
	let locales = v.locales.join(",");
	match (v.updated_at, &v.updated_by, &v.model) {
		(Some(at), Some(by), Some(_)) => println!("{}  locales {locales}; saved {at} by {by}", v.brand),
		(Some(at), Some(by), None) => println!("{}  locales {locales}; removed {at} by {by}, the sites serve their baked model", v.brand),
		_ => println!("{}  locales {locales}; never set, the sites serve their baked model", v.brand),
	}
	if let Some(model) = &v.model {
		println!("{}", serde_json::to_string_pretty(model)?);
	}
	Ok(())
}

/// A brand's pricing change refused, said problem by problem.
fn refused_pricing(e: PricingError) -> eyre::Report {
	match e {
		PricingError::Invalid(problems) => {
			let lines: Vec<String> = problems.iter().map(|p| format!("  {p}")).collect();
			eyre::eyre!("invalid pricing model:\n{}", lines.join("\n"))
		}
		PricingError::Stale(_) => eyre::eyre!("the pricing changed meanwhile; run it again"),
		PricingError::Internal(e) => e,
	}
}

async fn pricing(panel: &Panel, cmd: PricingCmd) -> eyre::Result<()> {
	let now = jiff::Timestamp::now();
	let cli = Editor::Cli;
	let view = match cmd {
		PricingCmd::Show { brand } => panel.pricing(&BrandId::parse(&brand)?).await?,
		PricingCmd::History { brand } => {
			for c in panel.pricing_history(&BrandId::parse(&brand)?).await? {
				let what = match (&c.valid_from, c.needs) {
					(Some(from), Some(n)) => format!("valid from {from}, {n} needs"),
					_ => "removed".to_owned(),
				};
				println!("{}  {:<6}  {:<24}  {what}  {}", c.at, c.kind.as_str(), c.by, c.id);
			}
			return Ok(());
		}
		PricingCmd::Set { brand, file } => {
			let brand = BrandId::parse(&brand)?;
			let raw = if file.as_os_str() == "-" {
				std::io::read_to_string(std::io::stdin()).wrap_err("reading the model from stdin")?
			} else {
				std::fs::read_to_string(&file).wrap_err_with(|| format!("reading the model at {}", file.display()))?
			};
			let model: serde_json::Value = serde_json::from_str(&raw).wrap_err("the model is not JSON")?;
			panel.set_pricing(&cli, &brand, &model, Expected::Any, now).await.map_err(refused_pricing)?
		}
		PricingCmd::Remove { brand } => panel.remove_pricing(&cli, &BrandId::parse(&brand)?, Expected::Any, now).await.map_err(refused_pricing)?,
		PricingCmd::Locales { brand, locales } => {
			let locales = panel_core::pricing::parse_locales(locales.split(','))?;
			panel.set_brand_locales(&cli, &BrandId::parse(&brand)?, &locales, now).await?
		}
	};
	print_pricing(&view)
}

/// The Google Calendar adapter and how often it pulls, when configured.
fn google(settings: &Settings) -> eyre::Result<Option<(google_calendar::GoogleCalendar, jiff::SignedDuration)>> {
	let Some(g) = settings.google(std::env::vars())? else { return Ok(None) };
	let every = jiff::SignedDuration::from_mins(i64::from(g.every_minutes));
	let api = google_calendar::GoogleCalendar::new(google_calendar::TOKEN_URL, google_calendar::API_BASE, &g.client_id, &g.client_secret, g.calendars)?;
	Ok(Some((api, every)))
}

/// What the origin serves besides the panel's own API.
struct Edge {
	/// The front end's export (`PANEL_WEB_DIR`).
	files: Option<Files>,
}

/// The services `serve` calls out to, each when configured.
struct Peers {
	/// PostHog (`POSTHOG_*`).
	capture: Option<CaptureApi>,
	/// Google Calendar (`GOOGLE_*`), and how often it is pulled.
	google: Option<(google_calendar::GoogleCalendar, jiff::SignedDuration)>,
	/// banking's ScopeLedger (`SCOPE_LEDGER_*`): the invoices.
	ledger: Option<Arc<ScopeLedger>>,
}

/// Who signs people in.
enum Identity {
	Concierge(settings::SignInSettings),
	/// `PANEL_DEV_SIGN_IN`, already refused outside development and off loopback.
	Dev(settings::DevSignIn),
}

async fn serve(
	panel: Panel,
	sign_in: Option<(Identity, Option<panel_server::forward::Upstreams>)>,
	telegram: Option<settings::TelegramSettings>,
	peers: Peers,
	front_end: Edge,
	bind: SocketAddr,
) -> eyre::Result<()> {
	let sign_in = match sign_in {
		Some((Identity::Concierge(s), forward)) => {
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
			Some((concierge, config, forward))
		}
		Some((Identity::Dev(d), forward)) => {
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
			Some((Concierge::dev(who), config, forward))
		}
		None => None,
	};
	let (stop, stopped) = tokio::sync::watch::channel(false);
	let bus = panel.bus().clone();
	let mut bot_work = None;
	// Held, and awaited at shutdown, like the bot's.
	let capture_work = match peers.capture {
		Some(api) => {
			tracing::info!(?api, "sending lead events to posthog");
			Some(tokio::spawn(capture::run(panel.clone(), api, stopped.clone())))
		}
		None => {
			tracing::warn!("POSTHOG_PROJECT_API_KEY unset: nothing is sent to PostHog");
			None
		}
	};
	// Held, and awaited at shutdown, like the sender's.
	let booking_work = match peers.google {
		Some((api, every)) => {
			let brands: Vec<String> = api.brands().iter().map(|b| b.as_str().to_owned()).collect();
			tracing::info!(?brands, minutes = every.as_mins(), "google calendar booking pull on; brands without a refresh token are off");
			Some(tokio::spawn(google_calendar::run(panel.clone(), api, every, stopped.clone())))
		}
		None => {
			tracing::warn!("GOOGLE_OAUTH_CLIENT_* or every GOOGLE_CALENDAR_REFRESH_TOKEN_<BRAND> unset: no Google Calendar bookings pulled");
			None
		}
	};
	// Held, and awaited at shutdown, like the others.
	let invoice_work = match &peers.ledger {
		Some(ledger) => {
			tracing::info!(?ledger, "invoices on: issued through banking's ScopeLedger, payments confirmed from its money events");
			Some(tokio::spawn(scope_ledger::run(panel.clone(), ledger.clone(), stopped.clone())))
		}
		None => {
			tracing::warn!("SCOPE_LEDGER_* unset: no invoices, /api/v1/invoices answers 503");
			None
		}
	};
	let app = match sign_in {
		Some((concierge, config, forward)) => {
			let bot = match telegram {
				Some(tg) => {
					let name = BotName::on(tg.username);
					let notifier = Notifier {
						panel: panel.clone(),
						bot: BotApi::new(telegram::API_BASE, &tg.token)?,
						concierge: concierge.clone(),
						locale: tg.locale,
						cabinet: config.concierge_origin.parse().wrap_err("the concierge origin is not a URL")?,
					};
					tracing::info!("telegram notifications on");
					// Held, and awaited at shutdown: the poller gives its lease back on the way out.
					bot_work = Some(tokio::spawn(telegram::run(notifier, name.clone(), stopped)));
					name
				}
				None => {
					tracing::warn!("TELEGRAM_BOT_TOKEN unset: Telegram notifications off");
					BotName::off()
				}
			};
			let sign_in = SignIn::new(panel, concierge, config);
			let app = http::app_with_telegram(sign_in.clone(), http::Limits::default(), bot);
			match forward {
				Some(upstreams) => app.merge(panel_server::forward::Forward::new(sign_in, upstreams).routes()),
				None => {
					tracing::warn!("forward not configured: no /api/review_archive, /review_archive/mfe or /playbook_mcp");
					app
				}
			}
		}
		None => {
			tracing::warn!("sign-in not configured: serving ingest only, no /auth, no /api/v1");
			if telegram.is_some() {
				tracing::warn!("Telegram notifications off: they need the sign-in, to link users and confirm their access");
			}
			http::router(panel)
		}
	};
	// The invoice routes find the ledger here; without it they answer 503.
	let app = match peers.ledger {
		Some(ledger) => app.layer(axum::Extension(ledger)),
		None => app,
	};
	let app = match front_end.files {
		Some(files) => web::serve(app, files),
		None => {
			tracing::warn!("PANEL_WEB_DIR unset: serving the API alone, no front end");
			app
		}
	};
	let listener = tokio::net::TcpListener::bind(bind).await.wrap_err_with(|| format!("binding {bind}"))?;
	tracing::info!(%bind, "serving");
	let served = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await.wrap_err("HTTP server");
	// The live sockets outlive the graceful shutdown (an upgraded connection is no longer the
	// server's): told to close, and given a moment for their close frames to go out.
	bus.publish(panel::live::Signal::GoingAway);
	bus.drained(std::time::Duration::from_secs(2)).await;
	// Nobody may be listening any more: the bot is simply not running then.
	let _sent = stop.send(true);
	if let Some(work) = bot_work
		&& let Err(e) = work.await
	{
		panel_server::report(&eyre::eyre!(e), "the telegram worker panicked");
	}
	if let Some(work) = capture_work
		&& let Err(e) = work.await
	{
		panel_server::report(&eyre::eyre!(e), "the posthog sender panicked");
	}
	if let Some(work) = booking_work
		&& let Err(e) = work.await
	{
		panel_server::report(&eyre::eyre!(e), "the google calendar sync panicked");
	}
	if let Some(work) = invoice_work
		&& let Err(e) = work.await
	{
		panel_server::report(&eyre::eyre!(e), "the invoice confirmation panicked");
	}
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

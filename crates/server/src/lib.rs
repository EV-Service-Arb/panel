//! The conductor: the HTTP surface over the engine's [`sa_panel::Panel`] — the sign-in through
//! concierge, its own API (people's GMBs and tokens among it), ingest and the forward to the
//! services behind it — and the hubs (live updates, alerts, review_archive's charges
//! and snapshot). The binary (`panel`) adds the CLI and the process bootstrap.
//!
//! A library target too, so tests can serve the real router in-process.

pub mod access;
pub mod alerts;
pub mod api;
pub mod concierge;
mod connected;
pub mod cookies;
pub mod forward;
pub mod gmbs;
pub mod grafana;
pub mod http;
mod ingest;
pub mod live;
pub mod signin;
pub mod telegram;
#[cfg(feature = "testing")]
pub mod testing;
pub mod tokens;
pub mod web;

/// What the contract's `provides` names, as the router mounts it (`--print-provided-paths`).
pub const PROVIDED_PATHS: [&str; 1] = [ingest::EVENTS];

/// Reports an error to Sentry and logs it — as a warning: the tracing layer would send an
/// error-level event to Sentry a second time.
pub fn report(e: &eyre::Report, what: &str) {
	ev_lib::error_monitoring::report(&**e);
	tracing::warn!(error = format!("{e:#}"), "{what}");
}

/// Runs `pass` every `period` until `shutdown` turns true. A failing pass is reported as
/// `what` and tried again at the next tick; nothing here ends `serve`.
pub(crate) async fn every<F, Fut>(period: std::time::Duration, mut shutdown: tokio::sync::watch::Receiver<bool>, what: &'static str, pass: F)
where
	F: Fn() -> Fut + Send,
	Fut: Future<Output = eyre::Result<()>> + Send, {
	let mut tick = tokio::time::interval(period);
	tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	loop {
		tokio::select! {
			_ = tick.tick() => {}
			_ = shutdown.changed() => return,
		}
		if *shutdown.borrow() {
			return;
		}
		if let Err(e) = pass().await {
			report(&e, what);
		}
	}
}

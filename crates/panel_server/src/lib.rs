//! The panel service: the HTTP surface over the engine's [`panel::Panel`] — ingest, the
//! sign-in through concierge, the operator API — and the Telegram bot. The binary
//! (`panel`) adds the CLI and the process bootstrap.
//!
//! A library target too, so tests can serve the real router in-process.

pub mod access;
pub mod api;
pub mod booking;
pub mod capture;
pub mod concierge;
pub mod cookies;
pub mod experiments;
pub mod forward;
pub mod google_calendar;
pub mod http;
pub mod invoices;
pub mod live;
pub mod places;
pub mod pricing;
pub mod scope_ledger;
pub mod signin;
pub mod telegram;
pub mod web;

/// Where `serve` listens unless told otherwise; the image binds the same port on 0.0.0.0.
pub const DEFAULT_BIND: &str = "127.0.0.1:59120";

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

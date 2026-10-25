//! The Service-Arb panel without I/O: no database, network, clock or randomness. Time
//! comes in as an argument.
//!
//! - [`ids`]: the typed identities events carry;
//! - [`event`]: the envelope of a `sa.funnel.v1` event, and what a signing key may write;
//! - [`fact`]: the registered event types, typed and checked;
//! - [`lead`]: a lead's stage and stage times, folded from its facts;
//! - [`experiment`]: a brand's experiments as its landing declares them and an admin sets them;
//! - [`funnel`]: shares no more precise than the data, and the contact SLA;
//! - [`signature`]: how a source signs a batch, and how the panel checks it;
//! - [`place`]: a place's live settings (phones, hours, …), checked as the sites read them;
//! - [`pricing`]: a brand's price list, checked and priced as kitstart does, to the cent;
//! - [`analytics`]: what PostHog is told of a lead's life, without PII;
//! - [`notify`]: the Telegram rules, their messages and buttons, and how a delivery is retried;
//! - [`booking`]: the booking providers and a place's choice of them, and a lead's booking
//!   folded from its facts; [`phone`]: a phone number as the sites normalize it.

pub mod analytics;
pub mod booking;
pub mod event;
pub mod experiment;
pub mod fact;
pub mod funnel;
pub mod ids;
pub mod invoice;
pub mod lead;
pub mod notify;
pub mod phone;
pub mod place;
pub mod pricing;
pub mod signature;

/// Why an event (or a part of one) cannot be accepted. The message is what the source is
/// told, so it names the field and never echoes a secret or PII.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

impl Invalid {
	pub fn new(msg: impl Into<String>) -> Self {
		Self(msg.into())
	}
}

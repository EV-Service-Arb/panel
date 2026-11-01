//! The conductor without I/O: no database, network, clock or randomness. Time comes in as an
//! argument.
//!
//! - [`ids`]: what a source key may name (brands), how its key id looks, and a site key's origin;
//! - [`signature`]: how a source signs a batch, and how the conductor checks it;
//! - [`notify`]: the Telegram rules, the conductor's own messages, the buttons' signed data, and
//!   how a delivery is retried;
//! - [`tokens`]: how a balance renews and a charge falls on its payers.

pub mod ids;
pub mod notify;
pub mod signature;
pub mod tokens;

/// Why an input cannot be accepted. The message is what the caller is told, so it names the
/// field and never echoes a secret or PII.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

impl Invalid {
	pub fn new(msg: impl Into<String>) -> Self {
		Self(msg.into())
	}
}

impl From<sa_auth::UnknownKind> for Invalid {
	fn from(e: sa_auth::UnknownKind) -> Self {
		Self(e.to_string())
	}
}

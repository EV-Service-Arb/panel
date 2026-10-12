//! What changed, told to whoever is watching: the bus the server's `/api/v1/live` sockets
//! listen on.
//!
//! The panel runs as one process (one pod, one SQLite writer), so the bus is in-process: a
//! `tokio::sync::broadcast` channel. Every write that a screen reads publishes here, from the
//! engine, after its transaction commits — never before, so a client told "changed" and
//! reading at once finds the change. What publishes:
//!
//! ```text
//! Panel::journal        every event journaled and projected: ingest, the operator's actions,
//!                       the Telegram buttons, a landing's experiments,
//!                       an admin's experiment settings, the booking adapters
//!                                                   → leads | lead | experiments | bookings
//! Panel::rebuild_…      the projections replaced whole                 → resync
//! place::edit, register a place's settings, withdrawn, registered      → places
//! pricing               a brand's model saved or removed, its locales  → pricing
//! add_source, revoke    the signing keys                               → sources
//! telegram              linked, unlinked, blocked, rules               → telegram (its user only)
//! sessions closed       logout, a sign-in replacing one, a refusal     → the sockets of that session end
//! ```
//!
//! Publishing never waits and never fails a write: with nobody subscribed the message is
//! simply dropped, and a subscriber that falls behind by more than the channel holds is told
//! so ([`broadcast::error::RecvError::Lagged`]) instead of the channel growing.
//!
//! A command run in another process (`panel place set`, `panel source add`, a rebuild from the
//! CLI) publishes on its own bus, which nobody listens to: the server's sockets do not see it.

use std::time::Duration;

use jiff::Timestamp;
use panel_core::{fact::Fact, ids::BrandId, lead::Recorded};
use sa_auth::{Analysis, PermissionSet, Sources, Work};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::session::SessionKey;

/// How many messages a subscriber may fall behind before it is told to read everything
/// afresh. A batch of ingest is at most 500 events; most moments carry a handful.
pub const DEFAULT_CAPACITY: usize = 1024;

/// What a change is about: which of the screens' reads it may have changed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Topic {
	/// A new lead (its `lead.created`).
	Leads,
	/// Something about one lead: a stage, a call, a payment.
	Lead,
	/// A place's settings, withdrawn or restored, or registered.
	Places,
	/// A brand's pricing: its model saved or removed, or its locales.
	Pricing,
	/// The signing keys: admins only, as `GET /sources` is.
	Sources,
	/// A brand's experiments: what its landing declared, or what an admin set over it.
	Experiments,
	/// One user's Telegram link and rules: that user only.
	Telegram,
	/// The providers' bookings without a lead: one came, changed, or was joined to a lead.
	Bookings,
}

impl Topic {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Leads => "leads",
			Self::Lead => "lead",
			Self::Places => "places",
			Self::Pricing => "pricing",
			Self::Sources => "sources",
			Self::Experiments => "experiments",
			Self::Telegram => "telegram",
			Self::Bookings => "bookings",
		}
	}
}

/// Something committed that a screen reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
	pub topic: Topic,
	pub brand: Option<BrandId>,
	/// The lead, or the place's slug; `None` when the topic has no single subject.
	pub id: Option<String>,
	/// Whose it is, for [`Topic::Telegram`]; `None` for everything else.
	pub user: Option<Uuid>,
	/// When it was committed (the writer's clock).
	pub at: Timestamp,
}

impl Change {
	/// Whether `user`, holding `permissions`, may be told of it: exactly when they may read
	/// what changed, for every brand (no permission is narrower than the whole panel); a
	/// Telegram link is its own user's.
	pub fn visible_to(&self, user: Uuid, permissions: &PermissionSet) -> bool {
		match self.topic {
			Topic::Leads | Topic::Lead | Topic::Places | Topic::Pricing | Topic::Bookings => permissions.may(Work::Read),
			Topic::Experiments => permissions.may(Analysis::Read),
			Topic::Sources => permissions.may(Sources::Manage),
			Topic::Telegram => self.user == Some(user),
		}
	}

	/// What journaling `event` changed; `None` for what no screen reads.
	pub fn of_event(event: &Recorded, at: Timestamp) -> Option<Self> {
		let lead = || event.subject.lead_id.as_ref().map(|l| l.as_str().to_owned());
		let (topic, id) = match &event.fact {
			Fact::LeadCreated { .. } => (Topic::Leads, lead()),
			Fact::LeadMessaged { .. }
			| Fact::LeadContacted { .. }
			| Fact::LeadQuoted { .. }
			| Fact::JobWon
			| Fact::LeadLost { .. }
			| Fact::JobCompleted
			| Fact::PaymentReceived { .. }
			| Fact::CallAttempted
			| Fact::CallLogged { .. }
			| Fact::BookingRequested { .. }
			| Fact::BookingCreated { .. }
			| Fact::BookingCanceled { .. }
			| Fact::BookingSet { .. }
			| Fact::BookingStatusChanged(_)
			| Fact::BookingCleared
			| Fact::BookingAttached { .. } => (Topic::Lead, lead()),
			Fact::ExperimentsDeclared(_) => (Topic::Experiments, None),
			Fact::ExperimentConfigured { patch, .. } => (Topic::Experiments, Some(patch.key.clone())),
			Fact::RetiredCount => return None,
		};
		Some(Self {
			topic,
			brand: Some(event.subject.brand_id.clone()),
			id,
			user: None,
			at,
		})
	}
}

impl Change {
	/// What journaling `event` changed, once projected: as [`Self::of_event`], but a booking
	/// event tells every lead it changed (a provider's booking may leave one lead and join
	/// another), and the list of bookings without a lead when that changed — and nothing of a
	/// lead it did not change.
	pub fn of_applied(event: &Recorded, applied: &crate::store::projections::Applied, at: Timestamp) -> Vec<Self> {
		let lead = |l: &panel_core::ids::LeadId| Self {
			topic: Topic::Lead,
			brand: Some(event.subject.brand_id.clone()),
			id: Some(l.as_str().to_owned()),
			user: None,
			at,
		};
		match &event.fact {
			Fact::BookingCreated { .. } | Fact::BookingCanceled { .. } | Fact::BookingAttached { .. } => {
				let mut out: Vec<Self> = applied.leads.iter().map(lead).collect();
				if applied.unmatched {
					out.push(Self {
						topic: Topic::Bookings,
						brand: Some(event.subject.brand_id.clone()),
						id: None,
						user: None,
						at,
					});
				}
				out
			}
			_ => Self::of_event(event, at).into_iter().collect(),
		}
	}
}

/// Which sessions have ended, so the sockets they opened close at once rather than at their
/// next check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ended {
	/// Every session of the user (a sign-out).
	User(Uuid),
	/// One session.
	Session(SessionKey),
}

/// What travels on the bus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Signal {
	Changed(Change),
	/// Too much changed to say what: read everything again.
	Resync,
	SessionsEnded(Ended),
	/// The process is stopping.
	GoingAway,
}

/// The in-process bus. Cloning it gives another handle on the same channel.
#[derive(Clone, Debug)]
pub struct Bus {
	tx: broadcast::Sender<Signal>,
}

impl Default for Bus {
	fn default() -> Self {
		Self::new(DEFAULT_CAPACITY)
	}
}

impl Bus {
	/// A bus whose subscribers may fall `capacity` messages behind (at least 1).
	pub fn new(capacity: usize) -> Self {
		Self {
			tx: broadcast::channel(capacity.max(1)).0,
		}
	}

	pub fn subscribe(&self) -> broadcast::Receiver<Signal> {
		self.tx.subscribe()
	}

	/// Tells every subscriber; never waits.
	pub fn publish(&self, signal: Signal) {
		// `Err` means nobody is subscribed, and then there is nobody to tell: the write it
		// follows has committed either way.
		let _nobody_listening = self.tx.send(signal);
	}

	pub fn changed(&self, change: Change) {
		self.publish(Signal::Changed(change));
	}

	/// Waits until every subscriber has let go, or `within` has passed: at shutdown, so the
	/// sockets' close frames go out before the process ends.
	pub async fn drained(&self, within: Duration) {
		let deadline = tokio::time::Instant::now() + within;
		while self.tx.receiver_count() > 0 && tokio::time::Instant::now() < deadline {
			// A broadcast sender cannot be awaited for its receivers to go; they go within
			// moments of `GoingAway`, so a short poll is enough.
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn change(topic: Topic, user: Option<Uuid>) -> Change {
		Change {
			topic,
			brand: None,
			id: None,
			user,
			at: Timestamp::UNIX_EPOCH,
		}
	}

	#[test]
	fn who_is_told() {
		let (ann, bob) = (Uuid::from_u128(1), Uuid::from_u128(2));
		let operator: PermissionSet = sa_auth::SA_OPERATOR.members.iter().copied().collect();
		let admin: PermissionSet = sa_auth::SA_ADMIN.members.iter().copied().collect();
		let work_only: PermissionSet = [Work::Read.as_str()].into_iter().collect();
		for topic in [Topic::Leads, Topic::Lead, Topic::Places, Topic::Pricing, Topic::Experiments, Topic::Bookings] {
			assert!(change(topic, None).visible_to(ann, &operator), "{topic:?}");
			assert!(change(topic, None).visible_to(ann, &admin), "{topic:?}");
		}
		assert!(!change(Topic::Experiments, None).visible_to(ann, &work_only), "an analyst's read");
		assert!(!change(Topic::Sources, None).visible_to(ann, &operator), "an admin's read");
		assert!(change(Topic::Sources, None).visible_to(ann, &admin));
		assert!(change(Topic::Telegram, Some(ann)).visible_to(ann, &operator));
		assert!(!change(Topic::Telegram, Some(ann)).visible_to(bob, &admin), "another user's link, admin or not");
		assert!(!change(Topic::Telegram, None).visible_to(ann, &admin), "nobody's");
	}

	#[tokio::test]
	async fn publishing_with_nobody_listening_is_fine() {
		let bus = Bus::new(2);
		bus.publish(Signal::Resync);
		let mut rx = bus.subscribe();
		bus.publish(Signal::Resync);
		assert_eq!(rx.recv().await.unwrap(), Signal::Resync);
		drop(rx);
		bus.drained(Duration::from_secs(1)).await;
	}

	#[tokio::test]
	async fn a_subscriber_behind_is_told_it_lagged() {
		let bus = Bus::new(2);
		let mut rx = bus.subscribe();
		for _ in 0..5 {
			bus.publish(Signal::Resync);
		}
		assert!(matches!(rx.recv().await, Err(broadcast::error::RecvError::Lagged(3))));
	}
}

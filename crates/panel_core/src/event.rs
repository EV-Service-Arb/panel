//! The envelope every `sa.funnel.v1` event carries, checked, and what a signing key may
//! write.

use std::{collections::BTreeSet, fmt, str::FromStr};

use jiff::{SignedDuration, Timestamp};

use crate::{
	Invalid,
	ids::{BrandId, EventId, JobId, LeadId, LocationId},
};

/// How far in the future `occurred_at` may be: the sources' clocks are not ours.
pub const MAX_CLOCK_SKEW: SignedDuration = SignedDuration::from_mins(5);

/// Where an event comes from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceKind {
	Site,
	ReviewArchive,
	Gbp,
	Posthog,
	/// Typed in by a person in the panel: events of this kind are manual.
	Panel,
	Sheet,
	Telephony,
	/// The panel's booking adapters: a provider's webhook, or the calendar it pulls. Written
	/// by the panel itself; no signing key is ever issued for it.
	Booking,
	/// A messenger bot or auto-responder (a brand's Telegram bot, a WhatsApp responder):
	/// it starts a lead from a conversation and says when a customer wrote.
	Bot,
}

impl SourceKind {
	pub const ALL: [Self; 9] = [
		Self::Site,
		Self::ReviewArchive,
		Self::Gbp,
		Self::Posthog,
		Self::Panel,
		Self::Sheet,
		Self::Telephony,
		Self::Booking,
		Self::Bot,
	];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Site => "site",
			Self::ReviewArchive => "review_archive",
			Self::Gbp => "gbp",
			Self::Posthog => "posthog",
			Self::Panel => "panel",
			Self::Sheet => "sheet",
			Self::Telephony => "telephony",
			Self::Booking => "booking",
			Self::Bot => "bot",
		}
	}

	/// Whether a signing key is issued for it: the panel writes its own kinds without one.
	pub fn keyed(self) -> bool {
		!matches!(self, Self::Panel | Self::Booking)
	}

	/// Whether a person entered it rather than a system observing it (spec §10a): the
	/// reports show which share of the funnel is hand-made.
	pub fn is_manual(self) -> bool {
		matches!(self, Self::Panel)
	}
}

impl FromStr for SourceKind {
	type Err = Invalid;

	fn from_str(s: &str) -> Result<Self, Invalid> {
		Self::ALL.into_iter().find(|k| k.as_str() == s).ok_or_else(|| {
			Invalid::new(format!(
				"source.kind {s:?} is not one of site, review_archive, gbp, posthog, panel, sheet, telephony, booking, bot"
			))
		})
	}
}

impl fmt::Display for SourceKind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
	pub kind: SourceKind,
	/// Which one of that kind; free-form, 1–128 characters.
	pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subject {
	pub brand_id: BrandId,
	pub location_id: Option<LocationId>,
	pub lead_id: Option<LeadId>,
	pub job_id: Option<JobId>,
}

/// `type@type_version`: the key of the registry.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TypeKey {
	pub name: String,
	pub version: u32,
}

impl TypeKey {
	/// A type name is dotted lowercase words (`lead.created`), up to 64 characters; the
	/// version starts at 1.
	pub fn parse(name: &str, version: u32) -> Result<Self, Invalid> {
		let well_formed = (1..=64).contains(&name.len())
			&& name
				.split('.')
				.all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
		if !well_formed {
			return Err(Invalid::new("type is not dotted lowercase words, e.g. \"lead.created\""));
		}
		// The journal keeps it in a Postgres integer.
		if version == 0 || version > i32::MAX as u32 {
			return Err(Invalid::new("type_version must be 1 to 2147483647"));
		}
		Ok(Self { name: name.to_owned(), version })
	}
}

impl fmt::Display for TypeKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}@{}", self.name, self.version)
	}
}

/// Everything about an event but its properties and PII.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Envelope {
	pub id: EventId,
	pub type_key: TypeKey,
	pub occurred_at: Timestamp,
	pub source: Source,
	pub subject: Subject,
}

impl Envelope {
	/// Checks what cannot be known from the fields one by one: that the event did not
	/// happen in the future.
	pub fn check_time(&self, now: Timestamp) -> Result<(), Invalid> {
		if self.occurred_at > now + MAX_CLOCK_SKEW {
			return Err(Invalid::new("occurred_at is in the future"));
		}
		Ok(())
	}
}

/// Whether a kind of source may write a type (spec §2, the "source" column). What only an
/// operator can know — a quote, a win, a loss, a finished job — comes from the panel alone,
/// and payments too, which are entered by hand (owner, 2026-09-30); calls and contacts also
/// from telephony, once there is one. The counts of the retired PostHog import were its alone
/// (§3.4), and stay so. A booking a site asks for is the site's; what a provider says of a
/// booking is its adapter's (kind booking); what an operator does to one is the panel's. A
/// landing declares its experiments; only an admin in the panel lays a setting over them. A
/// type the panel does not know is open to every kind (§3.2): it is stored, not projected,
/// and judged again once it is registered. A messenger bot starts a lead from a conversation
/// (its channel a messenger's: the registry checks that) and says when a customer wrote; an
/// operator may say the latter too, a landing never — it only knows a link was opened.
pub fn may_write(kind: SourceKind, type_name: &str) -> bool {
	use SourceKind::{Booking, Bot, Panel, Posthog, Site, Telephony};
	match type_name {
		"lead.created" => matches!(kind, Site | Panel | Bot),
		"lead.messaged" => matches!(kind, Bot | Panel),
		"booking.requested" => matches!(kind, Site),
		"booking.created" | "booking.canceled" => matches!(kind, Booking),
		"booking.set" | "booking.status_changed" | "booking.cleared" | "booking.attached" => matches!(kind, Panel),
		"site.metrics" | "contact.metrics" | "experiment.metrics" => matches!(kind, Posthog),
		"experiments.declared" => matches!(kind, Site),
		"experiment.configured" => matches!(kind, Panel),
		"lead.contacted" | "call.attempted" | "call.logged" => matches!(kind, Panel | Telephony),
		"lead.quoted" | "job.won" | "lead.lost" | "job.completed" | "payment.received" => matches!(kind, Panel),
		_ => true,
	}
}

/// A source's signing key, as the panel knows it: which kind of source holds it and which
/// brands it may write for. The key of aquafix cannot write vifnet's events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyGrant {
	pub key_id: String,
	pub kind: SourceKind,
	pub brands: BTreeSet<BrandId>,
}

impl KeyGrant {
	/// Whether an event signed with this key may say what it says about its source and brand.
	pub fn permits(&self, envelope: &Envelope) -> Result<(), Invalid> {
		if envelope.source.kind != self.kind {
			return Err(Invalid::new(format!("source.kind {} is not the kind this key is for", envelope.source.kind)));
		}
		// A source is called by its key: the id it gives itself cannot be another's, so what
		// the journal says came from aquafix-site did.
		if envelope.source.id != self.key_id {
			return Err(Invalid::new("source.id is not the id of the key that signed it"));
		}
		if !self.brands.contains(&envelope.subject.brand_id) {
			return Err(Invalid::new(format!("this key may not write for brand {}", envelope.subject.brand_id)));
		}
		if !may_write(self.kind, &envelope.type_key.name) {
			return Err(Invalid::new(format!("a {} source may not write {}", self.kind, envelope.type_key.name)));
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use uuid::Uuid;

	use super::*;

	fn envelope(kind: SourceKind, brand: &str) -> Envelope {
		Envelope {
			id: EventId::from_raw(Uuid::now_v7()),
			type_key: TypeKey::parse("lead.created", 1).unwrap(),
			occurred_at: "2026-09-30T10:00:00Z".parse().unwrap(),
			source: Source { kind, id: "aquafix-site".into() },
			subject: Subject {
				brand_id: BrandId::parse(brand).unwrap(),
				location_id: None,
				lead_id: None,
				job_id: None,
			},
		}
	}

	#[test]
	fn a_key_writes_its_own_kind_and_brands_only() {
		let grant = KeyGrant {
			key_id: "aquafix-site".into(),
			kind: SourceKind::Site,
			brands: [BrandId::parse("aquafix").unwrap()].into(),
		};
		assert!(grant.permits(&envelope(SourceKind::Site, "aquafix")).is_ok());
		assert_eq!(grant.permits(&envelope(SourceKind::Site, "vifnet")).unwrap_err().0, "this key may not write for brand vifnet");
		assert!(
			grant.permits(&envelope(SourceKind::Panel, "aquafix")).is_err(),
			"a site key cannot pass its events off as typed in by hand"
		);
		let mut posing = envelope(SourceKind::Site, "aquafix");
		posing.source.id = "vifnet-site".into();
		assert_eq!(grant.permits(&posing).unwrap_err().0, "source.id is not the id of the key that signed it");
	}

	#[test]
	fn who_writes_what() {
		use SourceKind::*;
		let table: [(&str, &[SourceKind]); 22] = [
			("lead.created", &[Site, Panel, Bot]),
			("lead.messaged", &[Bot, Panel]),
			("lead.contacted", &[Panel, Telephony]),
			("lead.quoted", &[Panel]),
			("job.won", &[Panel]),
			("lead.lost", &[Panel]),
			("job.completed", &[Panel]),
			("payment.received", &[Panel]),
			("call.attempted", &[Panel, Telephony]),
			("call.logged", &[Panel, Telephony]),
			("site.metrics", &[Posthog]),
			("contact.metrics", &[Posthog]),
			("experiment.metrics", &[Posthog]),
			("booking.requested", &[Site]),
			("booking.created", &[Booking]),
			("booking.canceled", &[Booking]),
			("booking.set", &[Panel]),
			("booking.status_changed", &[Panel]),
			("booking.cleared", &[Panel]),
			("booking.attached", &[Panel]),
			("experiments.declared", &[Site]),
			("experiment.configured", &[Panel]),
		];
		for (name, allowed) in table {
			for kind in SourceKind::ALL {
				assert_eq!(may_write(kind, name), allowed.contains(&kind), "{kind} {name}");
			}
		}
		assert!(SourceKind::ALL.into_iter().all(|k| may_write(k, "review.new")), "unknown types are open");

		let site = KeyGrant {
			key_id: "aquafix-site".into(),
			kind: Site,
			brands: [BrandId::parse("aquafix").unwrap()].into(),
		};
		let mut payment = envelope(Site, "aquafix");
		payment.type_key = TypeKey::parse("payment.received", 1).unwrap();
		assert_eq!(site.permits(&payment).unwrap_err().0, "a site source may not write payment.received");
	}

	#[test]
	fn future_events_are_refused_past_the_skew() {
		let e = envelope(SourceKind::Site, "aquafix");
		assert!(e.check_time(e.occurred_at - MAX_CLOCK_SKEW).is_ok());
		assert!(e.check_time(e.occurred_at - MAX_CLOCK_SKEW - SignedDuration::from_secs(1)).is_err());
	}

	#[test]
	fn type_keys() {
		assert_eq!(TypeKey::parse("call.logged", 2).unwrap().to_string(), "call.logged@2");
		for bad in ["", "Lead.created", "lead..created", "lead created", ".lead"] {
			assert!(TypeKey::parse(bad, 1).is_err(), "{bad:?}");
		}
		assert!(TypeKey::parse("lead.created", 0).is_err());
		assert!(TypeKey::parse("lead.created", i32::MAX as u32).is_ok());
		assert!(TypeKey::parse("lead.created", i32::MAX as u32 + 1).is_err());
		assert_eq!("review_archive".parse::<SourceKind>().unwrap(), SourceKind::ReviewArchive);
		assert!("Site".parse::<SourceKind>().is_err());
	}
}

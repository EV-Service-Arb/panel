//! protojson → the domain: one event of a batch, decoded and checked, and the registry of
//! the types the panel projects.
//!
//! Each event of a batch is decoded on its own, so one malformed event is rejected alone
//! rather than failing the batch.

use jiff::Timestamp;
use panel_contracts::{SCHEMA, v1};
use panel_core::{
	Invalid,
	booking::{self, BookingMatch, Closed},
	event::{Envelope, Source, SourceKind, Subject, TypeKey, may_write},
	experiment::{Declaration, Patch, check_declared, label},
	fact::{AnalyticsId, CallOutcome, ContactChannel, Fact, LeadChannel, LeadLocale, LeadOffer, LeadSuspect, MessageRef, Messenger, bounded, day, instant},
	ids::{BrandId, JobId, LeadId, LocationId, parse_event_id},
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Events in one request, at most.
pub const MAX_BATCH: usize = 500;

/// An event's `properties`, and its `pii`, serialized, at most — registered or not.
pub const MAX_OBJECT_BYTES: usize = 16 * 1024;

/// An event that passed the envelope checks; its properties are not judged yet.
#[derive(Clone, Debug)]
pub struct Incoming {
	pub envelope: Envelope,
	/// A JSON object, as sent.
	pub properties: Value,
	/// A non-empty JSON object, or none.
	pub pii: Option<Value>,
	/// The event in canonical form: the same event sent twice, in either field spelling or
	/// key order, reads the same. What the journal keeps of it is a MAC ([`crate::seal`]), not
	/// this.
	pub canonical: Vec<u8>,
}

/// The events of a request body: `{"events": [...]}`, 1 to [`MAX_BATCH`] of them. What is
/// wrong with the body as a whole is the caller's mistake.
pub fn batch(body: &[u8]) -> Result<Vec<Value>, Invalid> {
	let value: Value = serde_json::from_slice(body).map_err(|e| Invalid::new(format!("body is not JSON: {}", redact(&e))))?;
	let Value::Object(mut top) = value else {
		return Err(Invalid::new("body is not a JSON object"));
	};
	if let Some(other) = top.keys().find(|k| *k != "events") {
		return Err(Invalid::new(format!("unknown field {other:?} in the body")));
	}
	let Some(Value::Array(events)) = top.remove("events") else {
		return Err(Invalid::new("body has no events array"));
	};
	if events.is_empty() || events.len() > MAX_BATCH {
		return Err(Invalid::new(format!("a batch holds 1 to {MAX_BATCH} events, not {}", events.len())));
	}
	Ok(events)
}

/// The id an event was sent with, if it has a string one: for the answer about it, even
/// when nothing else about it can be read.
pub fn claimed_id(raw: &Value) -> String {
	raw.get("id").and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Decodes one event and checks its envelope. `now` bounds how far in the future it may
/// have happened.
pub fn decode(raw: Value, now: Timestamp) -> Result<Incoming, Invalid> {
	// `properties` and `pii` are kept as sent: a protobuf `Struct` holds every number as a
	// double, and going through it would turn `12000` into `12000.0`, which an int64 field
	// then refuses. The typed decode below still checks they are objects.
	let object = |field: &str| match raw.get(field) {
		None | Some(Value::Null) => Value::Object(Map::new()),
		Some(v) => v.clone(),
	};
	let (properties, pii) = (object("properties"), object("pii"));
	let event: v1::Event = serde_json::from_value(raw).map_err(|e| Invalid::new(format!("not a {SCHEMA} event: {}", redact(&e))))?;
	if event.schema != SCHEMA {
		return Err(Invalid::new(format!("schema is {:?}, not {SCHEMA:?}", event.schema)));
	}
	let canonical = canonical(&event)?;
	let id = parse_event_id(&event.id)?;
	let type_key = TypeKey::parse(&event.r#type, event.type_version)?;
	let occurred_at = event.occurred_at.as_ref().ok_or_else(|| Invalid::new("occurred_at is required"))?;
	let occurred_at = Timestamp::new(occurred_at.seconds, occurred_at.nanos)
		.map_err(|_| Invalid::new("occurred_at is out of range"))?
		// The journal keeps microseconds (`store::to_db`); cut here so what is checked is what is stored.
		.round(jiff::TimestampRound::new().smallest(jiff::Unit::Microsecond).mode(jiff::RoundMode::Trunc))
		.map_err(|_| Invalid::new("occurred_at is out of range"))?;

	let source = event.source.as_ref().ok_or_else(|| Invalid::new("source is required"))?;
	if !(1..=128).contains(&source.id.chars().count()) {
		return Err(Invalid::new("source.id must be 1 to 128 characters"));
	}
	if source.id.contains('\0') {
		return Err(Invalid::new("source.id contains a NUL character"));
	}
	let source = Source {
		kind: source.kind.parse::<SourceKind>()?,
		id: source.id.clone(),
	};

	let subject = event.subject.as_ref().ok_or_else(|| Invalid::new("subject is required"))?;
	let subject = Subject {
		brand_id: BrandId::parse(&subject.brand_id)?,
		location_id: subject.location_id.as_deref().map(LocationId::parse).transpose()?,
		lead_id: subject.lead_id.as_deref().map(LeadId::parse).transpose()?,
		job_id: subject.job_id.as_deref().map(JobId::parse).transpose()?,
	};

	let envelope = Envelope {
		id,
		type_key,
		occurred_at,
		source,
		subject,
	};
	envelope.check_time(now)?;
	check_object("properties", &properties)?;
	check_object("pii", &pii)?;
	let pii = pii.as_object().is_some_and(|o| !o.is_empty()).then_some(pii);
	Ok(Incoming {
		envelope,
		properties,
		pii,
		canonical,
	})
}

/// What the journal should not hold, or what would cost too much to keep, refused here per
/// event: a NUL character anywhere (SQLite's string and JSON functions stop at one, so a
/// query would read another value than was stored — and the contract has always refused it),
/// and an object past [`MAX_OBJECT_BYTES`].
fn check_object(field: &str, value: &Value) -> Result<(), Invalid> {
	fn has_nul(v: &Value) -> bool {
		match v {
			Value::String(s) => s.contains('\0'),
			Value::Array(items) => items.iter().any(has_nul),
			Value::Object(map) => map.iter().any(|(k, v)| k.contains('\0') || has_nul(v)),
			Value::Null | Value::Bool(_) | Value::Number(_) => false,
		}
	}
	if has_nul(value) {
		return Err(Invalid::new(format!("{field} contains a NUL character")));
	}
	let size = serde_json::to_vec(value).map_err(|e| Invalid::new(format!("{field} does not serialize: {}", redact(&e))))?.len();
	if size > MAX_OBJECT_BYTES {
		return Err(Invalid::new(format!("{field} is {size} bytes, past the {MAX_OBJECT_BYTES} allowed")));
	}
	Ok(())
}

/// A JSON error as the source may be told it: which field and what was wrong, never the
/// value — it may be the PII the event carries, and reasons end up in logs. Field names
/// stay; everything quoted otherwise is replaced.
fn redact(e: &serde_json::Error) -> String {
	let msg = e.to_string();
	if ["unknown field", "missing field", "duplicate field"].iter().any(|p| msg.starts_with(p)) {
		return msg;
	}
	let mut out = String::with_capacity(msg.len());
	let mut quote: Option<char> = None;
	for c in msg.chars() {
		match quote {
			Some(q) if c == q => {
				out.push('…');
				out.push(c);
				quote = None;
			}
			Some(_) => {}
			None if c == '"' || c == '`' => {
				out.push(c);
				quote = Some(c);
			}
			None => out.push(c),
		}
	}
	out
}

fn canonical(event: &v1::Event) -> Result<Vec<u8>, Invalid> {
	let value = serde_json::to_value(event).map_err(|e| Invalid::new(format!("event does not serialize: {e}")))?;
	let mut out = Vec::new();
	write_canonical(&value, &mut out);
	Ok(out)
}

/// JSON with object keys sorted at every level, whatever map the JSON library was built
/// with (a protobuf `Struct` is a hash map, so its order is not stable across runs).
fn write_canonical(value: &Value, h: &mut Vec<u8>) {
	match value {
		Value::Object(map) => {
			let mut keys: Vec<&String> = map.keys().collect();
			keys.sort();
			h.extend_from_slice(b"{");
			for (i, k) in keys.into_iter().enumerate() {
				if i > 0 {
					h.extend_from_slice(b",");
				}
				h.extend_from_slice(Value::String(k.clone()).to_string().as_bytes());
				h.extend_from_slice(b":");
				write_canonical(&map[k], h);
			}
			h.extend_from_slice(b"}");
		}
		Value::Array(items) => {
			h.extend_from_slice(b"[");
			for (i, item) in items.iter().enumerate() {
				if i > 0 {
					h.extend_from_slice(b",");
				}
				write_canonical(item, h);
			}
			h.extend_from_slice(b"]");
		}
		scalar => h.extend_from_slice(scalar.to_string().as_bytes()),
	}
}

/// What the registry makes of an event's type and properties.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Checked {
	/// A registered type, and its properties and subject hold.
	Registered(Fact),
	/// Not a registered `type@version`: stored, not projected.
	Unregistered,
	/// A registered type whose checks the event fails.
	Invalid(Invalid),
}

/// Every `type@version` the panel projects, with the message its properties must match.
pub const REGISTERED: &[(&str, u32, &str)] = &[
	("lead.created", 1, "sa.v1.LeadCreatedV1"),
	("lead.messaged", 1, "sa.v1.LeadMessagedV1"),
	("lead.contacted", 1, "sa.v1.LeadContactedV1"),
	("lead.quoted", 1, "sa.v1.LeadQuotedV1"),
	("job.won", 1, "sa.v1.JobWonV1"),
	("lead.lost", 1, "sa.v1.LeadLostV1"),
	("job.completed", 1, "sa.v1.JobCompletedV1"),
	("payment.received", 1, "sa.v1.PaymentReceivedV1"),
	("call.attempted", 1, "sa.v1.CallAttemptedV1"),
	("call.logged", 1, "sa.v1.CallLoggedV1"),
	("site.metrics", 1, "sa.v1.SiteMetricsV1"),
	("contact.metrics", 1, "sa.v1.ContactMetricsV1"),
	("experiment.metrics", 1, "sa.v1.ExperimentMetricsV1"),
	("booking.requested", 1, "sa.v1.BookingRequestedV1"),
	("booking.created", 1, "sa.v1.BookingCreatedV1"),
	("booking.canceled", 1, "sa.v1.BookingCanceledV1"),
	("booking.set", 1, "sa.v1.BookingSetV1"),
	("booking.status_changed", 1, "sa.v1.BookingStatusChangedV1"),
	("booking.cleared", 1, "sa.v1.BookingClearedV1"),
	("booking.attached", 1, "sa.v1.BookingAttachedV1"),
	("experiments.declared", 1, "sa.v1.ExperimentsDeclaredV1"),
	("experiment.configured", 1, "sa.v1.ExperimentConfiguredV1"),
];

/// Checks an event's properties against its `type@version`, and what the type needs of its
/// subject. The same call judges an event on arrival and again when the projections are
/// rebuilt, so a type registered later picks up what arrived before it.
pub fn check(key: &TypeKey, kind: SourceKind, properties: &Value, subject: &Subject) -> Checked {
	// Again here, not only in the key's grant: the rebuild judges stored events through this
	// call alone.
	if !may_write(kind, &key.name) {
		return match REGISTERED.iter().any(|(name, version, _)| *name == key.name && *version == key.version) {
			true => Checked::Invalid(Invalid::new(format!("a {kind} source may not write {}", key.name))),
			false => Checked::Unregistered,
		};
	}
	let fact = match (key.name.as_str(), key.version) {
		("lead.created", 1) => props::<v1::LeadCreatedV1>(key, properties).and_then(|p| {
			let channel = LeadChannel::parse(&p.channel)?;
			// A bot sees a conversation, never a form or a call: what it starts is a messenger's.
			if kind == SourceKind::Bot && channel.messenger().is_none() {
				return Err(Invalid::new("a bot source writes lead.created only with channel whatsapp or telegram"));
			}
			Ok(Fact::LeadCreated {
				channel,
				entered_by: bounded("properties.entered_by", p.entered_by)?,
				suspect: p.suspect.as_deref().map(LeadSuspect::parse).transpose()?,
				offer: LeadOffer::parse(p.flow.as_deref(), p.quoted_cents, p.pricing_valid_from.as_deref(), p.estimate_inputs)?,
				analytics_id: p.analytics_id.as_deref().map(AnalyticsId::parse).transpose()?,
				message_ref: p.message_ref.as_deref().map(MessageRef::parse).transpose()?,
				locale: p.locale.as_deref().map(LeadLocale::parse).transpose()?,
			})
		}),
		("lead.messaged", 1) => props::<v1::LeadMessagedV1>(key, properties).and_then(|p| {
			Ok(Fact::LeadMessaged {
				channel: Messenger::parse(&p.channel)?,
				message_ref: p.message_ref.as_deref().map(MessageRef::parse).transpose()?,
			})
		}),
		("lead.contacted", 1) => props::<v1::LeadContactedV1>(key, properties).and_then(|p| {
			Ok(Fact::LeadContacted {
				channel: p.channel.as_deref().map(ContactChannel::parse).transpose()?,
			})
		}),
		("lead.quoted", 1) => props::<v1::LeadQuotedV1>(key, properties).and_then(|p| {
			Ok(Fact::LeadQuoted {
				quote: Fact::quote(p.amount, p.currency.as_deref())?,
			})
		}),
		("job.won", 1) => props::<v1::JobWonV1>(key, properties).map(|_| Fact::JobWon),
		("lead.lost", 1) => props::<v1::LeadLostV1>(key, properties).and_then(|p| Fact::lost(&p.reason, p.note)),
		("job.completed", 1) => props::<v1::JobCompletedV1>(key, properties).map(|_| Fact::JobCompleted),
		("payment.received", 1) => props::<v1::PaymentReceivedV1>(key, properties).and_then(|p| Fact::payment(p.billed, p.commission, &p.currency)),
		("call.attempted", 1) => props::<v1::CallAttemptedV1>(key, properties).map(|_| Fact::CallAttempted),
		("call.logged", 1) => props::<v1::CallLoggedV1>(key, properties).and_then(|p| {
			Ok(Fact::CallLogged {
				outcome: CallOutcome::parse(&p.outcome)?,
				attempt_id: bounded("properties.attempt_id", p.attempt_id)?,
			})
		}),
		// The retired PostHog import's counts: their shape is still checked, so what the journal
		// holds keeps its status, but they say nothing the panel shows any more.
		("site.metrics", 1) => props::<v1::SiteMetricsV1>(key, properties).map(|_| Fact::RetiredCount),
		("contact.metrics", 1) => props::<v1::ContactMetricsV1>(key, properties).map(|_| Fact::RetiredCount),
		("experiment.metrics", 1) => props::<v1::ExperimentMetricsV1>(key, properties).map(|_| Fact::RetiredCount),
		("experiments.declared", 1) => props::<v1::ExperimentsDeclaredV1>(key, properties).and_then(|p| {
			let declared = p
				.experiments
				.into_iter()
				.map(|e| Declaration::parse(&e.key, e.variants, e.weights, e.enabled, e.holdout, e.summary))
				.collect::<Result<Vec<_>, _>>()?;
			check_declared(&declared)?;
			Ok(Fact::ExperimentsDeclared(declared))
		}),
		("experiment.configured", 1) => props::<v1::ExperimentConfiguredV1>(key, properties).and_then(|p| {
			Ok(Fact::ExperimentConfigured {
				patch: Patch::parse(&p.key, p.enabled, p.weights, p.holdout, &p.reset)?,
				by: label(&p.by)?,
			})
		}),
		("booking.requested", 1) => {
			// A field set to null is refused, not read as absent: the contract has no null.
			if properties.as_object().is_some_and(|m| m.values().any(Value::is_null)) {
				return Checked::Invalid(Invalid::new("properties of booking.requested hold no null"));
			}
			props::<v1::BookingRequestedV1>(key, properties).and_then(|p| {
				// The site names its lead twice, in the subject and as kitstart's leadRef: they
				// must be the one id, or the booking would join another lead than it says.
				let date = p.preferred_date.as_deref().map(|d| day("properties.preferred_date", d)).transpose()?;
				let (provider, preferred_date, preferred_part) = booking::requested(&p.lead_ref, &p.provider, date, p.preferred_part.as_deref())?;
				if subject.lead_id.as_ref().map(|l| l.as_str()) != Some(p.lead_ref.as_str()) {
					return Err(Invalid::new("properties.lead_ref is not subject.lead_id"));
				}
				Ok(Fact::BookingRequested {
					provider,
					preferred_date,
					preferred_part,
				})
			})
		}
		("booking.created", 1) => props::<v1::BookingCreatedV1>(key, properties).and_then(|p| {
			let (provider, external_ref, version) = Fact::external(&p.provider, &p.external_ref, Some(&p.version))?;
			let (start_at, end_at) = Fact::slot(&p.start_at, p.end_at.as_deref())?;
			Ok(Fact::BookingCreated {
				provider,
				external_ref,
				start_at,
				end_at,
				matched: p
					.r#match
					.as_deref()
					.map(|m| BookingMatch::parse(m).map_err(|e| Invalid::new(format!("properties.{e}"))))
					.transpose()?,
				version,
				booked_at: p.booked_at.as_deref().map(|b| instant("properties.booked_at", b)).transpose()?,
			})
		}),
		("booking.canceled", 1) => props::<v1::BookingCanceledV1>(key, properties).and_then(|p| {
			let (provider, external_ref, version) = Fact::external(&p.provider, &p.external_ref, Some(&p.version))?;
			Ok(Fact::BookingCanceled { provider, external_ref, version })
		}),
		("booking.set", 1) => props::<v1::BookingSetV1>(key, properties).and_then(|p| {
			let (start_at, end_at) = Fact::slot(&p.start_at, p.end_at.as_deref())?;
			Ok(Fact::BookingSet { start_at, end_at })
		}),
		("booking.status_changed", 1) =>
			props::<v1::BookingStatusChangedV1>(key, properties).and_then(|p| Closed::parse(&p.status).map(Fact::BookingStatusChanged).map_err(|e| Invalid::new(format!("properties.{e}")))),
		("booking.cleared", 1) => props::<v1::BookingClearedV1>(key, properties).map(|_| Fact::BookingCleared),
		("booking.attached", 1) => props::<v1::BookingAttachedV1>(key, properties).and_then(|p| {
			let (provider, external_ref, _) = Fact::external(&p.provider, &p.external_ref, None)?;
			Ok(Fact::BookingAttached { provider, external_ref })
		}),
		_ => return Checked::Unregistered,
	};
	match fact.and_then(|f| f.check_subject(subject).map(|()| f)) {
		Ok(f) => Checked::Registered(f),
		Err(e) => Checked::Invalid(e),
	}
}

fn props<T: DeserializeOwned>(key: &TypeKey, properties: &Value) -> Result<T, Invalid> {
	serde_json::from_value(properties.clone()).map_err(|e| Invalid::new(format!("properties do not match {key}: {}", redact(&e))))
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn now() -> Timestamp {
		"2026-09-30T12:00:00Z".parse().unwrap()
	}

	fn event() -> Value {
		json!({
			"id": uuid::Uuid::now_v7().to_string(),
			"schema": "sa.funnel.v1",
			"type": "lead.created",
			"typeVersion": 1,
			"occurredAt": "2026-09-30T10:00:00.123456789Z",
			"source": {"kind": "site", "id": "aquafix"},
			"subject": {"brandId": "aquafix", "locationId": "paris-11", "leadId": "L-1"},
			"properties": {"channel": "form"},
			"pii": {"phone": "+33 6 00 00 00 00", "nested": {"b": 1, "a": [1, 2]}}
		})
	}

	#[test]
	fn a_good_event() {
		let mut e = event();
		e["properties"]["n"] = json!(12000);
		let got = decode(e, now()).unwrap();
		assert_eq!(got.properties["n"], json!(12000), "an integer stays one, not a Struct's double");
		let got = decode(event(), now()).unwrap();
		assert_eq!(got.envelope.type_key.to_string(), "lead.created@1");
		assert_eq!(got.envelope.occurred_at.to_string(), "2026-09-30T10:00:00.123456Z", "cut to microseconds");
		assert_eq!(got.pii.unwrap()["phone"], "+33 6 00 00 00 00");
		assert_eq!(
			check(&got.envelope.type_key, got.envelope.source.kind, &got.properties, &got.envelope.subject),
			Checked::Registered(Fact::LeadCreated {
				channel: LeadChannel::Form,
				entered_by: None,
				suspect: None,
				offer: LeadOffer::default(),
				analytics_id: None,
				message_ref: None,
				locale: None,
			})
		);
	}

	#[test]
	fn a_locale_is_fr_or_en_or_absent() {
		let judge = |locale: Option<Value>| {
			let mut e = event();
			if let Some(l) = locale {
				e["properties"]["locale"] = l;
			}
			let got = decode(e, now()).unwrap();
			check(&got.envelope.type_key, got.envelope.source.kind, &got.properties, &got.envelope.subject)
		};
		for (word, want) in [("fr", Some(LeadLocale::Fr)), ("en", Some(LeadLocale::En))] {
			let Checked::Registered(Fact::LeadCreated { locale, .. }) = judge(Some(json!(word))) else {
				panic!("{word} refused")
			};
			assert_eq!(locale, want);
		}
		let Checked::Registered(Fact::LeadCreated { locale, .. }) = judge(None) else {
			panic!("no locale refused")
		};
		assert_eq!(locale, None);
		for bad in [json!("de"), json!("EN"), json!("")] {
			assert_eq!(judge(Some(bad.clone())), Checked::Invalid(Invalid::new("properties.locale is not one of fr, en")), "{bad}");
		}
	}

	#[test]
	fn a_suspect_mark_is_one_of_its_words() {
		let judge = |mark: Value| {
			let mut e = event();
			e["properties"]["suspect"] = mark;
			let got = decode(e, now()).unwrap();
			check(&got.envelope.type_key, got.envelope.source.kind, &got.properties, &got.envelope.subject)
		};
		for (word, mark) in [("rate_limited", LeadSuspect::RateLimited), ("too_fast", LeadSuspect::TooFast)] {
			assert_eq!(
				judge(json!(word)),
				Checked::Registered(Fact::LeadCreated {
					channel: LeadChannel::Form,
					entered_by: None,
					suspect: Some(mark),
					offer: LeadOffer::default(),
					analytics_id: None,
					message_ref: None,
					locale: None,
				})
			);
		}
		for bad in [json!("honeypot"), json!("RATE_LIMITED"), json!("")] {
			assert_eq!(
				judge(bad.clone()),
				Checked::Invalid(panel_core::Invalid::new("properties.suspect is not one of rate_limited, too_fast")),
				"{bad}"
			);
		}
	}

	#[test]
	fn a_lead_carries_the_beacon_id_or_is_rejected_for_it() {
		let judge = |id: Value| {
			let mut e = event();
			e["properties"]["analyticsId"] = id;
			let got = decode(e, now()).unwrap();
			check(&got.envelope.type_key, got.envelope.source.kind, &got.properties, &got.envelope.subject)
		};
		let Checked::Registered(Fact::LeadCreated { analytics_id, .. }) = judge(json!("0192f1c2-7d1e-7b3a-9c4d-1a2b3c4d5e6f")) else {
			panic!()
		};
		assert_eq!(analytics_id.unwrap().as_str(), "0192f1c2-7d1e-7b3a-9c4d-1a2b3c4d5e6f");
		for bad in [json!(""), json!("a b"), json!("x".repeat(129))] {
			assert_eq!(
				judge(bad.clone()),
				Checked::Invalid(Invalid::new("properties.analytics_id is not 1–128 of [A-Za-z0-9._:-]")),
				"{bad}"
			);
		}
	}

	#[test]
	fn a_lead_says_its_flow_in_either_spelling() {
		let judge = |props: Value| {
			let mut e = event();
			e["properties"] = props;
			let got = decode(e, now()).unwrap();
			check(&got.envelope.type_key, got.envelope.source.kind, &got.properties, &got.envelope.subject)
		};
		let camel = judge(json!({"channel": "form", "flow": "estimate", "quotedCents": "12900", "pricingValidFrom": "2026-10-01", "estimateInputs": {"zone": "a", "bedrooms": "2"}}));
		let snake = judge(json!({"channel": "form", "flow": "estimate", "quoted_cents": 12900, "pricing_valid_from": "2026-10-01", "estimate_inputs": {"bedrooms": "2", "zone": "a"}}));
		assert_eq!(camel, snake);
		let Checked::Registered(Fact::LeadCreated { offer, .. }) = camel else { panic!("{camel:?}") };
		assert_eq!(offer.price.unwrap().cents, 12_900);
		assert_eq!(offer.estimate_inputs.len(), 2);
		assert_eq!(
			judge(json!({"channel": "form", "flow": "quote", "quotedCents": 100, "pricingValidFrom": "2026-10-01"})),
			Checked::Invalid(Invalid::new("properties.quoted_cents and properties.pricing_valid_from are only for flow estimate or fixed"))
		);
		assert!(
			matches!(judge(json!({"channel": "form", "estimateInputs": {"zone": 1}})), Checked::Invalid(_)),
			"values are strings"
		);
	}

	#[test]
	fn messenger_leads_and_messages() {
		let judge = |r#type: &str, kind: SourceKind, props: Value, lead: Option<&str>| {
			let mut e = event();
			e["type"] = json!(r#type);
			e["properties"] = props;
			match lead {
				Some(l) => e["subject"]["leadId"] = json!(l),
				None => _ = e["subject"].as_object_mut().unwrap().remove("leadId"),
			}
			let got = decode(e, now()).unwrap();
			check(&got.envelope.type_key, kind, &got.properties, &got.envelope.subject)
		};
		let Checked::Registered(Fact::LeadCreated { channel, message_ref, .. }) =
			judge("lead.created", SourceKind::Site, json!({"channel": "whatsapp", "messageRef": "AQ-7K3F"}), Some("L-1"))
		else {
			panic!("a messenger lead refused")
		};
		assert_eq!((channel, message_ref.unwrap().as_str()), (LeadChannel::Whatsapp, "AQ-7K3F"));
		assert_eq!(
			judge("lead.created", SourceKind::Site, json!({"channel": "form", "message_ref": "aq-7k3f"}), Some("L-1")),
			Checked::Invalid(Invalid::new("properties.message_ref is not like \"AQ-7K3F\": 2–4 of A-Z, a dash, 4–8 of Crockford base32"))
		);
		assert!(matches!(
			judge("lead.created", SourceKind::Bot, json!({"channel": "telegram"}), Some("tg-1")),
			Checked::Registered(_)
		));
		assert_eq!(
			judge("lead.created", SourceKind::Bot, json!({"channel": "form"}), Some("tg-1")),
			Checked::Invalid(Invalid::new("a bot source writes lead.created only with channel whatsapp or telegram"))
		);
		assert_eq!(
			judge("lead.messaged", SourceKind::Bot, json!({"channel": "telegram", "messageRef": "AQ-7K3F"}), None),
			Checked::Registered(Fact::LeadMessaged {
				channel: Messenger::Telegram,
				message_ref: Some(MessageRef::parse("AQ-7K3F").unwrap()),
			})
		);
		assert_eq!(
			judge("lead.messaged", SourceKind::Bot, json!({"channel": "telegram"}), None),
			Checked::Invalid(Invalid::new("subject.lead_id or properties.message_ref is required for lead.messaged"))
		);
		assert_eq!(
			judge("lead.messaged", SourceKind::Panel, json!({"channel": "phone"}), Some("L-1")),
			Checked::Invalid(Invalid::new("properties.channel is not one of whatsapp, telegram"))
		);
		assert_eq!(
			judge("lead.messaged", SourceKind::Site, json!({"channel": "whatsapp"}), Some("L-1")),
			Checked::Invalid(Invalid::new("a site source may not write lead.messaged")),
			"a landing knows a link was opened, not that a message was sent"
		);
	}

	#[test]
	fn the_hash_ignores_spelling_and_key_order() {
		let camel = event();
		let mut snake = json!({});
		for (k, v) in camel.as_object().unwrap().iter().rev() {
			let k = match k.as_str() {
				"typeVersion" => "type_version",
				"occurredAt" => "occurred_at",
				other => other,
			};
			snake[k] = v.clone();
		}
		assert_eq!(decode(camel.clone(), now()).unwrap().canonical, decode(snake, now()).unwrap().canonical);
		let mut other = camel.clone();
		other["pii"]["phone"] = json!("+33 6 11 11 11 11");
		assert_ne!(decode(camel, now()).unwrap().canonical, decode(other, now()).unwrap().canonical);
	}

	#[test]
	fn envelope_rejections() {
		let cases: [(&str, Value, &str); 7] = [
			("schema", json!("sa.funnel.v2"), "schema is \"sa.funnel.v2\""),
			("id", json!("6ba7b810-9dad-11d1-80b4-00c04fd430c8"), "UUIDv1"),
			("occurredAt", json!("2026-09-30T12:06:00Z"), "in the future"),
			("source", json!({"kind": "email", "id": "x"}), "source.kind \"email\""),
			("subject", json!({"brandId": "Aquafix"}), "subject.brand_id"),
			("type", json!("Lead Created"), "dotted lowercase"),
			("extra", json!(1), "unknown field"),
		];
		for (field, value, want) in cases {
			let mut e = event();
			e[field] = value;
			let err = decode(e, now()).unwrap_err().0;
			assert!(err.contains(want), "{field}: {err}");
		}
	}

	#[test]
	fn the_registry() {
		let subject = decode(event(), now()).unwrap().envelope.subject;
		for (name, version, _) in REGISTERED {
			let got = check(&TypeKey::parse(name, *version).unwrap(), SourceKind::Panel, &json!({}), &subject);
			assert_ne!(got, Checked::Unregistered, "{name}@{version} is listed but not handled");
		}
		assert_eq!(
			check(&TypeKey::parse("lead.created", 2).unwrap(), SourceKind::Panel, &json!({"x": 1}), &subject),
			Checked::Unregistered
		);
		assert_eq!(check(&TypeKey::parse("review.new", 1).unwrap(), SourceKind::Panel, &json!({}), &subject), Checked::Unregistered);
		let Checked::Invalid(e) = check(&TypeKey::parse("call.logged", 1).unwrap(), SourceKind::Panel, &json!({"outcome": "voicemail"}), &subject) else {
			panic!("an unknown outcome passed")
		};
		assert_eq!(e.0, "properties.outcome is not one of answered, no_answer, wrong_number, later", "the vocabulary, not the value");
		let Checked::Invalid(e) = check(
			&TypeKey::parse("payment.received", 1).unwrap(),
			SourceKind::Panel,
			&json!({"billed": 100, "commission": 10, "currency": "EUR", "tip": 5}),
			&subject,
		) else {
			panic!("an unknown field passed")
		};
		assert!(e.0.contains("tip"), "{e}");
		assert_eq!(
			check(
				&TypeKey::parse("payment.received", 1).unwrap(),
				SourceKind::Panel,
				&json!({"billed": "12000", "commission": 1800, "currency": "EUR"}),
				&subject
			),
			Checked::Registered(Fact::payment(12_000, 1_800, "EUR").unwrap())
		);
		let payment = json!({"billed": 100, "commission": 10, "currency": "EUR"});
		assert_eq!(
			check(&TypeKey::parse("payment.received", 1).unwrap(), SourceKind::Site, &payment, &subject),
			Checked::Invalid(Invalid::new("a site source may not write payment.received")),
			"payments are entered by hand"
		);
	}

	#[test]
	fn the_retired_counts_still_pass_and_say_nothing() {
		let key = |name| TypeKey::parse(name, 1).unwrap();
		let mut subject = decode(event(), now()).unwrap().envelope.subject;
		let visits = json!({"day": "2026-09-30", "source": "google.com", "visits": "12", "revision": 2});
		assert_eq!(
			check(&key("site.metrics"), SourceKind::Posthog, &visits, &subject),
			Checked::Invalid(Invalid::new("a count names no lead and no job"))
		);
		subject.lead_id = None;
		assert_eq!(check(&key("site.metrics"), SourceKind::Posthog, &visits, &subject), Checked::Registered(Fact::RetiredCount));
		assert_eq!(
			check(&key("site.metrics"), SourceKind::Site, &visits, &subject),
			Checked::Invalid(Invalid::new("a site source may not write site.metrics")),
			"a landing never wrote its own counts"
		);
		let intents = json!({"day": "2026-09-30", "channel": "phone", "intents": 1, "revision": 1});
		assert_eq!(check(&key("contact.metrics"), SourceKind::Posthog, &intents, &subject), Checked::Registered(Fact::RetiredCount));
		subject.location_id = None;
		let arm = json!({"day": "2026-09-30", "experiment": "hero", "variant": "b", "exposures": 300, "formOpen": 4, "revision": 1});
		assert_eq!(check(&key("experiment.metrics"), SourceKind::Posthog, &arm, &subject), Checked::Registered(Fact::RetiredCount));
		let Checked::Invalid(e) = check(&key("experiment.metrics"), SourceKind::Posthog, &json!({"day": "2026-09-30", "clicks": 1}), &subject) else {
			panic!("an unknown field passed")
		};
		assert!(e.0.contains("clicks"), "the shape is still checked: {e}");
	}

	#[test]
	fn experiments_are_the_brands_and_their_writers() {
		let key = |name| TypeKey::parse(name, 1).unwrap();
		let mut subject = decode(event(), now()).unwrap().envelope.subject;
		let declared = json!({"experiments": [{"key": "lead_layout", "variants": ["a", "b"], "weights": [1, 1], "enabled": true, "holdout": 0.1, "summary": "One step converts better"}]});
		assert!(
			matches!(check(&key("experiments.declared"), SourceKind::Site, &declared, &subject), Checked::Invalid(_)),
			"names a lead"
		);
		subject.lead_id = None;
		subject.location_id = None;
		let Checked::Registered(Fact::ExperimentsDeclared(d)) = check(&key("experiments.declared"), SourceKind::Site, &declared, &subject) else {
			panic!("a declaration refused")
		};
		assert_eq!((d[0].key.as_str(), d[0].weights.as_slice(), d[0].holdout), ("lead_layout", &[1.0, 1.0][..], Some(0.1)));
		assert_eq!(
			check(&key("experiments.declared"), SourceKind::Panel, &declared, &subject),
			Checked::Invalid(Invalid::new("a panel source may not write experiments.declared"))
		);
		let nan = json!({"experiments": [{"key": "k", "variants": ["a", "b"], "weights": ["NaN", 1], "enabled": true}]});
		assert!(matches!(check(&key("experiments.declared"), SourceKind::Site, &nan, &subject), Checked::Invalid(_)));
		let Checked::Invalid(e) = check(&key("experiment.configured"), SourceKind::Panel, &json!({"key": "lead_layout", "enabled": false}), &subject) else {
			panic!("a change naming nobody passed")
		};
		assert!(e.0.contains("properties.by"), "{e}");
		let configured = json!({"key": "lead_layout", "weights": [3, 1], "reset": ["holdout"], "by": "ops@evinvest.ltd"});
		assert!(matches!(
			check(&key("experiment.configured"), SourceKind::Panel, &configured, &subject),
			Checked::Registered(Fact::ExperimentConfigured { .. })
		));
		assert_eq!(
			check(&key("experiment.configured"), SourceKind::Site, &configured, &subject),
			Checked::Invalid(Invalid::new("a site source may not write experiment.configured")),
			"a landing cannot set its own overrides"
		);
	}

	#[test]
	fn what_the_journal_would_refuse_is_rejected_per_event() {
		let reject = |f: &dyn Fn(&mut Value)| {
			let mut e = event();
			f(&mut e);
			decode(e, now()).unwrap_err().0
		};
		assert!(reject(&|e| e["typeVersion"] = json!(2_147_483_648u64)).contains("type_version"));
		assert!(reject(&|e| e["source"]["id"] = json!("a\u{0}b")).contains("source.id contains a NUL"));
		assert!(reject(&|e| e["properties"]["note"] = json!("x\u{0}")).contains("properties contains a NUL"));
		assert!(reject(&|e| e["properties"]["a\u{0}"] = json!(1)).contains("properties contains a NUL"));
		assert!(reject(&|e| e["pii"]["deep"] = json!([{"x": "\u{0}"}])).contains("pii contains a NUL"));
		assert!(reject(&|e| e["properties"]["blob"] = json!("x".repeat(MAX_OBJECT_BYTES))).contains("past the 16384 allowed"));
		let mut fits = event();
		fits["properties"]["blob"] = json!("x".repeat(MAX_OBJECT_BYTES - 64));
		assert!(decode(fits, now()).is_ok());
	}

	#[test]
	fn errors_do_not_quote_values() {
		let e = event();
		let mut bad = e.clone();
		bad["occurredAt"] = json!("+33 6 12 34 56 78");
		let msg = decode(bad, now()).unwrap_err().0;
		assert!(!msg.contains("+33"), "{msg}");
		let subject = decode(e, now()).unwrap().envelope.subject;
		let Checked::Invalid(msg) = check(
			&TypeKey::parse("payment.received", 1).unwrap(),
			SourceKind::Panel,
			&json!({"billed": "Jean Dupont", "commission": 1, "currency": "EUR"}),
			&subject,
		) else {
			panic!("a name passed for an amount")
		};
		assert!(!msg.0.contains("Jean"), "{msg}");
		let Checked::Invalid(msg) = check(
			&TypeKey::parse("payment.received", 1).unwrap(),
			SourceKind::Panel,
			&json!({"billed": 1, "commission": 1, "currency": "EUR", "phone": 1}),
			&subject,
		) else {
			panic!("an unknown field passed")
		};
		assert!(msg.0.contains("phone"), "field names stay: {msg}");
	}

	#[test]
	fn batches() {
		assert!(batch(br#"{"events":[{}]}"#).is_ok());
		for (body, want) in [
			(&b"[]"[..], "not a JSON object"),
			(b"{", "not JSON"),
			(br#"{"events":[]}"#, "1 to 500"),
			(br#"{"events":[], "more": 1}"#, "unknown field"),
			(br#"{}"#, "no events array"),
		] {
			assert!(batch(body).unwrap_err().0.contains(want), "{}", String::from_utf8_lossy(body));
		}
		let big = format!(r#"{{"events":[{}]}}"#, vec!["{}"; 501].join(","));
		assert!(batch(big.as_bytes()).is_err());
	}
}

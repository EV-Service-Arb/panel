//! `nix run .#gen`: the TS the front end would otherwise re-type from the Rust. The output is
//! committed, and the `generated` pre-commit hook re-runs this and re-stages it.

use std::path::Path;

use ev_lib::ts_gen::Ts;
use panel_core::{
	booking::{BookingMatch, BookingStatus, DayPart, Provider},
	event::SourceKind,
	fact::{LeadChannel, LeadFlow, LeadSuspect, Messenger},
	funnel::CONTACT_SLA,
	lead::Stage,
	pricing,
};
use panel_server::{
	access::AccessRequestDto,
	api::{CURRENCIES, IDEMPOTENCY_KEY},
	signin::Caller,
};
use sa_auth::Catalog;

fn main() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend/src");
	let [permissions, aliases] = Catalog::collect("sa", 0).ts();
	Ts::write(
		&root.join("entities/session/model/generated.ts"),
		&[Ts::types::<Caller>(), Ts::types::<AccessRequestDto>(), permissions, aliases],
	);
	Ts::write(
		&root.join("entities/lead/model/generated.ts"),
		&[
			Ts::Union {
				name: "STAGES",
				ty: "Stage",
				items: Stage::ALL.map(Stage::as_str).into(),
			},
			Ts::Union {
				name: "SUSPECTS",
				ty: "Suspect",
				items: LeadSuspect::ALL.map(LeadSuspect::as_str).into(),
			},
			Ts::Union {
				name: "CHANNELS",
				ty: "Channel",
				items: LeadChannel::ALL.map(LeadChannel::as_str).into(),
			},
			Ts::Union {
				name: "MANUAL_CHANNELS",
				ty: "ManualChannel",
				items: LeadChannel::MANUAL.map(LeadChannel::as_str).into(),
			},
			Ts::Union {
				name: "MESSENGERS",
				ty: "Messenger",
				items: Messenger::ALL.map(Messenger::as_str).into(),
			},
			Ts::Union {
				name: "FLOWS",
				ty: "Flow",
				items: LeadFlow::ALL.map(LeadFlow::as_str).into(),
			},
			Ts::Union {
				name: "BOOKING_STATUSES",
				ty: "BookingStatus",
				items: BookingStatus::ALL.map(BookingStatus::as_str).into(),
			},
			Ts::Union {
				name: "BOOKING_MATCHES",
				ty: "BookingMatch",
				items: BookingMatch::ALL.map(BookingMatch::as_str).into(),
			},
			Ts::Union {
				name: "DAY_PARTS",
				ty: "DayPart",
				items: DayPart::ALL.map(DayPart::as_str).into(),
			},
			Ts::Const {
				name: "QUOTE_CURRENCY",
				value: pricing::CURRENCY,
			},
			Ts::Value {
				name: "CONTACT_SLA_SECONDS",
				value: CONTACT_SLA.as_secs().into(),
			},
		],
	);
	Ts::write(
		&root.join("entities/source/model/generated.ts"),
		&[
			Ts::Union {
				name: "SOURCE_KINDS",
				ty: "SourceKind",
				items: SourceKind::ALL.map(SourceKind::as_str).into(),
			},
			Ts::Union {
				name: "KEYED_SOURCE_KINDS",
				ty: "KeyedSourceKind",
				items: SourceKind::ALL.into_iter().filter(|k| k.keyed()).map(SourceKind::as_str).collect(),
			},
		],
	);
	Ts::write(
		&root.join("shared/config/generated.ts"),
		&[
			Ts::Union {
				name: "CURRENCIES",
				ty: "Currency",
				items: CURRENCIES.into(),
			},
			Ts::Union {
				name: "BOOKING_PROVIDERS",
				ty: "BookingProvider",
				items: Provider::ALL.map(Provider::as_str).into(),
			},
			Ts::Union {
				name: "PAGE_PROVIDERS",
				ty: "PageProvider",
				items: Provider::ALL.into_iter().filter(|p| p.has_page()).map(Provider::as_str).collect(),
			},
		],
	);
	Ts::write(
		&root.join("shared/api/generated.ts"),
		&[Ts::Const {
			name: "IDEMPOTENCY_HEADER",
			value: IDEMPOTENCY_KEY,
		}],
	);
}

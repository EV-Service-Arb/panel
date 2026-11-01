//! `nix run .#gen`: the TS the front end would otherwise re-type from the Rust. The output is
//! committed, and the `generated` pre-commit hook re-runs this and re-stages it.

use std::path::Path;

use ev_lib::ts_gen::Ts;
use panel::tokens::Kind;
use panel_core::notify::Rule;
use panel_server::{
	access::AccessRequestDto,
	gmbs::{CandidatesDto, GmailsDto},
	signin::Caller,
	telegram::TelegramSettingsDto,
	tokens::{BalanceChangeDto, BalanceDto, LedgerDto, TokenHoldersDto, TokensDto, UsageDto},
};
use sa_auth::{Catalog, SourceKind};

fn main() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend/src");
	let [permissions, aliases] = Catalog::collect("sa", 0).ts();
	Ts::write(
		&root.join("entities/session/model/generated.ts"),
		&[Ts::types::<Caller>(), Ts::types::<AccessRequestDto>(), permissions, aliases],
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
	Ts::write(&root.join("entities/gmb/model/generated.ts"), &[Ts::types::<(GmailsDto, CandidatesDto)>()]);
	Ts::write(
		&root.join("entities/tokens/model/generated.ts"),
		&[
			Ts::types::<(TokensDto, LedgerDto, UsageDto, TokenHoldersDto, BalanceChangeDto, BalanceDto)>(),
			Ts::Union {
				name: "LEDGER_KINDS",
				ty: "LedgerKind",
				items: Kind::ALL.map(Kind::as_str).into(),
			},
		],
	);
	Ts::write(
		&root.join("entities/telegram/model/generated.ts"),
		&[
			Ts::types::<TelegramSettingsDto>(),
			Ts::Union {
				name: "RULES",
				ty: "Rule",
				items: Rule::ALL.map(Rule::as_str).into(),
			},
		],
	);
}

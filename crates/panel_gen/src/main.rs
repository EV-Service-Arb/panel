//! `nix run .#gen`: the TS the front end would otherwise re-type from the Rust. The output is
//! committed, and the `generated` pre-commit hook re-runs this and re-stages it.

use std::path::Path;

use ev_lib::ts_gen::Ts;
use panel_server::{access::AccessRequestDto, signin::Caller};
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
}

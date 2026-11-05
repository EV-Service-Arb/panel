//! `cargo r -p sa_auth --example keygen -- <kid>`: a fresh panel signing key (PANEL_ASSERTION_KEY),
//! and its public half for the services (PANEL_ASSERTION_KEYS).

use base64::{Engine, engine::general_purpose::STANDARD};

fn main() {
	let kid = std::env::args().nth(1).expect("usage: keygen <kid>");
	let mut seed = [0u8; 32];
	getrandom::fill(&mut seed).expect("the OS has randomness");
	let private = format!("{kid}:{}", STANDARD.encode(seed));
	let signer: sa_auth::Signer = private.parse().expect("a fresh key parses");
	println!("PANEL_ASSERTION_KEY={private}\nPANEL_ASSERTION_KEYS={}", signer.public());
}

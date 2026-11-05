//! How a source signs a batch, and how the panel checks it.
//!
//! ```text
//! x-sa-key-id:    <the source's key id>
//! x-sa-timestamp: <unix seconds when sent>
//! x-sa-signature: hex(HMAC-SHA256(secret, "sa-ingest/v1." + <x-sa-timestamp> + "." + <raw body>))
//! ```
//!
//! The timestamp is inside the MAC, not only beside it. A timestamp that travels only in
//! a header (as Didit's `X-Timestamp` does, which is why concierge's `kyc.rs` has to find a
//! signed copy in the body) can be re-stamped on a captured request for free, and the
//! replay window would defend nothing. Here re-stamping breaks the signature. The
//! `sa-ingest/v1.` prefix names what is signed, so a MAC made with the same secret for
//! anything else never passes for a batch.
//!
//! Deliveries more than [`REPLAY_WINDOW`] away from the panel's clock, either way, are
//! refused; inside it, a replay is harmless because every event is deduplicated by id.

use hmac::{Hmac, KeyInit, Mac};
use jiff::{SignedDuration, Timestamp};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// How far a delivery's timestamp may be from now.
pub const REPLAY_WINDOW: SignedDuration = SignedDuration::from_mins(5);

/// What every signed batch starts with, inside the MAC.
const DOMAIN: &[u8] = b"sa-ingest/v1.";

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SignatureError {
	#[error("x-sa-timestamp is not unix seconds")]
	MalformedTimestamp,
	#[error("x-sa-timestamp is outside the replay window")]
	Stale,
	#[error("x-sa-signature does not match")]
	Mismatch,
}

/// The signature a source sends, as lowercase hex.
pub fn sign(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
	hex::encode(mac(secret, timestamp, body).finalize().into_bytes())
}

/// Whether a delivery's timestamp is inside the window. Checked before anything else — before
/// the body is even read — because it is free and needs no key; the panel's clock is no
/// secret.
pub fn check_window(timestamp: &str, now: Timestamp) -> Result<(), SignatureError> {
	let sent: i64 = timestamp.trim().parse().map_err(|_| SignatureError::MalformedTimestamp)?;
	let sent = Timestamp::from_second(sent).map_err(|_| SignatureError::MalformedTimestamp)?;
	if now.duration_since(sent).abs() > REPLAY_WINDOW {
		return Err(SignatureError::Stale);
	}
	Ok(())
}

/// Checks a delivery: the window, then the signature.
///
/// The comparison is constant-time over the decoded digest, and a signature that is not 64
/// hex characters is a plain mismatch.
pub fn verify(secret: &[u8], timestamp: &str, signature: &str, body: &[u8], now: Timestamp) -> Result<(), SignatureError> {
	check_window(timestamp, now)?;
	let expected = mac(secret, timestamp, body).finalize().into_bytes();
	let presented = hex::decode(signature.trim()).map_err(|_| SignatureError::Mismatch)?;
	if presented.len() != expected.len() || !bool::from(presented.as_slice().ct_eq(expected.as_slice())) {
		return Err(SignatureError::Mismatch);
	}
	Ok(())
}

fn mac(secret: &[u8], timestamp: &str, body: &[u8]) -> Hmac<Sha256> {
	// HMAC takes a key of any length, so this cannot fail.
	let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC accepts any key length");
	mac.update(DOMAIN);
	mac.update(timestamp.as_bytes());
	mac.update(b".");
	mac.update(body);
	mac
}

#[cfg(test)]
mod tests {
	use super::*;

	const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
	const BODY: &[u8] = br#"{"events":[]}"#;

	fn now() -> Timestamp {
		Timestamp::from_second(1_790_762_400).unwrap()
	}

	#[test]
	fn a_good_signature_passes() {
		let ts = "1790762400";
		assert_eq!(verify(SECRET, ts, &sign(SECRET, ts, BODY), BODY, now()), Ok(()));
		assert_eq!(verify(SECRET, ts, &sign(SECRET, ts, BODY).to_uppercase(), BODY, now()), Ok(()), "hex case is not the secret");
	}

	#[test]
	fn anything_else_is_a_mismatch() {
		let ts = "1790762400";
		let sig = sign(SECRET, ts, BODY);
		assert_eq!(verify(b"another secret", ts, &sig, BODY, now()), Err(SignatureError::Mismatch));
		assert_eq!(verify(SECRET, ts, &sig, br#"{"events":[{}]}"#, now()), Err(SignatureError::Mismatch));
		assert_eq!(verify(SECRET, "1790762401", &sig, BODY, now()), Err(SignatureError::Mismatch), "re-stamping breaks it");
		assert_eq!(verify(SECRET, ts, &sig[..62], BODY, now()), Err(SignatureError::Mismatch));
		assert_eq!(verify(SECRET, ts, "zz", BODY, now()), Err(SignatureError::Mismatch));
		assert_eq!(verify(SECRET, ts, "", BODY, now()), Err(SignatureError::Mismatch));
	}

	#[test]
	fn the_window_is_five_minutes_either_way() {
		for (offset, ok) in [(-300, true), (300, true), (-301, false), (301, false)] {
			let ts = (1_790_762_400 + offset).to_string();
			let got = verify(SECRET, &ts, &sign(SECRET, &ts, BODY), BODY, now());
			assert_eq!(got.is_ok(), ok, "offset {offset}: {got:?}");
			if !ok {
				assert_eq!(got, Err(SignatureError::Stale));
			}
		}
		let ts = "yesterday";
		assert_eq!(verify(SECRET, ts, &sign(SECRET, ts, BODY), BODY, now()), Err(SignatureError::MalformedTimestamp));
		assert_eq!(check_window("1790762400", now()), Ok(()));
		assert_eq!(check_window("99999999999999999999", now()), Err(SignatureError::MalformedTimestamp));
	}

	/// A fixed vector, so a source written in another language can check its signing.
	#[test]
	fn the_domain_prefix_is_signed() {
		use hmac::{KeyInit, Mac};
		let mut bare = <Hmac<Sha256> as KeyInit>::new_from_slice(SECRET).unwrap();
		bare.update(b"1790762400.");
		bare.update(BODY);
		let bare = hex::encode(bare.finalize().into_bytes());
		assert_eq!(verify(SECRET, "1790762400", &bare, BODY, now()), Err(SignatureError::Mismatch), "a MAC without the prefix");
		assert_eq!(sign(SECRET, "1790762400", BODY), "8201ce64bf37d5332c8782bf19dba21d02573a6bf0f424dac260a9db83419b4f");
	}
}

//! Encryption at rest for what the database must not hold in the clear: a source's HMAC secret,
//! a session's tokens, a queued Telegram message.
//!
//! XChaCha20-Poly1305 with a random 24-byte nonce per seal, so there is no nonce
//! bookkeeping to get wrong (the same construction as banking's signer `key_vault`). What a
//! blob belongs to — the key id, the message — is bound as associated data, so a blob
//! copied onto another row does not open there.
//!
//! This protects a stolen dump or backup. It does not protect against the running
//! process, which holds the key.

// `chacha20poly1305 0.10` re-exports `generic-array 0.14`, whose `from_slice` is deprecated
// in favour of an API the crate has not moved to; the calls are right for this version.
#![allow(deprecated)]

use chacha20poly1305::{
	Key, XChaCha20Poly1305, XNonce,
	aead::{Aead, KeyInit, Payload},
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const NONCE_LEN: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SealError {
	#[error("PANEL_DATA_KEY must be 64 hex characters (32 bytes)")]
	KeyFormat,
	#[error("a sealed blob does not open: wrong data key, or it was tampered with or moved")]
	Open,
	#[error("encryption failed")]
	Seal,
	#[error("the OS random source failed")]
	Random,
}

/// The data key, from the environment. Wiped on drop.
pub struct DataKey {
	key: Zeroizing<[u8; 32]>,
}

impl DataKey {
	pub fn from_hex(hex_key: &str) -> Result<Self, SealError> {
		let raw = Zeroizing::new(hex::decode(hex_key.trim()).map_err(|_| SealError::KeyFormat)?);
		let key: [u8; 32] = raw.as_slice().try_into().map_err(|_| SealError::KeyFormat)?;
		Ok(Self { key: Zeroizing::new(key) })
	}

	/// A fresh random key, as `panel gen-data-key` prints it.
	pub fn generate_hex() -> Result<String, SealError> {
		let mut key = Zeroizing::new([0u8; 32]);
		getrandom::fill(key.as_mut_slice()).map_err(|_| SealError::Random)?;
		Ok(hex::encode(key.as_slice()))
	}

	/// Names the key without revealing it: stored beside every blob, so a blob sealed under
	/// another key is known for one before anything tries to open it.
	pub fn fingerprint(&self) -> [u8; 32] {
		let mut h = Sha256::new();
		h.update(b"sa-panel/data-key-fp/v1");
		h.update(self.key.as_slice());
		h.finalize().into()
	}

	/// The key the Telegram buttons' callback data is signed with
	/// (`panel_core::notify::callback_data`).
	pub fn telegram_callback_key(&self) -> Zeroizing<[u8; 32]> {
		self.derive(b"sa-panel/tg-callback-key/v1")
	}

	/// A key for one purpose, named by `label`, that says nothing of this one or the others.
	fn derive(&self, label: &[u8]) -> Zeroizing<[u8; 32]> {
		let mut derived = Zeroizing::new([0u8; 32]);
		let mut h = Sha256::new();
		h.update(label);
		h.update(self.key.as_slice());
		derived.copy_from_slice(&h.finalize());
		derived
	}

	/// `nonce(24) || ciphertext+tag`.
	pub fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
		let mut nonce = [0u8; NONCE_LEN];
		getrandom::fill(&mut nonce).map_err(|_| SealError::Random)?;
		let ct = self.cipher().encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad }).map_err(|_| SealError::Seal)?;
		let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
		out.extend_from_slice(&nonce);
		out.extend_from_slice(&ct);
		Ok(out)
	}

	pub fn open(&self, aad: &[u8], blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError> {
		if blob.len() < NONCE_LEN {
			return Err(SealError::Open);
		}
		let (nonce, ct) = blob.split_at(NONCE_LEN);
		self.cipher()
			.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
			.map(Zeroizing::new)
			.map_err(|_| SealError::Open)
	}

	fn cipher(&self) -> XChaCha20Poly1305 {
		XChaCha20Poly1305::new(Key::from_slice(self.key.as_slice()))
	}
}

impl std::fmt::Debug for DataKey {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("DataKey").field("fingerprint", &hex::encode(&self.fingerprint()[..4])).finish_non_exhaustive()
	}
}

/// Associated data of a source's HMAC secret.
pub fn source_secret_aad(key_id: &str) -> Vec<u8> {
	[b"sa-panel/source-secret/v1/".as_slice(), key_id.as_bytes()].concat()
}

/// Associated data of a queued Telegram message's text. A chat without a topic binds as it did
/// before topics, so what was queued then still opens.
pub fn telegram_text_aad(rule: &str, event_id: uuid::Uuid, chat_id: i64, topic_id: Option<i64>) -> Vec<u8> {
	let topic = topic_id.map(i64::to_be_bytes);
	[
		b"sa-panel/tg-text/v1/".as_slice(),
		rule.as_bytes(),
		b"/",
		event_id.as_bytes(),
		&chat_id.to_be_bytes(),
		topic.as_ref().map_or(&[][..], |t| t.as_slice()),
	]
	.concat()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip_and_binding() {
		let key = DataKey::from_hex(&DataKey::generate_hex().unwrap()).unwrap();
		let blob = key.seal(b"row-1", b"+33 6 00 00 00 00").unwrap();
		assert_eq!(key.open(b"row-1", &blob).unwrap().as_slice(), b"+33 6 00 00 00 00");
		assert_eq!(key.open(b"row-2", &blob).unwrap_err(), SealError::Open, "moved to another row");
		let other = DataKey::from_hex(&DataKey::generate_hex().unwrap()).unwrap();
		assert_eq!(other.open(b"row-1", &blob).unwrap_err(), SealError::Open);
		assert_ne!(key.fingerprint(), other.fingerprint());
		assert_ne!(key.seal(b"row-1", b"x").unwrap(), key.seal(b"row-1", b"x").unwrap(), "a fresh nonce each time");
		assert!(!format!("{key:?}").contains(&hex::encode(key.key.as_slice())));
	}

	#[test]
	fn key_format() {
		assert_eq!(DataKey::from_hex("abcd").unwrap_err(), SealError::KeyFormat);
		assert_eq!(DataKey::from_hex(&"zz".repeat(32)).unwrap_err(), SealError::KeyFormat);
	}
}

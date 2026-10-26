//! Service-Arb's permissions (the `sa` namespace concierge resolves), and the assertion the
//! panel signs for each request it forwards to a service behind it: who is calling, what of
//! that service they may do, and which one request it is for.

use std::{collections::BTreeMap, str::FromStr};

use base64::{
	Engine,
	engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use concierge_iam::alias;
pub use concierge_iam::{Catalog, Permission, PermissionSet};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:work")]
pub enum Work {
	Read,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:work:leads")]
pub enum Leads {
	Edit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:work:pii")]
pub enum Pii {
	See,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:work:places")]
pub enum Places {
	Edit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:work:pricing")]
pub enum Pricing {
	Edit,
}

/// Issue an invoice through banking (`sa:work:invoices:create`). Reading the invoices needs
/// only the work section: an operator chasing a payment sees whether it came.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:work:invoices")]
pub enum Invoices {
	Create,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:analysis")]
pub enum Analysis {
	Read,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:analysis:experiments")]
pub enum Experiments {
	Edit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:admin:sources")]
pub enum Sources {
	Manage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:review_archive:archive")]
pub enum Archive {
	Operate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:review_archive:tokens")]
pub enum Tokens {
	Grant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:review_archive:members")]
pub enum Members {
	ActAs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Permission)]
#[permission("sa:playbook:mcp")]
pub enum Mcp {
	Use,
}

alias!(pub SA_OPERATOR = "sa:operator", [Work::Read, Leads::Edit, Pii::See, Analysis::Read]);
// Held by every member of the `sa` scope, without a grant (concierge's `<namespace>:everyone`).
alias!(pub SA_EVERYONE = "sa:everyone", [Mcp::Use]);
alias!(
	pub SA_ADMIN = "sa:admin",
	[
		Work::Read,
		Leads::Edit,
		Pii::See,
		Places::Edit,
		Pricing::Edit,
		Invoices::Create,
		Analysis::Read,
		Experiments::Edit,
		Sources::Manage,
		Archive::Operate,
		Tokens::Grant,
		Members::ActAs,
		Mcp::Use,
	],
	delegates [SA_OPERATOR]
);

/// The request header the assertion rides in, panel → service.
pub const HEADER: &str = "x-sa-assertion";

/// How long an assertion lives, in seconds: one request's way through the panel.
pub const TTL: i64 = 60;

/// A service behind the panel: the assertion's audience, and the permission slice it gets.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
	ReviewArchive,
	Playbook,
}

impl Service {
	/// `sa:<service>:`, the prefix of every permission the service is told about.
	pub fn prefix(self) -> &'static str {
		match self {
			Self::ReviewArchive => "sa:review_archive:",
			Self::Playbook => "sa:playbook:",
		}
	}
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Assertion {
	pub aud: Service,
	/// The concierge user id.
	pub sub: String,
	pub email: String,
	pub email_verified: bool,
	pub name: String,
	/// The caller's permissions under [`Service::prefix`] of `aud`, and no others.
	pub permissions: PermissionSet,
	pub method: String,
	pub path: String,
	pub exp: i64,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum Refused {
	#[error("not a compact JWS")]
	Malformed,
	#[error("signed with a key this service does not hold")]
	UnknownKey,
	#[error("the signature does not verify")]
	Signature,
	#[error("addressed to another service")]
	Audience,
	#[error("issued for another request")]
	Request,
	#[error("expired, or living longer than an assertion may")]
	Expiry,
	#[error("carries a permission outside its service")]
	Slice,
}

#[derive(Deserialize, Serialize)]
struct Header {
	alg: String,
	kid: String,
}

/// The panel's signing key: `<kid>:<base64 of the 32-byte Ed25519 seed>`.
pub struct Signer {
	kid: String,
	key: SigningKey,
}

impl Signer {
	/// `<kid>:<base64 of the public key>`, what the services are given.
	pub fn public(&self) -> String {
		format!("{}:{}", self.kid, STANDARD.encode(self.key.verifying_key().as_bytes()))
	}

	pub fn sign(&self, assertion: &Assertion) -> String {
		assert!(
			assertion.permissions.iter().all(|p| p.starts_with(assertion.aud.prefix())),
			"the panel slices the permissions before signing"
		);
		let header = serde_json::to_vec(&Header {
			alg: "EdDSA".into(),
			kid: self.kid.clone(),
		})
		.expect("a struct of strings serializes");
		let claims = serde_json::to_vec(assertion).expect("an assertion serializes");
		let signed = format!("{}.{}", URL_SAFE_NO_PAD.encode(header), URL_SAFE_NO_PAD.encode(claims));
		let signature = self.key.sign(signed.as_bytes());
		format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
	}
}

impl FromStr for Signer {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, String> {
		let (kid, seed) = split_key(s)?;
		Ok(Self {
			kid,
			key: SigningKey::from_bytes(&seed),
		})
	}
}

/// The panel's public keys a service accepts: `<kid>:<base64 key>` comma-separated, more than
/// one while a key rotates.
#[derive(Clone, Debug)]
pub struct Keys(BTreeMap<String, VerifyingKey>);

impl FromStr for Keys {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, String> {
		let mut keys = BTreeMap::new();
		for entry in s.split(',').map(str::trim).filter(|e| !e.is_empty()) {
			let (kid, raw) = split_key(entry)?;
			let key = VerifyingKey::from_bytes(&raw).map_err(|e| format!("key `{kid}` is not an Ed25519 public key: {e}"))?;
			if keys.insert(kid.clone(), key).is_some() {
				return Err(format!("key `{kid}` is listed twice"));
			}
		}
		match keys.is_empty() {
			true => Err("no key".into()),
			false => Ok(Self(keys)),
		}
	}
}

fn split_key(s: &str) -> Result<(String, [u8; 32]), String> {
	let (kid, b64) = s.split_once(':').ok_or("expected `<kid>:<base64>`")?;
	if kid.is_empty() {
		return Err("the key id is empty".into());
	}
	let raw = STANDARD.decode(b64.trim()).map_err(|e| format!("key `{kid}`: {e}"))?;
	let raw: [u8; 32] = raw.try_into().map_err(|_| format!("key `{kid}` is not 32 bytes"))?;
	Ok((kid.to_owned(), raw))
}

/// The assertion on one request, if the panel signed it for `aud` and for exactly this
/// `method` and `path`, and it is alive at `now` (unix seconds).
pub fn verify(keys: &Keys, token: &str, aud: Service, method: &str, path: &str, now: i64) -> Result<Assertion, Refused> {
	let mut parts = token.split('.');
	let (Some(header), Some(claims), Some(signature), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
		return Err(Refused::Malformed);
	};
	let decode = |part: &str| URL_SAFE_NO_PAD.decode(part).map_err(|_| Refused::Malformed);
	let parsed: Header = serde_json::from_slice(&decode(header)?).map_err(|_| Refused::Malformed)?;
	if parsed.alg != "EdDSA" {
		return Err(Refused::Malformed);
	}
	let key = keys.0.get(&parsed.kid).ok_or(Refused::UnknownKey)?;
	let signature = Signature::from_slice(&decode(signature)?).map_err(|_| Refused::Malformed)?;
	key.verify_strict(format!("{header}.{claims}").as_bytes(), &signature).map_err(|_| Refused::Signature)?;
	let assertion: Assertion = serde_json::from_slice(&decode(claims)?).map_err(|_| Refused::Malformed)?;
	if assertion.aud != aud {
		return Err(Refused::Audience);
	}
	if assertion.method != method || assertion.path != path {
		return Err(Refused::Request);
	}
	if assertion.exp <= now || assertion.exp > now + TTL + 5 {
		return Err(Refused::Expiry);
	}
	if !assertion.permissions.iter().all(|p| p.starts_with(aud.prefix())) {
		return Err(Refused::Slice);
	}
	Ok(assertion)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn signer(kid: &str) -> Signer {
		let mut seed = [0u8; 32];
		getrandom::fill(&mut seed).unwrap();
		format!("{kid}:{}", STANDARD.encode(seed)).parse().unwrap()
	}

	fn assertion(now: i64) -> Assertion {
		Assertion {
			aud: Service::ReviewArchive,
			sub: "0192".into(),
			email: "a@b.c".into(),
			email_verified: true,
			name: "A".into(),
			permissions: [Archive::Operate.as_str()].into_iter().collect(),
			method: "POST".into(),
			path: "/targets".into(),
			exp: now + TTL,
		}
	}

	#[test]
	fn an_assertion_holds_for_its_one_request_only() {
		let (panel, other) = (signer("k1"), signer("k2"));
		let keys: Keys = format!("{}, {}", panel.public(), other.public()).parse().unwrap();
		let now = 1_000;
		let token = panel.sign(&assertion(now));
		let check = |aud, method, path, now| verify(&keys, &token, aud, method, path, now);
		assert_eq!(check(Service::ReviewArchive, "POST", "/targets", now), Ok(assertion(now)));
		assert_eq!(check(Service::Playbook, "POST", "/targets", now), Err(Refused::Audience));
		assert_eq!(check(Service::ReviewArchive, "GET", "/targets", now), Err(Refused::Request));
		assert_eq!(check(Service::ReviewArchive, "POST", "/targets/1", now), Err(Refused::Request));
		assert_eq!(check(Service::ReviewArchive, "POST", "/targets", now + TTL), Err(Refused::Expiry));

		let only_other: Keys = other.public().parse().unwrap();
		assert_eq!(verify(&only_other, &token, Service::ReviewArchive, "POST", "/targets", now), Err(Refused::UnknownKey));
		let impostor = signer("k1").sign(&assertion(now));
		assert_eq!(verify(&keys, &impostor, Service::ReviewArchive, "POST", "/targets", now), Err(Refused::Signature));
		let mut forever = assertion(now);
		forever.exp = now + 3600;
		assert_eq!(verify(&keys, &panel.sign(&forever), Service::ReviewArchive, "POST", "/targets", now), Err(Refused::Expiry));
	}

	#[test]
	fn admin_holds_every_permission_and_delegates_operator() {
		let catalog = concierge_iam::Catalog::collect("sa", 1);
		assert_eq!(catalog.aliases["sa:admin"], catalog.permissions, "sa:admin holds every sa permission");
		assert_eq!(catalog.delegations["sa:admin"], ["sa:operator".to_owned()].into());
	}
}

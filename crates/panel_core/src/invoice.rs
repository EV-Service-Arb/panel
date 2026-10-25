//! An invoice the panel issues through banking's ScopeLedger (`scope.v1.InvoiceService`, the
//! protos in `contracts/proto/scope`): the state the panel stores, and the checks on what an
//! operator types before anything reaches banking.
//!
//! Banking is the authority on an invoice: its id, its pay page, whether it was paid. The panel
//! keeps a row per invoice it asked for, so a retry finds the first attempt and the list needs
//! no call to banking.

use uuid::Uuid;

use crate::Invalid;

/// The concierge tenant (banking scope) the panel's invoices live in.
pub const SCOPE: &str = "sa";
/// The scope account every invoice pays into. Its prefix `invoice:` is the panel's grant in
/// banking's grants file (`(sa-panel client, sa, invoice:)`).
pub const ACCOUNT_LABEL: &str = "invoice:sa";
/// The type half of the `subject_ref` banking echoes in the invoice and its money events:
/// `sa_invoice:<the panel's row id>`.
pub const SUBJECT_REF_TYPE: &str = "sa_invoice";
/// What banking's amounts are in.
pub const CURRENCY: &str = "USDT";
/// Banking refuses a longer memo (`validate_memo`, counted in characters).
pub const MAX_MEMO_CHARS: usize = 140;
/// Banking refuses an `expires_at` further ahead than this.
pub const MAX_EXPIRES_IN_DAYS: u32 = 30;
/// The panel's own bound on an amount typed by hand: whole USDT up to 999 999 999, and at
/// most 6 decimals (a micro-USDT). Banking would take 18; nobody bills a fraction finer.
pub const MAX_INTEGER_DIGITS: usize = 9;
pub const MAX_FRACTION_DIGITS: usize = 6;

/// Where an invoice stands, as the panel stores it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvoiceState {
	/// The panel has the row, banking has not answered yet (or the answer was lost): a retry
	/// with the same idempotency key sends the stored terms again.
	Creating,
	/// Payable on banking's pay page.
	Open,
	/// The payer paid and the ledger has not posted it yet. Not a payment: it ends `paid`, or
	/// falls back to `open` if the ledger refuses it. Never fulfil on it.
	Processing,
	/// The ledger took the payment (`INVOICE_STATE_PAID`, `KIND_INVOICE_PAID`). Final.
	Paid,
	Cancelled,
	Expired,
}

impl InvoiceState {
	pub const ALL: [Self; 6] = [Self::Creating, Self::Open, Self::Processing, Self::Paid, Self::Cancelled, Self::Expired];

	pub fn as_str(self) -> &'static str {
		match self {
			Self::Creating => "creating",
			Self::Open => "open",
			Self::Processing => "processing",
			Self::Paid => "paid",
			Self::Cancelled => "cancelled",
			Self::Expired => "expired",
		}
	}

	pub fn parse(raw: &str) -> Result<Self, Invalid> {
		Self::ALL
			.into_iter()
			.find(|s| s.as_str() == raw)
			.ok_or_else(|| Invalid::new("invoice state is not one of creating, open, processing, paid, cancelled, expired"))
	}

	/// Nothing banking says later moves an invoice out of these.
	pub fn is_final(self) -> bool {
		match self {
			Self::Paid | Self::Cancelled | Self::Expired => true,
			Self::Creating | Self::Open | Self::Processing => false,
		}
	}

	/// The state to store when banking reports `reported` for an invoice stored as `self`. A
	/// paid invoice stays paid, whatever comes after: banking never takes a payment back, and
	/// an answer that seems to is a stale read, not news.
	pub fn after(self, reported: Self) -> Self {
		if self == Self::Paid { self } else { reported }
	}
}

/// `raw` as banking's canonical decimal (`1000`, `0.5`, `12.05`): digits, an optional point
/// and fraction, no sign or exponent, no leading zeros but a lone `0`, no trailing zeros after
/// the point. Strictly positive, within [`MAX_INTEGER_DIGITS`] and [`MAX_FRACTION_DIGITS`].
pub fn canonical_amount(raw: &str) -> Result<String, Invalid> {
	let bad = || {
		Invalid::new(format!(
			"amount is a positive {CURRENCY} amount like 12.5, at most {MAX_INTEGER_DIGITS} digits before the point and {MAX_FRACTION_DIGITS} after"
		))
	};
	let raw = raw.trim();
	let (int, frac) = raw.split_once('.').unwrap_or((raw, ""));
	if (int.is_empty() && frac.is_empty()) || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
		return Err(bad());
	}
	let int = int.trim_start_matches('0');
	let frac = frac.trim_end_matches('0');
	if int.len() > MAX_INTEGER_DIGITS || frac.len() > MAX_FRACTION_DIGITS || (int.is_empty() && frac.is_empty()) {
		return Err(bad());
	}
	let int = if int.is_empty() { "0" } else { int };
	Ok(if frac.is_empty() { int.to_owned() } else { format!("{int}.{frac}") })
}

/// The memo the payer reads: at most [`MAX_MEMO_CHARS`] characters, no control characters.
/// Banking checks it again, and also refuses a link in it.
pub fn check_memo(memo: &str) -> Result<(), Invalid> {
	if memo.chars().count() > MAX_MEMO_CHARS {
		return Err(Invalid::new(format!("memo is longer than {MAX_MEMO_CHARS} characters")));
	}
	if memo.chars().any(char::is_control) {
		return Err(Invalid::new("memo is plain text on one line, without control characters"));
	}
	Ok(())
}

/// Days until the invoice lapses: 1 to [`MAX_EXPIRES_IN_DAYS`].
pub fn check_expires_in_days(days: u32) -> Result<(), Invalid> {
	if (1..=MAX_EXPIRES_IN_DAYS).contains(&days) {
		Ok(())
	} else {
		Err(Invalid::new(format!("expires_in_days is 1 to {MAX_EXPIRES_IN_DAYS}")))
	}
}

/// `sa_invoice:<id>`, the form banking requires (a type, a colon, a lowercase hyphenated UUID).
pub fn subject_ref(id: Uuid) -> String {
	format!("{SUBJECT_REF_TYPE}:{}", id.hyphenated())
}

/// The idempotency key banking gets for the invoice the panel's row `id` stands for: one per
/// row, so a retry of the same panel request is a repeat at banking too.
pub fn ledger_key(id: Uuid) -> String {
	format!("sa-panel:invoice:{}", id.hyphenated())
}

/// The idempotency key of opening [`ACCOUNT_LABEL`]: one for good, as the account is.
pub const OPEN_ACCOUNT_KEY: &str = "sa-panel:open-account:invoice:sa";

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn amounts_come_out_canonical() {
		for (raw, canonical) in [
			("12.5", "12.5"),
			("12.50", "12.5"),
			("0012.050", "12.05"),
			("1000", "1000"),
			("1000.", "1000"),
			(".5", "0.5"),
			("0.000001", "0.000001"),
			(" 7 ", "7"),
			("999999999.999999", "999999999.999999"),
		] {
			assert_eq!(canonical_amount(raw).as_deref(), Ok(canonical), "{raw}");
		}
		for raw in ["", ".", "0", "0.0", "00", "-1", "+1", "1e3", "1,5", "1.2.3", "0.0000001", "1000000000", "١٢", "1 000", "NaN"] {
			assert!(canonical_amount(raw).is_err(), "{raw:?} is refused");
		}
	}

	#[test]
	fn memos_are_bounded_plain_text() {
		assert!(check_memo("").is_ok());
		assert!(check_memo(&"я".repeat(MAX_MEMO_CHARS)).is_ok(), "counted in characters, not bytes");
		assert!(check_memo(&"я".repeat(MAX_MEMO_CHARS + 1)).is_err());
		assert!(check_memo("two\nlines").is_err());
	}

	#[test]
	fn expiry_is_one_to_thirty_days() {
		assert!(check_expires_in_days(0).is_err());
		assert!(check_expires_in_days(1).is_ok());
		assert!(check_expires_in_days(30).is_ok());
		assert!(check_expires_in_days(31).is_err());
	}

	#[test]
	fn states_round_trip_and_paid_is_never_left() {
		for s in InvoiceState::ALL {
			assert_eq!(InvoiceState::parse(s.as_str()), Ok(s));
		}
		assert!(InvoiceState::parse("PAID").is_err());
		for s in InvoiceState::ALL {
			assert_eq!(InvoiceState::Paid.after(s), InvoiceState::Paid, "{s:?}");
		}
		assert_eq!(InvoiceState::Open.after(InvoiceState::Processing), InvoiceState::Processing);
		assert_eq!(InvoiceState::Processing.after(InvoiceState::Open), InvoiceState::Open, "a refused payment falls back to open");
	}

	#[test]
	fn the_subject_ref_has_banking_s_shape() {
		let id = Uuid::parse_str("0192F0C1-7B3A-7C2E-9D4F-1A2B3C4D5E6F").unwrap();
		assert_eq!(subject_ref(id), "sa_invoice:0192f0c1-7b3a-7c2e-9d4f-1a2b3c4d5e6f");
		assert_eq!(ledger_key(id), "sa-panel:invoice:0192f0c1-7b3a-7c2e-9d4f-1a2b3c4d5e6f");
	}
}

//! A person's tokens: how a balance renews, and how a charge falls on its payers. Balances are a
//! ledger's sum; this is the arithmetic, the ledger is the engine's.

use jiff::{SignedDuration, Timestamp};

/// How balances renew.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Renewal {
	/// Added per day, while a balance is under `cap`.
	pub daily: i64,
	/// Renewal stops here; tokens added or granted above it stay.
	pub cap: i64,
	/// A person's first balance, in `cap`s.
	pub start_cap_multiple: f64,
}

impl Renewal {
	pub fn new(daily: i64, cap: i64, start_cap_multiple: f64) -> Result<Self, crate::Invalid> {
		if daily < 0 || cap < 0 {
			return Err(crate::Invalid::new(format!("TOKENS_DAILY ({daily}) and TOKENS_CAP ({cap}) are 0 or more")));
		}
		if !start_cap_multiple.is_finite() || start_cap_multiple < 0. {
			return Err(crate::Invalid::new(format!("TOKENS_START_CAP_MULTIPLE ({start_cap_multiple}) is a finite multiple, 0 or more")));
		}
		Ok(Self { daily, cap, start_cap_multiple })
	}

	/// A person's first balance.
	pub fn start(&self) -> i64 {
		(self.cap as f64 * self.start_cap_multiple).round() as i64
	}
}

/// What renewal adds to `balance` at `now`, renewed last at `last`, and the time it is renewed
/// up to: whole days only, so asking twice in a day adds once.
pub fn accrue(balance: i64, last: Timestamp, now: Timestamp, daily: i64, cap: i64) -> (i64, Timestamp) {
	let days = now.duration_since(last).as_secs() / 86_400;
	if days <= 0 {
		return (0, last);
	}
	let delta = (days * daily).min(cap - balance).max(0);
	(delta, last + SignedDuration::from_hours(24 * days))
}

/// `cost` over `payers` (`(who, balance)`): equal shares, none past its payer's balance, what
/// one cannot pay going to the others. A cost past every balance together is charged up to them.
pub fn split<W: Clone>(cost: i64, payers: &[(W, i64)]) -> Vec<(W, i64)> {
	assert!(cost >= 0 && payers.iter().all(|(_, b)| *b >= 0), "costs and balances are never negative");
	let mut by_balance: Vec<&(W, i64)> = payers.iter().collect();
	by_balance.sort_by_key(|(_, b)| *b);
	let mut left = cost;
	let n = by_balance.len() as i64;
	by_balance
		.into_iter()
		.zip(0..)
		.map(|((who, balance), i)| {
			let share = (left + (n - i) - 1) / (n - i);
			let paid = share.min(*balance);
			left -= paid;
			(who.clone(), paid)
		})
		.filter(|(_, paid)| *paid > 0)
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn day(n: i64) -> Timestamp {
		Timestamp::from_second(1_790_000_000 + n * 86_400).unwrap()
	}

	#[test]
	fn renewal_is_by_whole_days_up_to_the_cap() {
		assert_eq!(accrue(0, day(0), day(1) - SignedDuration::from_secs(1), 15, 300), (0, day(0)));
		assert_eq!(accrue(0, day(0), day(3) + SignedDuration::from_hours(5), 15, 300), (45, day(3)));
		assert_eq!(accrue(290, day(0), day(30), 15, 300), (10, day(30)));
		assert_eq!(accrue(500, day(0), day(2), 15, 300), (0, day(2)), "bought tokens above the cap stay");
	}

	#[test]
	fn the_poorer_payer_pays_what_it_has_and_the_rest_falls_on_the_others() {
		let payers = |bs: &[i64]| bs.iter().enumerate().map(|(i, b)| (format!("m{i}"), *b)).collect::<Vec<_>>();
		assert_eq!(split(20, &payers(&[5, 100])), [("m0".to_owned(), 5), ("m1".to_owned(), 15)]);
		assert_eq!(split(21, &payers(&[100, 100, 100])).iter().map(|(_, p)| p).sum::<i64>(), 21);
		assert_eq!(split(50, &payers(&[10, 20])), [("m0".to_owned(), 10), ("m1".to_owned(), 20)], "past every balance");
		assert!(split(0, &payers(&[10])).is_empty());
	}
}

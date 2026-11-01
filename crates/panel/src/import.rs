//! review_archive's people, gmails, GMBs, ledger and pending kick-backs, as its
//! `scripts/export_members.sql` prints them, into the conductor at the cutover. One transaction,
//! ids kept; it opens tokens and GMBs (see [`crate::tokens`]). A fresh install imports an empty
//! document.

use std::collections::BTreeSet;

use eyre::WrapErr;
use jiff::Timestamp;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
	Panel,
	store::{
		gmbs as gmbs_db,
		tokens::{self as db, Charged, Entry, Owed, REVIEW_ARCHIVE},
		users::{self, User},
	},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
	pub people: Vec<Person>,
	pub gmails: Vec<ExportGmail>,
	pub gmbs: Vec<ExportGmb>,
	pub ledger: Vec<ExportEntry>,
	pub kickbacks: Vec<ExportKickback>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
	pub sub: Option<Uuid>,
	pub email: String,
	pub name: String,
	pub first_seen: Timestamp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportGmail {
	pub id: i64,
	pub sub: Option<Uuid>,
	pub email: String,
	pub gmail: String,
	pub created_at: Timestamp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportGmb {
	pub id: i64,
	pub gmail_id: i64,
	pub place_id: String,
	pub name: Option<String>,
	pub address: Option<String>,
	pub created_at: Timestamp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportEntry {
	pub sub: Option<Uuid>,
	pub email: String,
	pub at: Timestamp,
	pub delta: i64,
	pub kind: String,
	pub run_id: Option<i64>,
	pub label: Option<String>,
	pub by_email: Option<String>,
	pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportKickback {
	pub sub: Option<Uuid>,
	pub email: String,
	pub at: Timestamp,
	pub amount: i64,
	pub run_id: Option<i64>,
	pub label: Option<String>,
	pub by_email: Option<String>,
	pub note: Option<String>,
}

/// A run's charge or kick-back, as the ledger names it.
fn charged(run_id: Option<i64>, label: Option<String>) -> eyre::Result<Option<Charged>> {
	match run_id {
		Some(run) => Ok(Some(Charged {
			service: REVIEW_ARCHIVE.to_owned(),
			key: format!("run:{run}"),
			label,
		})),
		None => {
			eyre::ensure!(label.is_none(), "a label {label:?} without a run");
			Ok(None)
		}
	}
}

impl Panel {
	/// Writes `export` and opens tokens and GMBs. Refused before anything is written when a
	/// person has no sub (every such email listed), or when tokens are open already or anything
	/// was written to them.
	pub async fn import_review_archive(&self, export: Export) -> eyre::Result<()> {
		let subless: BTreeSet<&str> = export
			.people
			.iter()
			.filter(|p| p.sub.is_none())
			.map(|p| p.email.as_str())
			.chain(export.gmails.iter().filter(|g| g.sub.is_none()).map(|g| g.email.as_str()))
			.chain(export.ledger.iter().filter(|e| e.sub.is_none()).map(|e| e.email.as_str()))
			.chain(export.kickbacks.iter().filter(|k| k.sub.is_none()).map(|k| k.email.as_str()))
			.collect();
		eyre::ensure!(
			subless.is_empty(),
			"people without a sub (they sign in to the conductor first): {}",
			subless.into_iter().collect::<Vec<_>>().join(", ")
		);
		let sub = |s: Option<Uuid>| s.expect("checked above");
		let people: BTreeSet<Uuid> = export.people.iter().map(|p| sub(p.sub)).collect();
		let named = export
			.gmails
			.iter()
			.map(|g| g.sub)
			.chain(export.ledger.iter().map(|e| e.sub))
			.chain(export.kickbacks.iter().map(|k| k.sub));
		for s in named {
			eyre::ensure!(people.contains(&sub(s)), "{} is named but not among the people", sub(s));
		}

		let mut tx = self.store.begin_write().await?;
		eyre::ensure!(db::cursor(&mut tx).await?.is_none(), "tokens and GMBs are open already: imported before");
		eyre::ensure!(db::untouched(&mut tx).await?, "gmails, GMBs, the ledger or kick-backs hold rows already");
		for p in &export.people {
			let user = User {
				user_id: sub(p.sub),
				email: p.email.clone(),
				name: p.name.clone(),
			};
			users::seen(&mut tx, &user, p.first_seen, p.first_seen).await?;
		}
		for g in &export.gmails {
			let added = gmbs_db::add_gmail(&mut tx, Some(g.id), sub(g.sub), &g.gmail, g.created_at).await?;
			eyre::ensure!(added.is_some_and(|a| a.id == g.id), "gmail {} ({}) clashes with another", g.id, g.gmail);
		}
		for b in &export.gmbs {
			let place = sa_places::Place {
				place_id: b.place_id.clone(),
				name: b.name.clone(),
				address: b.address.clone(),
			};
			let added = gmbs_db::add_gmb(&mut tx, Some(b.id), b.gmail_id, &place, b.created_at).await?;
			eyre::ensure!(added.is_some_and(|a| a.id == b.id), "GMB {} (gmail {}, {}) clashes with another", b.id, b.gmail_id, b.place_id);
		}
		for e in &export.ledger {
			let entry = Entry {
				at: e.at,
				delta: e.delta,
				kind: e.kind.parse()?,
				charged: charged(e.run_id, e.label.clone())?,
				by: e.by_email.clone(),
				note: e.note.clone(),
			};
			db::append(&mut tx, sub(e.sub), &entry).await.wrap_err_with(|| format!("{}'s ledger row at {}", e.email, e.at))?;
		}
		for k in &export.kickbacks {
			let why = match (charged(k.run_id, k.label.clone())?, &k.by_email, &k.note) {
				(Some(c), None, None) => Owed::Charge(c),
				(None, Some(by), Some(note)) => Owed::Grant { by: by.clone(), note: note.clone() },
				other => eyre::bail!("{}'s kick-back at {}: a run's, or a grant with its admin and note; got {other:?}", k.email, k.at),
			};
			db::owe(&mut tx, sub(k.sub), k.at, k.amount, &why).await?;
		}
		db::open(&mut tx).await?;
		tx.commit().await.wrap_err("committing the import")?;
		tracing::info!(
			people = export.people.len(),
			gmails = export.gmails.len(),
			gmbs = export.gmbs.len(),
			ledger = export.ledger.len(),
			kickbacks = export.kickbacks.len(),
			"imported from review_archive; tokens and GMBs open"
		);
		self.snapshot_due.notify_one();
		Ok(())
	}
}

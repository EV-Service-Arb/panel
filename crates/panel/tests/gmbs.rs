//! A GMB's switch: its owner's to turn, and what review_archive's snapshot carries.

use jiff::Timestamp;
use panel::{
	Refused,
	import::Export,
	testing::{TestDb, panel},
	tokens::User,
};
use uuid::Uuid;

fn now() -> Timestamp {
	"2026-11-01T12:00:00Z".parse().unwrap()
}

async fn person(p: &panel::Panel, email: &str) -> Uuid {
	let user_id = Uuid::now_v7();
	p.user_seen(
		&User {
			user_id,
			email: email.to_owned(),
			name: email.to_owned(),
		},
		now(),
	)
	.await
	.unwrap();
	user_id
}

async fn snapshot_switch(p: &panel::Panel, gmb: i64) -> bool {
	p.snapshot(now()).await.unwrap().unwrap().gmbs.iter().find(|m| m.id == gmb).unwrap().enabled
}

#[tokio::test]
async fn a_gmb_is_switched_by_its_owner_and_review_archive_is_told() {
	let db = TestDb::create().await;
	let p = panel(&db).await;
	p.import_review_archive(Export {
		people: vec![],
		gmails: vec![],
		gmbs: vec![],
		ledger: vec![],
		kickbacks: vec![],
	})
	.await
	.unwrap();
	let owner = person(&p, "owner@localhost").await;
	let other = person(&p, "other@localhost").await;
	let gmail = p.add_gmail(owner, "a@gmail.com", now()).await.unwrap().unwrap();
	let one = p.add_gmb(Some(owner), gmail.id, "ChIJLU7jZClu5kcR4PcOOO6p3I0", now()).await.unwrap().unwrap();
	let two = p.add_gmb(Some(owner), gmail.id, "ChIJD7fiBh9u5kcRYJSMaMOCCwQ", now()).await.unwrap().unwrap();
	assert!(snapshot_switch(&p, one.id).await, "a GMB starts on");

	p.switch_gmbs(owner, &[one.id, two.id], false).await.unwrap().unwrap();
	assert!(!snapshot_switch(&p, one.id).await);
	assert!(!snapshot_switch(&p, two.id).await);
	let listed = p.gmails(owner).await.unwrap();
	assert!(listed[0].1.iter().all(|b| !b.enabled), "the owner sees them off");

	p.switch_gmbs(owner, &[two.id], true).await.unwrap().unwrap();
	assert!(!snapshot_switch(&p, one.id).await);
	assert!(snapshot_switch(&p, two.id).await);
	p.switch_gmbs(owner, &[two.id], true).await.unwrap().unwrap();
	assert!(snapshot_switch(&p, two.id).await, "switching on what is on changes nothing");

	assert!(matches!(p.switch_gmbs(other, &[two.id], false).await.unwrap(), Err(Refused::NotFound)), "not theirs");
	assert!(matches!(p.switch_gmbs(owner, &[one.id, 9999], true).await.unwrap(), Err(Refused::NotFound)), "all or nothing");
	assert!(!snapshot_switch(&p, one.id).await, "nothing of a refused switch is taken");

	p.remove_gmb(owner, one.id, now()).await.unwrap().unwrap();
	assert!(
		matches!(p.switch_gmbs(owner, &[one.id], true).await.unwrap(), Err(Refused::NotFound)),
		"a removed GMB is no one's to switch"
	);
}

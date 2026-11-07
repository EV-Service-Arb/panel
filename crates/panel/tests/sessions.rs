//! Sign-in sessions against a real Postgres: sealed tokens, rotation under a lock, and what
//! a refusal or an outage of concierge does to a session.

use std::sync::{
	Mutex,
	atomic::{AtomicUsize, Ordering},
};

use jiff::{SignedDuration, Timestamp};
use sa_panel::{
	session::{PRELOGIN_TTL, RefreshError, Refresher, SessionError, SessionKey, Tokens, pkce_challenge},
	testing::{TestDb, panel},
};
use uuid::Uuid;
use zeroize::Zeroizing;

fn now() -> Timestamp {
	"2026-09-30T18:00:00Z".parse().unwrap()
}

fn tokens(n: u32, access_for: SignedDuration) -> Tokens {
	Tokens {
		access: Zeroizing::new(format!("access-{n}")),
		access_expires_at: now() + access_for,
		refresh: Zeroizing::new(format!("refresh-{n}")),
		refresh_expires_at: now() + SignedDuration::from_hours(24 * 30),
	}
}

/// Answers with the next pair, or as told; counts calls and remembers what it was shown.
struct Fake {
	answer: Mutex<Option<RefreshError>>,
	calls: AtomicUsize,
	seen: Mutex<Vec<String>>,
}

impl Fake {
	fn new(answer: Option<RefreshError>) -> Self {
		Self {
			answer: Mutex::new(answer),
			calls: AtomicUsize::new(0),
			seen: Mutex::new(Vec::new()),
		}
	}
}

impl Refresher for Fake {
	async fn refresh(&self, refresh_token: &str) -> Result<Tokens, RefreshError> {
		let n = self.calls.fetch_add(1, Ordering::SeqCst) + 2;
		self.seen.lock().unwrap().push(refresh_token.to_owned());
		match self.answer.lock().unwrap().take() {
			Some(e) => Err(e),
			None => Ok(tokens(n as u32, SignedDuration::from_hours(1))),
		}
	}
}

#[tokio::test]
async fn a_session_lives_rotates_and_ends() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let user = Uuid::now_v7();
	let fake = Fake::new(None);

	let opened = panel.open_session(user, &tokens(1, SignedDuration::from_mins(15)), now()).await.unwrap();
	let s = panel.session(&opened.cookie, now(), &fake).await.unwrap();
	assert_eq!((s.user_id, s.access.as_str()), (user, "access-1"));
	assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "fresh: no refresh");

	let pool = db.pool().await;
	let (access, refresh): (Vec<u8>, Vec<u8>) = sqlx::query_as("SELECT access_sealed, refresh_sealed FROM sessions").fetch_one(&pool).await.unwrap();
	assert!(!access.windows(8).any(|w| w == b"access-1"), "sealed");
	assert!(!refresh.windows(9).any(|w| w == b"refresh-1"), "sealed");
	let stored: Vec<u8> = sqlx::query_scalar("SELECT id_hash FROM sessions").fetch_one(&pool).await.unwrap();
	assert_ne!(stored, opened.cookie.as_bytes(), "the cookie itself is not stored");

	// Near expiry, two requests at once: one rotation, both get the new token.
	let later = now() + SignedDuration::from_mins(15) - SignedDuration::from_secs(10);
	let (a, b) = tokio::join!(panel.session(&opened.cookie, later, &fake), panel.session(&opened.cookie, later, &fake));
	assert_eq!((a.unwrap().access.as_str(), b.unwrap().access.as_str()), ("access-2", "access-2"));
	assert_eq!(fake.calls.load(Ordering::SeqCst), 1, "the refresh token is presented once");
	assert_eq!(fake.seen.lock().unwrap().as_slice(), ["refresh-1"]);

	assert!(matches!(panel.session("not-a-session", now(), &fake).await, Err(SessionError::Missing)));
	assert!(matches!(panel.session(&"0".repeat(64), now(), &fake).await, Err(SessionError::Missing)));

	let key = SessionKey::of_cookie(&opened.cookie).unwrap();
	assert!(panel.close_session(&key).await.unwrap());
	assert!(matches!(panel.session(&opened.cookie, now(), &fake).await, Err(SessionError::Missing)));
	assert!(!panel.close_session(&key).await.unwrap());
}

#[tokio::test]
async fn concierge_refusing_closes_and_an_outage_does_not() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let stale = now() + SignedDuration::from_mins(20);

	let down = Fake::new(Some(RefreshError::Unavailable("connection refused".into())));
	let opened = panel.open_session(Uuid::now_v7(), &tokens(1, SignedDuration::from_mins(15)), now()).await.unwrap();
	assert!(matches!(panel.session(&opened.cookie, stale, &down).await, Err(SessionError::Unavailable)));
	let back = Fake::new(None);
	assert_eq!(
		panel.session(&opened.cookie, stale, &back).await.unwrap().access.as_str(),
		"access-2",
		"still there once concierge is back"
	);

	let refused = Fake::new(Some(RefreshError::Rejected));
	let opened = panel.open_session(Uuid::now_v7(), &tokens(1, SignedDuration::from_mins(15)), now()).await.unwrap();
	assert!(matches!(panel.session(&opened.cookie, stale, &refused).await, Err(SessionError::Rejected)));
	assert!(matches!(panel.session(&opened.cookie, now(), &back).await, Err(SessionError::Missing)), "closed");

	let past_deadline = now() + SignedDuration::from_hours(24 * 31);
	let opened = panel.open_session(Uuid::now_v7(), &tokens(1, SignedDuration::from_mins(15)), now()).await.unwrap();
	assert!(matches!(panel.session(&opened.cookie, past_deadline, &back).await, Err(SessionError::Missing)));
}

#[tokio::test]
async fn pre_logins() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let begun = panel.begin_sign_in(Some("/review_archive"), now()).unwrap();
	assert_eq!(begun.state.len(), 64, "256 bits");
	assert_eq!(begun.challenge.len(), 43);

	let pre = panel.finish_sign_in(&begun.cookie, &begun.state, now() + SignedDuration::from_mins(9)).unwrap();
	assert_eq!(pkce_challenge(&pre.verifier), begun.challenge);
	assert_eq!(pre.return_to.as_deref(), Some("/review_archive"));
	assert!(panel.finish_sign_in(&begun.cookie, &"0".repeat(64), now()).is_none(), "another state");
	assert!(panel.finish_sign_in(&begun.cookie, "", now()).is_none());
	assert!(
		panel.finish_sign_in(&begun.cookie, &begun.state, now() + PRELOGIN_TTL + SignedDuration::from_secs(1)).is_none(),
		"too old"
	);
	assert!(panel.finish_sign_in("garbage", &begun.state, now()).is_none());
	let other = sa_panel::testing::panel(&db).await.begin_sign_in(None, now()).unwrap();
	assert!(panel.finish_sign_in(&other.cookie, &other.state, now()).is_none(), "sealed under another key");
	// RFC 7636, appendix B.
	assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

/// Slow to answer, as concierge over the network is; counts what it was shown.
struct Slow {
	calls: AtomicUsize,
	seen: Mutex<Vec<String>>,
}

impl Refresher for Slow {
	async fn refresh(&self, refresh_token: &str) -> Result<Tokens, RefreshError> {
		self.seen.lock().unwrap().push(refresh_token.to_owned());
		let n = self.calls.fetch_add(1, Ordering::SeqCst) + 2;
		tokio::time::sleep(std::time::Duration::from_millis(300)).await;
		Ok(tokens(n as u32, SignedDuration::from_hours(1)))
	}
}

#[tokio::test]
async fn two_replicas_present_a_refresh_token_once() {
	let db = TestDb::create().await;
	let hex = sa_panel::seal::DataKey::generate_hex().unwrap();
	let replica = |store| sa_panel::testing::panel_with(store, sa_panel::seal::DataKey::from_hex(&hex).unwrap());
	let (a, b) = (replica(db.store().await), replica(db.store().await));
	let slow = Slow {
		calls: AtomicUsize::new(0),
		seen: Mutex::new(Vec::new()),
	};
	let opened = a.open_session(Uuid::now_v7(), &tokens(1, SignedDuration::from_mins(15)), now()).await.unwrap();
	let later = now() + SignedDuration::from_mins(15) - SignedDuration::from_secs(10);
	let (x, y) = tokio::join!(a.session(&opened.cookie, later, &slow), b.session(&opened.cookie, later, &slow));
	assert_eq!((x.unwrap().access.as_str(), y.unwrap().access.as_str()), ("access-2", "access-2"));
	assert_eq!(slow.seen.lock().unwrap().as_slice(), ["refresh-1"], "one replica asked, the other waited for its answer");

	let lease: Option<i64> = sqlx::query_scalar("SELECT rotating_until FROM sessions").fetch_one(&db.pool().await).await.unwrap();
	assert!(lease.is_none(), "the lease is dropped with the rotation");
}

#[tokio::test]
async fn a_lease_of_a_dead_replica_lapses() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let opened = panel.open_session(Uuid::now_v7(), &tokens(1, SignedDuration::from_mins(15)), now()).await.unwrap();
	let later = now() + SignedDuration::from_mins(15) - SignedDuration::from_secs(10);
	// A replica took the lease and died.
	sqlx::query("UPDATE sessions SET rotating_until = $1")
		.bind((later + sa_panel::session::ROTATION_LEASE).as_microsecond())
		.execute(&db.pool().await)
		.await
		.unwrap();
	let fake = Fake::new(None);
	let before = std::time::Instant::now();
	assert!(
		matches!(panel.session(&opened.cookie, later, &fake).await, Err(SessionError::Unavailable)),
		"held: wait, then 503"
	);
	assert!(before.elapsed() >= sa_panel::session::ROTATION_WAIT.unsigned_abs());
	assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
	let lapsed = later + sa_panel::session::ROTATION_LEASE;
	assert_eq!(
		panel.session(&opened.cookie, lapsed, &fake).await.unwrap().access.as_str(),
		"access-2",
		"taken over once it lapsed"
	);
}

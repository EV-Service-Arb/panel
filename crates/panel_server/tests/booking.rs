//! Booking end to end on the real router and a throwaway SQLite file: an operator's slot and
//! its transitions, the Google Calendar pull against a fake Google served in-process (axum on
//! a local port — never the network), the matching of its bookings to leads, the bookings
//! without a lead and their attachment, the push route with a fake adapter, and what the
//! live bus is told.

use std::{
	collections::{HashMap, VecDeque},
	sync::{Arc, Mutex},
};

use axum::{
	Json, Router,
	body::{Body, to_bytes},
	extract::{Query, State},
	http::{HeaderMap, Method, Request, StatusCode, header},
	routing::{get, post},
};
use jiff::{SignedDuration, Timestamp};
use panel::{
	Panel,
	booking::{BookingEvent, Change, Contact, Provider, PushError, PushRequest, PushSource, PushSources},
	live::{Signal, Topic},
	testing::{TestDb, event, panel, sign},
};
use panel_core::ids::BrandId;
use panel_server::{
	concierge::{Concierge, DevIdentity},
	google_calendar::{BrandCalendar, GoogleCalendar},
	http,
	signin::{SignIn, SignInConfig},
};
use sa_auth::SourceKind;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;
use uuid::Uuid;
use zeroize::Zeroizing;

const ORIGIN: &str = "http://127.0.0.1:59120";

fn app(panel: Panel, alias: &str) -> Router {
	let who = DevIdentity {
		permissions: sa_auth::Catalog::collect("sa", 0).aliases[alias].iter().cloned().collect(),
		email: format!("dev-{}@localhost", alias.trim_start_matches("sa:")),
	};
	let config = SignInConfig {
		panel_origin: ORIGIN.to_owned(),
		concierge_origin: ORIGIN.to_owned(),
	};
	http::app(SignIn::new(panel, Concierge::dev(who), config))
}

#[derive(Default)]
struct Browser {
	jar: HashMap<String, String>,
}

impl Browser {
	async fn send(&mut self, app: &Router, method: Method, uri: &str, body: Option<Value>, headers: &[(&str, &str)]) -> (StatusCode, Value) {
		let mut req = Request::builder().method(method.clone()).uri(uri);
		if !self.jar.is_empty() {
			req = req.header(header::COOKIE, self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "));
		}
		if method != Method::GET
			&& let Some(t) = self.jar.get("sa_csrf")
		{
			req = req.header("x-sa-csrf", t);
		}
		for (k, v) in headers {
			req = req.header(*k, *v);
		}
		let req = match body {
			Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
			None => req.body(Body::empty()),
		};
		let res = app.clone().oneshot(req.unwrap()).await.unwrap();
		for c in res.headers().get_all(header::SET_COOKIE) {
			let (pair, _) = c.to_str().unwrap().split_once(';').unwrap_or((c.to_str().unwrap(), ""));
			let (k, v) = pair.split_once('=').unwrap();
			self.jar.insert(k.to_owned(), v.to_owned());
		}
		let status = res.status();
		let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
		(status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
	}

	async fn get(&mut self, app: &Router, uri: &str) -> (StatusCode, Value) {
		self.send(app, Method::GET, uri, None, &[]).await
	}

	async fn post(&mut self, app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
		self.send(app, Method::POST, uri, Some(body), &[]).await
	}

	async fn signed_in(app: &Router) -> Self {
		let mut b = Self::default();
		let login = app.clone().oneshot(Request::get("/auth/login").body(Body::empty()).unwrap()).await.unwrap();
		let location = login.headers()[header::LOCATION].to_str().unwrap().to_owned();
		for c in login.headers().get_all(header::SET_COOKIE) {
			let (pair, _) = c.to_str().unwrap().split_once(';').unwrap();
			let (k, v) = pair.split_once('=').unwrap();
			b.jar.insert(k.to_owned(), v.to_owned());
		}
		let (status, _) = b.get(app, location.strip_prefix(ORIGIN).unwrap()).await;
		assert_eq!(status, StatusCode::SEE_OTHER);
		b
	}
}

fn vifnet() -> BrandId {
	BrandId::parse("vifnet").unwrap()
}

/// A site key for vifnet, and leads sent through it with their PII.
async fn site(panel: &Panel) -> String {
	panel.add_source("vifnet-site", SourceKind::Site, [vifnet()].into()).await.unwrap().unwrap().secret.to_string()
}

async fn site_lead(panel: &Panel, secret: &str, id: &str, at: Timestamp, pii: Value) {
	let mut e = event(
		"lead.created",
		at,
		"site",
		json!({"brandId": "vifnet", "locationId": "vifnet", "leadId": id}),
		json!({"channel": "form"}),
	);
	e["pii"] = pii;
	let now = Timestamp::now();
	let v = panel.ingest(sign("vifnet-site", secret, &[e], now).batch(), now).await.unwrap();
	assert_eq!(v[0].outcome, panel::Outcome::Accepted { unregistered: false });
}

/// What the bus says, drained: `(topic, id)`.
fn told(rx: &mut broadcast::Receiver<Signal>) -> Vec<(Topic, Option<String>)> {
	let mut out = Vec::new();
	while let Ok(s) = rx.try_recv() {
		if let Signal::Changed(c) = s {
			out.push((c.topic, c.id));
		}
	}
	out
}

#[tokio::test]
async fn an_operator_books_closes_and_clears_a_slot() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let app = app(panel.clone(), "sa:operator");
	let mut b = Browser::signed_in(&app).await;
	let (status, made) = b
		.post(&app, "/api/v1/leads", json!({"brand": "vifnet", "location": "vifnet", "need": "ménage", "phone": "0612345678"}))
		.await;
	assert_eq!(status, StatusCode::CREATED, "{made}");
	let lead = format!("/api/v1/leads/vifnet/{}", made["lead_id"].as_str().unwrap());
	let booking = |b: Value| b["lead"]["booking"].clone();

	assert_eq!(
		booking(b.get(&app, &lead).await.1),
		json!({"status": "none", "provider": null, "start_at": null, "end_at": null, "external_ref": null, "match": null, "preferred_date": null, "preferred_part": null})
	);
	assert_eq!(
		b.post(&app, &format!("{lead}/booking/status"), json!({"status": "done"})).await.0,
		StatusCode::CONFLICT,
		"nothing booked"
	);
	assert_eq!(
		b.post(&app, &format!("{lead}/booking"), json!({"action": "clear"})).await.0,
		StatusCode::CONFLICT,
		"nothing to clear"
	);
	let (status, e) = b
		.post(
			&app,
			&format!("{lead}/booking"),
			json!({"action": "set", "start_at": "2026-10-08T10:00:00+02:00", "end_at": "2026-10-08T09:00:00+02:00"}),
		)
		.await;
	assert_eq!(status, StatusCode::BAD_REQUEST, "an end before its start: {e}");

	let mut rx = panel.bus().subscribe();
	let set = json!({"action": "set", "start_at": "2026-10-08T10:00:00+02:00", "end_at": "2026-10-08T11:00:00+02:00"});
	let (status, first) = b.send(&app, Method::POST, &format!("{lead}/booking"), Some(set.clone()), &[("idempotency-key", "k1")]).await;
	assert_eq!(status, StatusCode::CREATED);
	let (status, again) = b.send(&app, Method::POST, &format!("{lead}/booking"), Some(set), &[("idempotency-key", "k1")]).await;
	assert_eq!((status, &again), (StatusCode::OK, &first), "a retry is the first answer");
	assert_eq!(told(&mut rx), [(Topic::Lead, made["lead_id"].as_str().map(str::to_owned))]);
	assert_eq!(
		booking(b.get(&app, &lead).await.1),
		json!({"status": "booked", "provider": "manual", "start_at": "2026-10-08T08:00:00Z", "end_at": "2026-10-08T09:00:00Z",
			"external_ref": null, "match": "manual", "preferred_date": null, "preferred_part": null})
	);
	assert_eq!(b.post(&app, &format!("{lead}/booking/status"), json!({"status": "lost"})).await.0, StatusCode::BAD_REQUEST);
	assert_eq!(b.post(&app, &format!("{lead}/booking/status"), json!({"status": "no_show"})).await.0, StatusCode::CREATED);
	assert_eq!(booking(b.get(&app, &lead).await.1)["status"], "no_show");
	assert_eq!(
		b.post(&app, &format!("{lead}/booking/status"), json!({"status": "done"})).await.0,
		StatusCode::CONFLICT,
		"closed already"
	);
	assert_eq!(b.post(&app, &format!("{lead}/booking"), json!({"action": "clear"})).await.0, StatusCode::CREATED);
	assert_eq!(booking(b.get(&app, &lead).await.1)["status"], "none");
	b.post(&app, &format!("{lead}/booking"), json!({"action": "set", "start_at": "2026-10-09T10:00:00+02:00"})).await;
	assert_eq!(b.post(&app, &format!("{lead}/booking/status"), json!({"status": "done"})).await.0, StatusCode::CREATED);
	assert_eq!(
		b.post(&app, &format!("{lead}/booking"), json!({"action": "set", "start_at": "2026-10-10T10:00:00+02:00"}))
			.await
			.0,
		StatusCode::CONFLICT,
		"a done booking is not booked again"
	);
	assert_eq!(b.post(&app, &format!("{lead}/booking"), json!({"action": "clear"})).await.0, StatusCode::CONFLICT);
	let (_, listed) = b.get(&app, "/api/v1/leads?booking=done").await;
	assert_eq!(listed["leads"].as_array().unwrap().len(), 1);
	assert_eq!(b.get(&app, "/api/v1/leads?booking=booked").await.1["leads"], json!([]));
	assert_eq!(b.get(&app, "/api/v1/leads?booking=maybe").await.0, StatusCode::BAD_REQUEST);
	assert_eq!(b.post(&app, "/api/v1/leads/vifnet/p-nope/booking", json!({"action": "clear"})).await.0, StatusCode::NOT_FOUND);

	// The rebuild lands on the same booking.
	let before = b.get(&app, &lead).await.1["lead"]["booking"].clone();
	panel.rebuild_projections().await.unwrap();
	assert_eq!(b.get(&app, &lead).await.1["lead"]["booking"], before);
}

// ── a fake Google ───────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Fake {
	/// What each events.list answers, in order.
	answers: VecDeque<(StatusCode, Value)>,
	/// Each events.list's query.
	asked: Vec<HashMap<String, String>>,
	tokens: u32,
}

#[derive(Clone, Default)]
struct FakeGoogle(Arc<Mutex<Fake>>);

impl FakeGoogle {
	fn answer(&self, status: StatusCode, body: Value) {
		self.0.lock().unwrap().answers.push_back((status, body));
	}

	fn asked(&self) -> Vec<HashMap<String, String>> {
		std::mem::take(&mut self.0.lock().unwrap().asked)
	}
}

async fn token(State(g): State<FakeGoogle>, body: String) -> (StatusCode, Json<Value>) {
	let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
	if form.get("refresh_token").map(String::as_str) != Some("1//vifnet-refresh") || form.get("client_secret").map(String::as_str) != Some("csecret") {
		return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant"})));
	}
	g.0.lock().unwrap().tokens += 1;
	(StatusCode::OK, Json(json!({"access_token": "ya29.fake", "expires_in": 3599, "token_type": "Bearer"})))
}

async fn events(State(g): State<FakeGoogle>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
	if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer ya29.fake") {
		return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": 401}})));
	}
	let mut f = g.0.lock().unwrap();
	f.asked.push(q);
	let (status, body) = f.answers.pop_front().unwrap_or((StatusCode::OK, json!({"items": [], "nextSyncToken": "idle"})));
	(status, Json(body))
}

async fn fake_google() -> (FakeGoogle, GoogleCalendar) {
	let g = FakeGoogle::default();
	let app = Router::new()
		.route("/token", post(token))
		.route("/calendars/{calendar}/events", get(events))
		.with_state(g.clone());
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let base = format!("http://{}", listener.local_addr().unwrap());
	// Dropped with the test's runtime.
	tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
	let calendars = [(
		vifnet(),
		BrandCalendar {
			refresh_token: Zeroizing::new("1//vifnet-refresh".into()),
			calendar_id: "primary".into(),
		},
	)]
	.into();
	let api = GoogleCalendar::new(&format!("{base}/token"), &base, "cid", "csecret", calendars).unwrap();
	(g, api)
}

/// An appointment booked `booked_mins_ago`, at `start`, by `email` with `phone` (the form's
/// answer), as the adapter assumes Google writes one.
fn appointment(id: &str, etag: &str, start: &str, email: &str, phone: Option<&str>) -> Value {
	let now = Timestamp::now();
	let description = match phone {
		Some(p) => format!("Réservé par<br>Client<br>{email}<br><br>Téléphone<br>{p}"),
		None => format!("Réservé par<br>Client<br>{email}"),
	};
	json!({
		"id": id, "etag": etag, "status": "confirmed",
		"created": (now - SignedDuration::from_mins(30)).to_string(),
		"updated": (now - SignedDuration::from_mins(1)).to_string(),
		"description": description,
		"start": {"dateTime": start}, "end": {"dateTime": (start.parse::<Timestamp>().unwrap() + SignedDuration::from_mins(30)).to_string()},
		"attendees": [{"email": "owner@vifnet.fr", "self": true, "organizer": true}, {"email": email, "displayName": "Client"}]
	})
}

async fn lead_booking(b: &mut Browser, app: &Router, lead: &str) -> Value {
	let (status, body) = b.get(app, &format!("/api/v1/leads/vifnet/{lead}")).await;
	assert_eq!(status, StatusCode::OK, "{lead}: {body}");
	body["lead"]["booking"].clone()
}

async fn sync(panel: &Panel, api: &GoogleCalendar, full: bool) -> panel::booking::Synced {
	panel
		.sync_bookings(api, &vifnet(), Uuid::now_v7(), Timestamp::now(), SignedDuration::from_mins(5), true, full)
		.await
		.unwrap()
		.unwrap()
}

#[tokio::test]
async fn google_bookings_are_pulled_matched_moved_and_canceled() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = site(&panel).await;
	let app = app(panel.clone(), "sa:operator");
	let mut b = Browser::signed_in(&app).await;
	let (g, api) = fake_google().await;
	let hour_ago = Timestamp::now() - SignedDuration::from_hours(1);
	site_lead(&panel, &secret, "lead-1-0000000a", hour_ago, json!({"phone": "06 12 34 56 78", "need": "x"})).await;
	site_lead(&panel, &secret, "lead-2-0000000b", hour_ago, json!({"phone": "+33 7 00 00 00 01", "email": "Bea@Example.fr"})).await;
	// Two leads with one number: a booking from it is not for either of them by itself.
	site_lead(&panel, &secret, "lead-3-0000000c", hour_ago, json!({"phone": "0699999901"})).await;
	site_lead(&panel, &secret, "lead-4-0000000d", hour_ago, json!({"phone": "+33699999901"})).await;
	// Too old to be the one: outside the 14 days.
	site_lead(&panel, &secret, "lead-5-0000000e", hour_ago - SignedDuration::from_hours(24 * 15), json!({"phone": "0655555501"})).await;

	let mut rx = panel.bus().subscribe();
	// Kept: Google answers one revision of an event the same way every time.
	let original = appointment("evphone1", "\"e1\"", "2026-10-08T10:00:00+02:00", "anyone@example.fr", Some("+33 6 12 34 56 78"));
	g.answer(
		StatusCode::OK,
		json!({"items": [
			original.clone(),
			appointment("evmail22", "\"e1\"", "2026-10-08T11:00:00+02:00", "bea@example.fr", None),
		], "nextPageToken": "p2"}),
	);
	g.answer(
		StatusCode::OK,
		json!({"items": [
			appointment("evtwin33", "\"e1\"", "2026-10-08T12:00:00+02:00", "twin@example.fr", Some("06 99 99 99 01")),
			appointment("evnone44", "\"e1\"", "2026-10-08T13:00:00+02:00", "old@example.fr", Some("06 55 55 55 01")),
			{"id": "meeting5", "etag": "\"e1\"", "status": "confirmed", "description": "Accountant", "start": {"dateTime": "2026-10-08T14:00:00+02:00"},
			 "attendees": [{"email": "acc@example.fr"}]},
			{"id": "deleted6", "etag": "\"e2\"", "status": "cancelled"},
		], "nextSyncToken": "t1"}),
	);
	let first = sync(&panel, &api, false).await;
	let asked = g.asked();
	assert_eq!(asked.len(), 2, "both pages");
	assert!(
		asked[0].contains_key("timeMin") && !asked[0].contains_key("syncToken"),
		"the first pull is a full one: {:?}",
		asked[0]
	);
	assert_eq!((asked[0]["singleEvents"].as_str(), asked[0]["showDeleted"].as_str()), ("true", "true"));
	assert_eq!(asked[1]["pageToken"], "p2");
	assert!(first.full);
	let i = first.ingested;
	assert_eq!(
		(i.written, i.matched, i.unmatched, i.ignored),
		(4, 2, 2, 1),
		"{i:?}: the meeting is no booking, the unknown cancellation ignored"
	);

	let phone = lead_booking(&mut b, &app, "lead-1-0000000a").await;
	assert_eq!(
		(phone["status"].as_str(), phone["match"].as_str(), phone["provider"].as_str()),
		(Some("booked"), Some("contact"), Some("google_calendar"))
	);
	assert_eq!((phone["external_ref"].as_str(), phone["start_at"].as_str()), (Some("evphone1"), Some("2026-10-08T08:00:00Z")));
	assert_eq!(lead_booking(&mut b, &app, "lead-2-0000000b").await["external_ref"], "evmail22", "by email, whatever its case");
	for twin in ["lead-3-0000000c", "lead-4-0000000d", "lead-5-0000000e"] {
		assert_eq!(lead_booking(&mut b, &app, twin).await["status"], "none", "{twin}");
	}
	let (status, unmatched) = b.get(&app, "/api/v1/bookings/unmatched?brand=vifnet").await;
	assert_eq!(status, StatusCode::OK);
	let refs: Vec<&str> = unmatched["bookings"].as_array().unwrap().iter().map(|x| x["external_ref"].as_str().unwrap()).collect();
	assert_eq!(refs, ["evtwin33", "evnone44"], "the next slot first");
	let twin = &unmatched["bookings"][0];
	assert_eq!(
		twin["contact"],
		json!({"name": "Client", "email": "twin@example.fr", "phone": "+33699999901"}),
		"the attendee, for whoever sees PII"
	);
	let topics: Vec<Topic> = told(&mut rx).into_iter().map(|(t, _)| t).collect();
	assert_eq!(topics.iter().filter(|t| **t == Topic::Lead).count(), 2, "{topics:?}");
	assert_eq!(topics.iter().filter(|t| **t == Topic::Bookings).count(), 2, "{topics:?}");

	// Moved: the same event, another etag, another start; the lead's slot follows.
	let moved = appointment("evphone1", "\"e2\"", "2026-10-09T15:00:00+02:00", "anyone@example.fr", Some("+33 6 12 34 56 78"));
	g.answer(StatusCode::OK, json!({"items": [moved.clone()], "nextSyncToken": "t2"}));
	let second = sync(&panel, &api, false).await;
	assert_eq!(g.asked()[0]["syncToken"], "t1", "from the cursor");
	assert_eq!((second.full, second.ingested.written, second.ingested.matched), (false, 1, 1));
	let after = lead_booking(&mut b, &app, "lead-1-0000000a").await;
	assert_eq!(
		(after["start_at"].as_str(), after["match"].as_str()),
		(Some("2026-10-09T13:00:00Z"), Some("contact")),
		"a move, not a second booking"
	);
	assert_eq!(told(&mut rx), [(Topic::Lead, Some("lead-1-0000000a".to_owned()))]);

	// The same pull again changes nothing; an older revision seen again is the journal's already.
	g.answer(StatusCode::OK, json!({"items": [moved], "nextSyncToken": "t3"}));
	assert_eq!(sync(&panel, &api, false).await.ingested.unchanged, 1);
	g.answer(StatusCode::OK, json!({"items": [original], "nextSyncToken": "t4"}));
	let again = sync(&panel, &api, false).await.ingested;
	assert_eq!((again.written, again.duplicate), (0, 1), "{again:?}");
	assert_eq!(lead_booking(&mut b, &app, "lead-1-0000000a").await["start_at"], "2026-10-09T13:00:00Z");

	// Canceled.
	g.answer(
		StatusCode::OK,
		json!({"items": [{"id": "evphone1", "etag": "\"e3\"", "status": "cancelled"}], "nextSyncToken": "t5"}),
	);
	sync(&panel, &api, false).await;
	assert_eq!(lead_booking(&mut b, &app, "lead-1-0000000a").await["status"], "canceled");

	// The sync token expired: everything again, from the window.
	g.asked();
	g.answer(StatusCode::GONE, json!({"error": {"code": 410, "message": "Sync token is no longer valid"}}));
	g.answer(StatusCode::OK, json!({"items": [], "nextSyncToken": "t-full"}));
	let resynced = sync(&panel, &api, false).await;
	let asked = g.asked();
	assert_eq!(
		(asked[0]["syncToken"].as_str(), asked[1].contains_key("timeMin"), asked[1].contains_key("syncToken")),
		("t5", true, false)
	);
	assert!(resynced.full);
	g.answer(StatusCode::OK, json!({"items": [], "nextSyncToken": "t6"}));
	sync(&panel, &api, false).await;
	assert_eq!(g.asked()[0]["syncToken"], "t-full", "the full pull's cursor kept");
	assert_eq!(g.0.lock().unwrap().tokens, 1, "the access token is reused while it lasts");

	// A failure keeps the cursor, and the lease lets go.
	g.answer(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "backend"}));
	assert!(
		panel
			.sync_bookings(&api, &vifnet(), Uuid::now_v7(), Timestamp::now(), SignedDuration::from_mins(5), true, false)
			.await
			.is_err()
	);
	g.answer(StatusCode::OK, json!({"items": [], "nextSyncToken": "t7"}));
	sync(&panel, &api, false).await;
	assert_eq!(g.asked()[1]["syncToken"], "t6");

	// Not due: another pull within the period is no pull.
	assert!(
		panel
			.sync_bookings(&api, &vifnet(), Uuid::now_v7(), Timestamp::now(), SignedDuration::from_mins(5), false, false)
			.await
			.unwrap()
			.is_none()
	);

	// An operator attaches one without a lead; it leaves the list, its lead is booked.
	let id = twin["id"].as_str().unwrap();
	assert_eq!(
		b.post(&app, &format!("/api/v1/bookings/{id}/attach"), json!({"lead": "lead-9-0000000f"})).await.0,
		StatusCode::NOT_FOUND
	);
	assert_eq!(
		b.post(&app, &format!("/api/v1/bookings/{}/attach", Uuid::now_v7()), json!({"lead": "lead-3-0000000c"})).await.0,
		StatusCode::NOT_FOUND
	);
	told(&mut rx);
	assert_eq!(
		b.post(&app, &format!("/api/v1/bookings/{id}/attach"), json!({"lead": "lead-3-0000000c"})).await.0,
		StatusCode::CREATED
	);
	assert_eq!(
		b.post(&app, &format!("/api/v1/bookings/{id}/attach"), json!({"lead": "lead-3-0000000c"})).await.0,
		StatusCode::CONFLICT
	);
	let attached = lead_booking(&mut b, &app, "lead-3-0000000c").await;
	assert_eq!(
		(attached["status"].as_str(), attached["match"].as_str(), attached["external_ref"].as_str()),
		(Some("booked"), Some("manual"), Some("evtwin33"))
	);
	let left: Vec<Value> = b.get(&app, "/api/v1/bookings/unmatched").await.1["bookings"].as_array().unwrap().clone();
	assert_eq!(left.iter().map(|x| x["external_ref"].as_str().unwrap()).collect::<Vec<_>>(), ["evnone44"]);
	let topics = told(&mut rx);
	assert!(
		topics.contains(&(Topic::Lead, Some("lead-3-0000000c".to_owned()))) && topics.iter().any(|(t, _)| *t == Topic::Bookings),
		"{topics:?}"
	);

	// Re-attached elsewhere: the first lead loses it.
	assert_eq!(
		b.post(&app, &format!("/api/v1/bookings/{id}/attach"), json!({"lead": "lead-4-0000000d"})).await.0,
		StatusCode::CREATED
	);
	assert_eq!(lead_booking(&mut b, &app, "lead-3-0000000c").await["status"], "none");
	assert_eq!(lead_booking(&mut b, &app, "lead-4-0000000d").await["external_ref"], "evtwin33");

	// And all of it again from the journal alone.
	let snapshot = [lead_booking(&mut b, &app, "lead-1-0000000a").await, lead_booking(&mut b, &app, "lead-4-0000000d").await];
	let unmatched_before = b.get(&app, "/api/v1/bookings/unmatched").await.1;
	panel.rebuild_projections().await.unwrap();
	assert_eq!(
		[lead_booking(&mut b, &app, "lead-1-0000000a").await, lead_booking(&mut b, &app, "lead-4-0000000d").await],
		snapshot
	);
	assert_eq!(b.get(&app, "/api/v1/bookings/unmatched").await.1, unmatched_before);
}

// ── push ────────────────────────────────────────────────────────────────────────────────

/// A push provider that signs with a header saying `ok`, and sends `{ref, uid, start}`.
struct FakeCalCom;

impl PushSource for FakeCalCom {
	fn provider(&self) -> Provider {
		Provider::CalCom
	}

	fn verify(&self, request: &PushRequest<'_>) -> Result<Vec<BookingEvent>, PushError> {
		if request.header("x-fake-signature") != Some("ok") {
			return Err(PushError::Unauthorized);
		}
		let body: Value = serde_json::from_slice(request.body).map_err(|e| PushError::BadRequest(e.to_string()))?;
		let start = body["start"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| PushError::BadRequest("no start".into()))?;
		Ok(vec![BookingEvent {
			provider: Provider::CalCom,
			external_ref: body["uid"].as_str().unwrap_or_default().to_owned(),
			version: "v1".into(),
			at: request.now,
			change: Change::Booked { start, end: None, booked_at: None },
			lead_ref: body["ref"].as_str().map(str::to_owned),
			contact: Contact::default(),
		}])
	}
}

async fn hook(app: &Router, uri: &str, signature: &str, body: Value) -> (StatusCode, Value) {
	let req = Request::post(uri)
		.header("x-fake-signature", signature)
		.header(header::CONTENT_TYPE, "application/json")
		.body(Body::from(body.to_string()))
		.unwrap();
	let res = app.clone().oneshot(req).await.unwrap();
	let status = res.status();
	(status, serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_push_route_answers_registered_providers_only() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = site(&panel).await;
	site_lead(
		&panel,
		&secret,
		"lead-7-0000000a",
		Timestamp::now() - SignedDuration::from_mins(5),
		json!({"phone": "0612345678"}),
	)
	.await;
	let body = json!({"ref": "lead-7-0000000a", "uid": "calbk-1", "start": "2026-10-08T08:00:00Z"});

	let served = http::router(panel.clone());
	for uri in [
		"/api/hooks/booking/cal_com/vifnet",
		"/api/hooks/booking/google_calendar/vifnet",
		"/api/hooks/booking/calendly/vifnet",
	] {
		assert_eq!(hook(&served, uri, "ok", body.clone()).await.0, StatusCode::NOT_FOUND, "{uri}: no provider is registered");
	}

	let fake = http::router_with_hooks(panel.clone(), http::Limits::default(), PushSources::default().with(FakeCalCom));
	assert_eq!(hook(&fake, "/api/hooks/booking/google_calendar/vifnet", "ok", body.clone()).await.0, StatusCode::NOT_FOUND);
	assert_eq!(hook(&fake, "/api/hooks/booking/cal_com/Vifnet", "ok", body.clone()).await.0, StatusCode::NOT_FOUND, "not a brand");
	assert_eq!(hook(&fake, "/api/hooks/booking/cal_com/vifnet", "forged", body.clone()).await.0, StatusCode::UNAUTHORIZED);
	assert_eq!(hook(&fake, "/api/hooks/booking/cal_com/vifnet", "ok", json!({"uid": "x"})).await.0, StatusCode::BAD_REQUEST);
	let (status, done) = hook(&fake, "/api/hooks/booking/cal_com/vifnet", "ok", body.clone()).await;
	assert_eq!((status, done["written"].as_u64()), (StatusCode::OK, Some(1)), "{done}");
	assert_eq!(hook(&fake, "/api/hooks/booking/cal_com/vifnet", "ok", body).await.1["unchanged"], 1, "a retried webhook");

	let operator = app(panel.clone(), "sa:operator");
	let mut b = Browser::signed_in(&operator).await;
	let booked = lead_booking(&mut b, &operator, "lead-7-0000000a").await;
	assert_eq!(
		(booked["match"].as_str(), booked["provider"].as_str(), booked["status"].as_str()),
		(Some("ref"), Some("cal_com"), Some("booked"))
	);
}

/// A site's `booking.requested` before its lead's `lead.created`: 409 with Retry-After, which
/// kitstart's outbox sends again; once the lead is there, the same batch is journaled.
#[tokio::test]
async fn a_request_before_its_lead_is_answered_409_to_retry() {
	let db = TestDb::create().await;
	let panel = panel(&db).await;
	let secret = site(&panel).await;
	let served = http::router(panel.clone());
	let now = Timestamp::now();
	let requested = event(
		"booking.requested",
		now,
		"site",
		json!({"brandId": "vifnet", "leadId": "lead-8-0000000a"}),
		json!({"lead_ref": "lead-8-0000000a", "provider": "google_calendar"}),
	);
	let post = |signed: panel::testing::Signed| {
		Request::post("/api/ingest/v1/events")
			.header("x-sa-key-id", signed.key_id)
			.header("x-sa-timestamp", signed.timestamp)
			.header("x-sa-signature", signed.signature)
			.body(Body::from(signed.body))
			.unwrap()
	};
	let res = served.clone().oneshot(post(sign("vifnet-site", &secret, std::slice::from_ref(&requested), now))).await.unwrap();
	assert_eq!(res.status(), StatusCode::CONFLICT);
	assert_eq!(res.headers()[header::RETRY_AFTER], "30");
	let body: Value = serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
	assert_eq!(body["results"][0]["status"], "deferred", "{body}");

	site_lead(&panel, &secret, "lead-8-0000000a", now - SignedDuration::from_mins(1), json!({"phone": "0612345678"})).await;
	let res = served.oneshot(post(sign("vifnet-site", &secret, &[requested], Timestamp::now()))).await.unwrap();
	assert_eq!(res.status(), StatusCode::MULTI_STATUS);
	let operator = app(panel, "sa:operator");
	let mut b = Browser::signed_in(&operator).await;
	let got = lead_booking(&mut b, &operator, "lead-8-0000000a").await;
	assert_eq!((got["status"].as_str(), got["provider"].as_str()), (Some("requested"), Some("google_calendar")));
}

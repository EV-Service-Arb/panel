-- The sites business's tables, empty: their rows live in sites_panel's copy of this database.
CREATE TABLE IF NOT EXISTS "events" (
	id BLOB PRIMARY KEY CHECK (length(id) = 16),
	schema TEXT NOT NULL,
	type TEXT NOT NULL,
	type_version INTEGER NOT NULL CHECK (type_version > 0),
	occurred_at INTEGER NOT NULL,
	received_at INTEGER NOT NULL,
	source_kind TEXT NOT NULL CHECK (source_kind IN ('site', 'review_archive', 'gbp', 'posthog', 'panel', 'sheet', 'telephony', 'booking', 'bot')),
	source_id TEXT NOT NULL,
	-- The key that signed it; NULL for events the panel writes itself.
	key_id TEXT REFERENCES sources (key_id),
	brand_id TEXT NOT NULL,
	location_id TEXT,
	lead_id TEXT,
	job_id TEXT,
	properties TEXT NOT NULL CHECK (json_valid(properties)),
	pii_sealed BLOB,
	data_key_fp BLOB CHECK (length(data_key_fp) = 32),
	-- HMAC-SHA256 of the event in canonical form, under a key derived from PANEL_DATA_KEY.
	content_mac BLOB NOT NULL CHECK (length(content_mac) = 32),
	status TEXT NOT NULL CHECK (status IN ('registered', 'unregistered', 'invalid')),
	status_reason TEXT,
	manual INTEGER GENERATED ALWAYS AS (source_kind = 'panel') STORED,
	CHECK ((pii_sealed IS NULL) = (data_key_fp IS NULL))
) STRICT;
CREATE INDEX events_by_lead ON events (brand_id, lead_id, occurred_at, id) WHERE lead_id IS NOT NULL;
CREATE INDEX events_in_order ON events (occurred_at, id);
CREATE INDEX events_not_registered ON events (type, type_version) WHERE status <> 'registered';
CREATE INDEX events_by_key ON events (key_id, received_at) WHERE key_id IS NOT NULL;
CREATE INDEX events_of_experiments ON events (brand_id, occurred_at, id)
WHERE type IN ('experiments.declared', 'experiment.configured');
CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN
	SELECT RAISE(ABORT, 'events is append-only: DELETE refused');
END;
CREATE TRIGGER events_only_status BEFORE UPDATE ON events
WHEN NEW.id IS NOT OLD.id OR NEW.schema IS NOT OLD.schema OR NEW.type IS NOT OLD.type OR NEW.type_version IS NOT OLD.type_version
	OR NEW.occurred_at IS NOT OLD.occurred_at OR NEW.received_at IS NOT OLD.received_at
	OR NEW.source_kind IS NOT OLD.source_kind OR NEW.source_id IS NOT OLD.source_id OR NEW.key_id IS NOT OLD.key_id
	OR NEW.brand_id IS NOT OLD.brand_id OR NEW.location_id IS NOT OLD.location_id OR NEW.lead_id IS NOT OLD.lead_id OR NEW.job_id IS NOT OLD.job_id
	OR NEW.properties IS NOT OLD.properties OR NEW.pii_sealed IS NOT OLD.pii_sealed OR NEW.data_key_fp IS NOT OLD.data_key_fp
	OR NEW.content_mac IS NOT OLD.content_mac
BEGIN
	SELECT RAISE(ABORT, 'events is append-only: UPDATE refused');
END;
CREATE TABLE IF NOT EXISTS "leads" (
	brand_id TEXT NOT NULL,
	lead_id TEXT NOT NULL,
	location_id TEXT,
	job_id TEXT,
	stage TEXT NOT NULL CHECK (stage IN ('created', 'contacted', 'quoted', 'won', 'completed', 'paid', 'lost')),
	channel TEXT CHECK (channel IN ('form', 'phone_inbound', 'callback', 'whatsapp', 'telegram')),
	manual INTEGER NOT NULL CHECK (manual IN (0, 1)),
	-- When the lead first reached each stage.
	created_at INTEGER,
	contacted_at INTEGER,
	quoted_at INTEGER,
	won_at INTEGER,
	completed_at INTEGER,
	paid_at INTEGER,
	lost_at INTEGER,
	lost_reason TEXT,
	last_event_id BLOB NOT NULL REFERENCES events (id),
	last_event_at INTEGER NOT NULL,
	suspect TEXT CHECK (suspect IN ('rate_limited', 'too_fast')),
	flow TEXT CHECK (flow IN ('quote', 'estimate', 'fixed')),
	-- Integer cents, EUR TTC.
	quoted_cents INTEGER CHECK (quoted_cents >= 0),
	pricing_valid_from TEXT CHECK (date(pricing_valid_from) IS pricing_valid_from),
	-- A JSON object, input id → value id.
	estimate_inputs TEXT CHECK (json_valid(estimate_inputs) AND json_type(estimate_inputs) = 'object'),
	booking_status TEXT CHECK (booking_status IN ('requested', 'booked', 'canceled', 'done', 'no_show')),
	booking_provider TEXT CHECK (booking_provider IN ('manual', 'link', 'google_calendar', 'cal_com')),
	booking_start_at INTEGER,
	booking_end_at INTEGER,
	booking_external_ref TEXT,
	booking_match TEXT CHECK (booking_match IN ('ref', 'contact', 'manual')),
	booking_preferred_date TEXT CHECK (date(booking_preferred_date) IS booking_preferred_date),
	booking_preferred_part TEXT CHECK (booking_preferred_part IN ('morning', 'afternoon', 'evening')),
	-- The ref the customer carries into a messenger (`AQ-7K3F`), from its lead.created. Not unique:
	-- a ref names the brand's newest lead carrying it.
	message_ref TEXT CHECK (message_ref GLOB '[A-Z][A-Z]*-[0-9A-Z][0-9A-Z][0-9A-Z][0-9A-Z]*'),
	-- When the customer first wrote on a messenger (lead.messaged), and on which.
	messaged_at INTEGER,
	messaged_channel TEXT CHECK (messaged_channel IN ('whatsapp', 'telegram')),
	CHECK ((messaged_at IS NULL) = (messaged_channel IS NULL)),
	PRIMARY KEY (brand_id, lead_id)
) STRICT;
CREATE INDEX leads_by_booking ON leads (booking_status) WHERE booking_status IS NOT NULL;
CREATE INDEX leads_by_message_ref ON leads (brand_id, message_ref) WHERE message_ref IS NOT NULL;
CREATE TABLE calls (
	event_id BLOB PRIMARY KEY REFERENCES events (id),
	brand_id TEXT NOT NULL,
	lead_id TEXT NOT NULL,
	location_id TEXT,
	kind TEXT NOT NULL CHECK (kind IN ('attempted', 'logged')),
	outcome TEXT CHECK (outcome IN ('answered', 'no_answer', 'wrong_number', 'later')),
	attempt_id TEXT,
	occurred_at INTEGER NOT NULL,
	manual INTEGER NOT NULL CHECK (manual IN (0, 1)),
	CHECK ((kind = 'logged') = (outcome IS NOT NULL))
) STRICT;
CREATE INDEX calls_by_lead ON calls (brand_id, lead_id, occurred_at);
CREATE TABLE payments (
	event_id BLOB PRIMARY KEY REFERENCES events (id),
	brand_id TEXT NOT NULL,
	lead_id TEXT NOT NULL,
	location_id TEXT,
	job_id TEXT,
	-- Minor units of `currency`.
	billed INTEGER NOT NULL CHECK (billed >= 0),
	commission INTEGER NOT NULL CHECK (commission BETWEEN 0 AND billed),
	currency TEXT NOT NULL CHECK (length(currency) = 3 AND currency NOT GLOB '*[^A-Z]*'),
	occurred_at INTEGER NOT NULL,
	manual INTEGER NOT NULL CHECK (manual IN (0, 1))
) STRICT;
CREATE INDEX payments_by_lead ON payments (brand_id, lead_id, occurred_at);
CREATE TABLE places (
	brand_id TEXT NOT NULL CHECK (length(brand_id) BETWEEN 1 AND 64 AND brand_id GLOB '[a-z0-9]*' AND brand_id NOT GLOB '*[^a-z0-9_-]*'),
	location_id TEXT NOT NULL CHECK (length(location_id) BETWEEN 1 AND 128),
	registered_at INTEGER NOT NULL,
	-- Who made it known: a user's email, or `cli`.
	registered_by TEXT NOT NULL,
	-- 1: the sites answer it as gone (a JSON 404), whatever its settings.
	withdrawn INTEGER NOT NULL DEFAULT 0 CHECK (withdrawn IN (0, 1)),
	PRIMARY KEY (brand_id, location_id)
) STRICT;
CREATE TRIGGER places_no_delete BEFORE DELETE ON places
BEGIN
	SELECT RAISE(ABORT, 'places are never deleted, only withdrawn: DELETE refused');
END;
CREATE TRIGGER places_only_withdrawn BEFORE UPDATE ON places
WHEN NEW.brand_id IS NOT OLD.brand_id OR NEW.location_id IS NOT OLD.location_id
	OR NEW.registered_at IS NOT OLD.registered_at OR NEW.registered_by IS NOT OLD.registered_by
BEGIN
	SELECT RAISE(ABORT, 'a place only ever gets withdrawn or restored: UPDATE refused');
END;
CREATE TABLE place_settings (
	brand_id TEXT NOT NULL,
	location_id TEXT NOT NULL,
	settings TEXT NOT NULL CHECK (json_valid(settings) AND json_type(settings) = 'object'),
	-- Strictly increasing per place: the token of optimistic concurrency.
	updated_at INTEGER NOT NULL,
	updated_by TEXT NOT NULL,
	-- The concierge user id; NULL for the CLI.
	updated_by_user BLOB CHECK (length(updated_by_user) = 16),
	PRIMARY KEY (brand_id, location_id),
	FOREIGN KEY (brand_id, location_id) REFERENCES places (brand_id, location_id)
) STRICT;
CREATE TRIGGER place_settings_no_delete BEFORE DELETE ON place_settings
BEGIN
	SELECT RAISE(ABORT, 'place settings are cleared to {}, not deleted: DELETE refused');
END;
CREATE TABLE place_changes (
	id BLOB PRIMARY KEY CHECK (length(id) = 16),
	brand_id TEXT NOT NULL,
	location_id TEXT NOT NULL,
	at INTEGER NOT NULL,
	kind TEXT NOT NULL CHECK (kind IN ('register', 'set', 'revert', 'withdraw', 'restore')),
	changed_by TEXT NOT NULL,
	changed_by_user BLOB CHECK (length(changed_by_user) = 16),
	before TEXT NOT NULL CHECK (json_valid(before) AND json_type(before) = 'object'),
	after TEXT NOT NULL CHECK (json_valid(after) AND json_type(after) = 'object'),
	-- The change a revert went back before.
	reverts BLOB REFERENCES place_changes (id),
	CHECK ((kind = 'revert') = (reverts IS NOT NULL)),
	FOREIGN KEY (brand_id, location_id) REFERENCES places (brand_id, location_id)
) STRICT;
CREATE INDEX place_changes_by_place ON place_changes (brand_id, location_id, at);
CREATE TRIGGER place_changes_no_delete BEFORE DELETE ON place_changes
BEGIN
	SELECT RAISE(ABORT, 'place_changes is append-only: DELETE refused');
END;
CREATE TRIGGER place_changes_no_update BEFORE UPDATE ON place_changes
BEGIN
	SELECT RAISE(ABORT, 'place_changes is append-only: UPDATE refused');
END;
CREATE TABLE brand_locales (
	brand_id TEXT PRIMARY KEY CHECK (length(brand_id) BETWEEN 1 AND 64 AND brand_id GLOB '[a-z0-9]*' AND brand_id NOT GLOB '*[^a-z0-9_-]*'),
	-- A JSON array of locales, never empty.
	locales TEXT NOT NULL CHECK (json_valid(locales) AND json_type(locales) = 'array' AND json_array_length(locales) > 0),
	updated_at INTEGER NOT NULL,
	-- A user's email, or `cli`.
	updated_by TEXT NOT NULL
) STRICT;
CREATE TABLE pricing (
	brand_id TEXT PRIMARY KEY CHECK (length(brand_id) BETWEEN 1 AND 64 AND brand_id GLOB '[a-z0-9]*' AND brand_id NOT GLOB '*[^a-z0-9_-]*'),
	model TEXT CHECK (model IS NULL OR (json_valid(model) AND json_type(model) = 'object')),
	-- Strictly increasing per brand: the token of optimistic concurrency.
	updated_at INTEGER NOT NULL,
	updated_by TEXT NOT NULL,
	-- The concierge user id; NULL for the CLI.
	updated_by_user BLOB CHECK (length(updated_by_user) = 16)
) STRICT;
CREATE TRIGGER pricing_no_delete BEFORE DELETE ON pricing
BEGIN
	SELECT RAISE(ABORT, 'a brand''s pricing is removed by setting it NULL, not deleted: DELETE refused');
END;
CREATE TABLE pricing_changes (
	id BLOB PRIMARY KEY CHECK (length(id) = 16),
	brand_id TEXT NOT NULL REFERENCES pricing (brand_id),
	at INTEGER NOT NULL,
	kind TEXT NOT NULL CHECK (kind IN ('set', 'remove')),
	changed_by TEXT NOT NULL,
	changed_by_user BLOB CHECK (length(changed_by_user) = 16),
	before TEXT CHECK (before IS NULL OR (json_valid(before) AND json_type(before) = 'object')),
	after TEXT CHECK (after IS NULL OR (json_valid(after) AND json_type(after) = 'object')),
	CHECK ((kind = 'remove') = (after IS NULL)),
	CHECK (kind = 'set' OR before IS NOT NULL)
) STRICT;
CREATE INDEX pricing_changes_by_brand ON pricing_changes (brand_id, at);
CREATE TRIGGER pricing_changes_no_delete BEFORE DELETE ON pricing_changes
BEGIN
	SELECT RAISE(ABORT, 'pricing_changes is append-only: DELETE refused');
END;
CREATE TRIGGER pricing_changes_no_update BEFORE UPDATE ON pricing_changes
BEGIN
	SELECT RAISE(ABORT, 'pricing_changes is append-only: UPDATE refused');
END;
CREATE TABLE booking_events (
	event_id BLOB PRIMARY KEY REFERENCES events (id),
	brand_id TEXT NOT NULL,
	kind TEXT NOT NULL CHECK (kind IN ('requested', 'created', 'canceled', 'set', 'status_changed', 'cleared', 'attached')),
	provider TEXT CHECK (provider IN ('manual', 'link', 'google_calendar', 'cal_com')),
	-- A provider's booking: set exactly for its own events and an attachment.
	external_ref TEXT,
	-- The lead the event names (a provider's: the one it was matched to as it arrived).
	lead_id TEXT,
	-- `status_changed`: done | no_show | canceled.
	status TEXT CHECK (status IN ('done', 'no_show', 'canceled')),
	-- `created` and `set`: the slot's start.
	start_at INTEGER,
	occurred_at INTEGER NOT NULL,
	CHECK ((external_ref IS NOT NULL) = (kind IN ('created', 'canceled', 'attached'))),
	CHECK ((status IS NOT NULL) = (kind = 'status_changed'))
) STRICT;
CREATE INDEX booking_events_by_booking ON booking_events (brand_id, provider, external_ref) WHERE external_ref IS NOT NULL;
CREATE TABLE bookings (
	-- Derived from (brand, provider, external_ref): what the API names it by.
	id BLOB PRIMARY KEY CHECK (length(id) = 16),
	brand_id TEXT NOT NULL,
	provider TEXT NOT NULL CHECK (provider IN ('google_calendar', 'cal_com')),
	external_ref TEXT NOT NULL,
	lead_id TEXT,
	match TEXT CHECK (match IN ('ref', 'contact', 'manual')),
	status TEXT NOT NULL CHECK (status IN ('booked', 'canceled')),
	-- The latest slot, kept once canceled.
	start_at INTEGER NOT NULL,
	end_at INTEGER,
	-- When the customer booked, as the provider says.
	booked_at INTEGER,
	-- The latest booking.created: the attendee's sealed PII is its.
	created_event_id BLOB NOT NULL REFERENCES events (id),
	first_event_id BLOB NOT NULL REFERENCES events (id),
	last_event_id BLOB NOT NULL REFERENCES events (id),
	last_event_at INTEGER NOT NULL,
	UNIQUE (brand_id, provider, external_ref),
	CHECK ((lead_id IS NULL) = (match IS NULL))
) STRICT;
CREATE INDEX bookings_unmatched ON bookings (brand_id, start_at) WHERE lead_id IS NULL;
CREATE INDEX bookings_by_lead ON bookings (brand_id, lead_id) WHERE lead_id IS NOT NULL;
CREATE TABLE booking_sync (
	provider TEXT NOT NULL CHECK (provider IN ('google_calendar', 'cal_com')),
	brand_id TEXT NOT NULL,
	-- The provider's cursor (Google's nextSyncToken); NULL: the next pull is a full one.
	cursor TEXT,
	holder BLOB,
	leased_until INTEGER,
	-- The last try, and the last pull that finished.
	attempted_at INTEGER,
	synced_at INTEGER,
	PRIMARY KEY (provider, brand_id)
) STRICT;
CREATE TABLE experiments (
	brand_id TEXT NOT NULL,
	key TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 64 AND key NOT GLOB '*[^a-z0-9_]*'),
	-- As the latest declaration naming it said; JSON arrays, the control first.
	variants TEXT NOT NULL CHECK (json_valid(variants) AND json_type(variants) = 'array'),
	declared_weights TEXT NOT NULL CHECK (json_valid(declared_weights) AND json_type(declared_weights) = 'array'),
	declared_enabled INTEGER NOT NULL CHECK (declared_enabled IN (0, 1)),
	declared_holdout REAL CHECK (declared_holdout >= 0 AND declared_holdout < 1),
	summary TEXT,
	declared_at INTEGER NOT NULL,
	first_declared_at INTEGER NOT NULL,
	-- What an admin set; NULL follows the declaration. Kept as set: the landings (and the
	-- screens' `effective`) ignore a field that no longer fits the declaration.
	override_enabled INTEGER CHECK (override_enabled IN (0, 1)),
	override_weights TEXT CHECK (json_valid(override_weights) AND json_type(override_weights) = 'array'),
	override_holdout REAL CHECK (override_holdout >= 0 AND override_holdout < 1),
	-- The admin's concierge id and when, while anything is set.
	changed_by TEXT,
	changed_at INTEGER,
	weights_changed_at INTEGER,
	-- Not in the brand's latest declaration.
	retired INTEGER NOT NULL CHECK (retired IN (0, 1)),
	CHECK ((changed_by IS NULL) = (changed_at IS NULL)),
	PRIMARY KEY (brand_id, key)
) STRICT;
CREATE TABLE posthog_outbox (
	event_id BLOB PRIMARY KEY REFERENCES events (id),
	event TEXT NOT NULL CHECK (event GLOB 'sa_[a-z]*'),
	distinct_id TEXT NOT NULL CHECK (length(distinct_id) BETWEEN 1 AND 200),
	properties TEXT NOT NULL CHECK (json_valid(properties) AND json_type(properties) = 'object'),
	occurred_at INTEGER NOT NULL,
	queued_at INTEGER NOT NULL,
	tries INTEGER NOT NULL DEFAULT 0 CHECK (tries >= 0),
	next_attempt_at INTEGER NOT NULL,
	-- Why the last try failed, PostHog's words cut short.
	last_error TEXT
) STRICT;
CREATE INDEX posthog_outbox_due ON posthog_outbox (next_attempt_at, event_id);
CREATE TABLE invoices (
	-- Derived from the operator and their idempotency key.
	id BLOB PRIMARY KEY CHECK (length(id) = 16),
	-- Banking's id; NULL exactly while `creating`.
	invoice_id TEXT UNIQUE,
	-- Canonical decimal USDT, as banking takes it.
	amount TEXT NOT NULL CHECK (length(amount) BETWEEN 1 AND 32),
	memo TEXT NOT NULL,
	state TEXT NOT NULL CHECK (state IN ('creating', 'open', 'processing', 'paid', 'cancelled', 'expired')),
	-- The concierge user who issued it.
	created_by BLOB NOT NULL CHECK (length(created_by) = 16),
	created_at INTEGER NOT NULL,
	expires_at INTEGER NOT NULL,
	-- Banking's page on evinvest.ltd where the payer pays; NULL while `creating`.
	pay_url TEXT,
	-- Set with `paid`, never changed after: when the ledger booked it, and the payer's
	-- `auth_subject` (opaque).
	paid_at INTEGER,
	paid_by TEXT,
	-- The money event the payment was confirmed from: banking delivers at least once, and a
	-- redelivered event finds it set.
	paid_event_id TEXT UNIQUE,
	CHECK ((invoice_id IS NULL) = (state = 'creating')),
	CHECK ((paid_at IS NULL) = (state <> 'paid')),
	CHECK ((paid_at IS NULL) = (paid_by IS NULL)),
	CHECK (paid_event_id IS NULL OR state = 'paid')
) STRICT;
CREATE INDEX invoices_by_time ON invoices (created_at);
CREATE TABLE invoice_cursor (
	id INTEGER PRIMARY KEY CHECK (id = 1),
	position INTEGER NOT NULL CHECK (position >= 0)
) STRICT;
CREATE VIEW reporting_calls AS
SELECT event_id, brand_id, lead_id, location_id, kind, outcome, occurred_at, manual
FROM calls
/* reporting_calls(event_id,brand_id,lead_id,location_id,kind,outcome,occurred_at,manual) */;
CREATE VIEW reporting_payments AS
SELECT event_id, brand_id, lead_id, location_id, job_id, billed, commission, currency, occurred_at, manual
FROM payments
/* reporting_payments(event_id,brand_id,lead_id,location_id,job_id,billed,commission,currency,occurred_at,manual) */;
CREATE VIEW reporting_ingest_daily AS
SELECT date(received_at / 1000000, 'unixepoch') AS day, brand_id, source_kind, source_id, type, type_version, status,
	count(*) AS events
FROM events
GROUP BY 1, 2, 3, 4, 5, 6, 7
/* reporting_ingest_daily(day,brand_id,source_kind,source_id,type,type_version,status,events) */;
CREATE VIEW reporting_leads AS
SELECT brand_id, lead_id, location_id, job_id, stage, channel, manual, suspect,
	created_at, contacted_at, quoted_at, won_at, completed_at, paid_at, lost_at, lost_reason, last_event_at,
	flow, quoted_cents, pricing_valid_from, estimate_inputs,
	message_ref, messaged_at, messaged_channel
FROM leads
/* reporting_leads(brand_id,lead_id,location_id,job_id,stage,channel,manual,suspect,created_at,contacted_at,quoted_at,won_at,completed_at,paid_at,lost_at,lost_reason,last_event_at,flow,quoted_cents,pricing_valid_from,estimate_inputs,message_ref,messaged_at,messaged_channel) */;
CREATE VIEW reporting_funnel_daily AS
SELECT date(created_at / 1000000, 'unixepoch') AS day, brand_id, location_id,
	count(*) AS leads,
	count(contacted_at) AS contacted,
	count(quoted_at) AS quoted,
	count(won_at) AS won,
	count(completed_at) AS completed,
	count(paid_at) AS paid,
	count(*) FILTER (WHERE stage = 'lost') AS lost_now,
	count(*) FILTER (WHERE manual) AS manual,
	count(suspect) AS suspect,
	count(*) FILTER (WHERE flow = 'estimate') AS estimate,
	count(*) FILTER (WHERE flow = 'fixed') AS fixed,
	count(messaged_at) AS messaged
FROM leads
WHERE created_at IS NOT NULL
GROUP BY 1, 2, 3
/* reporting_funnel_daily(day,brand_id,location_id,leads,contacted,quoted,won,completed,paid,lost_now,manual,suspect,estimate,fixed,messaged) */;
INSERT INTO invoice_cursor (id, position) VALUES (1, 0);

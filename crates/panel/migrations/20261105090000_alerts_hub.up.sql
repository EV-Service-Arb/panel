-- Alerts: the hub's access snapshots, destinations and their rules, and the services' feeds;
-- Telegram one way a destination is reached. A private chat linked before is a destination with
-- the rules its user chose.

-- What concierge last confirmed a user holds, and when; permissions NULL once their access is
-- known lost. Nothing is sent on a confirmation older than the hub's ACCESS_TTL.
CREATE TABLE alert_access (
	user_id BLOB PRIMARY KEY CHECK (length(user_id) = 16),
	permissions TEXT CHECK (json_valid(permissions) AND json_type(permissions) = 'array'),
	confirmed_at INTEGER NOT NULL,
	-- When the hub last tried to confirm it, answer or not: it asks at most so often.
	tried_at INTEGER,
	-- preferred_name or email: what "taken by" says.
	display_name TEXT NOT NULL
) STRICT;

INSERT INTO alert_access (user_id, permissions, confirmed_at, tried_at, display_name)
SELECT user_id, permissions, permissions_checked_at, access_tried_at, display_name FROM telegram_links;

CREATE TABLE alert_destinations (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	user_id BLOB NOT NULL CHECK (length(user_id) = 16),
	method TEXT NOT NULL CHECK (method IN ('telegram')),
	-- The GMB rules only for this gmail's GMBs; NULL: every GMB of the user's.
	gmail_id INTEGER REFERENCES gmails (id),
	created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX alert_destinations_by_user ON alert_destinations (user_id);

-- A rule turned on or off at a destination; a rule without a row is at its default.
CREATE TABLE alert_rules (
	destination_id INTEGER NOT NULL REFERENCES alert_destinations (id) ON DELETE CASCADE,
	rule TEXT NOT NULL CHECK (rule IN (
		'new_lead', 'contact_overdue', 'payment_received', 'source_silent', 'booked', 'access_requested',
		'review_new', 'review_changed', 'review_gone', 'review_reappeared', 'scan_failed'
	)),
	enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
	PRIMARY KEY (destination_id, rule)
) STRICT;

-- A destination reached by Telegram: the user's private chat with the bot (one per user), or a
-- channel, a group or one forum topic of it the user administers.
CREATE TABLE telegram_destinations (
	destination_id INTEGER PRIMARY KEY REFERENCES alert_destinations (id) ON DELETE CASCADE,
	kind TEXT NOT NULL CHECK (kind IN ('private', 'channel', 'group')),
	chat_id INTEGER NOT NULL,
	topic_id INTEGER CHECK (topic_id IS NULL OR (kind = 'group' AND topic_id > 0)),
	-- A private chat's account ("@username", else its first name); a channel's or group's title.
	title TEXT,
	-- Nothing goes there until a send succeeds again (a link, a test).
	dead_at INTEGER,
	dead_reason TEXT,
	CHECK ((dead_at IS NULL) = (dead_reason IS NULL)),
	CHECK ((kind = 'private') = (chat_id > 0))
) STRICT;

CREATE UNIQUE INDEX telegram_destinations_by_chat ON telegram_destinations (chat_id, ifnull(topic_id, 0));

INSERT INTO alert_destinations (user_id, method, created_at)
SELECT user_id, 'telegram', linked_at FROM telegram_links ORDER BY linked_at, user_id;

INSERT INTO telegram_destinations (destination_id, kind, chat_id, title, dead_at, dead_reason)
SELECT d.id, 'private', l.chat_id, coalesce('@' || l.tg_username, l.tg_first_name), l.dead_at,
	CASE WHEN l.dead_at IS NOT NULL THEN 'the bot is blocked, or the chat is gone' END
FROM telegram_links l JOIN alert_destinations d ON d.user_id = l.user_id;

INSERT INTO alert_rules (destination_id, rule, enabled)
SELECT d.id, r.rule, r.enabled FROM telegram_rules r JOIN alert_destinations d ON d.user_id = r.user_id;

-- Where each service's notes were read to: a page is taken only from where its cursor stands.
CREATE TABLE alert_feeds (
	service TEXT PRIMARY KEY CHECK (service IN ('sites', 'review_archive')),
	after INTEGER NOT NULL CHECK (after >= 0)
) STRICT;

CREATE TRIGGER alert_feeds_forward BEFORE UPDATE ON alert_feeds
WHEN NEW.after < OLD.after OR NEW.service IS NOT OLD.service
BEGIN
	SELECT RAISE(ABORT, 'a feed cursor only moves forward: UPDATE refused');
END;

INSERT INTO alert_feeds (service, after) SELECT 'sites', notes_after FROM telegram_poller;
INSERT INTO alert_feeds (service, after) VALUES ('review_archive', 0);

ALTER TABLE telegram_poller DROP COLUMN notes_after;

ALTER TABLE telegram_fanout RENAME TO alert_fanout;
DROP INDEX telegram_fanout_by_time;
CREATE INDEX alert_fanout_by_time ON alert_fanout (fanned_at);

-- The outbox, per destination: a chat's topics are destinations of their own. A message for a
-- chat no destination stands for any more (the user linked another) goes.
CREATE TABLE telegram_outbox_new (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	destination_id INTEGER NOT NULL REFERENCES alert_destinations (id) ON DELETE CASCADE,
	user_id BLOB NOT NULL CHECK (length(user_id) = 16),
	chat_id INTEGER NOT NULL,
	topic_id INTEGER,
	rule TEXT NOT NULL,
	event_id BLOB NOT NULL,
	buttons TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(buttons) AND json_type(buttons) = 'array'),
	-- A path on the rule's service: the photo, fetched under the conductor's assertion at the send.
	photo TEXT,
	text_sealed BLOB,
	data_key_fp BLOB CHECK (length(data_key_fp) = 32),
	state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'sent', 'dead')),
	attempts INTEGER NOT NULL DEFAULT 0,
	next_attempt_at INTEGER NOT NULL,
	leased_until INTEGER,
	attempted_at INTEGER,
	sent_at INTEGER,
	tg_message_id INTEGER,
	last_error TEXT,
	created_at INTEGER NOT NULL,
	UNIQUE (rule, event_id, destination_id),
	CHECK ((state = 'pending') = (text_sealed IS NOT NULL)),
	CHECK ((text_sealed IS NULL) = (data_key_fp IS NULL))
) STRICT;

INSERT INTO telegram_outbox_new (id, destination_id, user_id, chat_id, rule, event_id, buttons, text_sealed, data_key_fp, state,
	attempts, next_attempt_at, leased_until, attempted_at, sent_at, tg_message_id, last_error, created_at)
SELECT o.id, t.destination_id, o.user_id, o.chat_id, o.rule, o.event_id, o.buttons, o.text_sealed, o.data_key_fp, o.state,
	o.attempts, o.next_attempt_at, o.leased_until, o.attempted_at, o.sent_at, o.tg_message_id, o.last_error, o.created_at
FROM telegram_outbox o
JOIN telegram_destinations t ON t.chat_id = o.chat_id AND t.topic_id IS NULL
JOIN alert_destinations d ON d.id = t.destination_id AND d.user_id = o.user_id;

-- A button's data names its message's id: one handed out before is never handed out again.
INSERT INTO sqlite_sequence (name, seq)
SELECT 'telegram_outbox_new', seq FROM sqlite_sequence WHERE name = 'telegram_outbox'
	AND NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'telegram_outbox_new');
UPDATE sqlite_sequence SET seq = max(seq, (SELECT seq FROM sqlite_sequence WHERE name = 'telegram_outbox'))
WHERE name = 'telegram_outbox_new' AND EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'telegram_outbox');

DROP TABLE telegram_outbox;
ALTER TABLE telegram_outbox_new RENAME TO telegram_outbox;

CREATE INDEX telegram_outbox_due ON telegram_outbox (next_attempt_at, id) WHERE state = 'pending';
CREATE INDEX telegram_outbox_by_attempt ON telegram_outbox (attempted_at);
CREATE INDEX telegram_outbox_by_chat ON telegram_outbox (chat_id, attempted_at);
CREATE INDEX telegram_outbox_by_destination ON telegram_outbox (destination_id, rule) WHERE state = 'pending';

DROP TABLE telegram_rules;
DROP TABLE telegram_links;

-- Back to one private chat per user: channels, groups and their messages go, and so do the rules
-- the earlier build did not know.

CREATE TABLE telegram_links (
	user_id BLOB PRIMARY KEY CHECK (length(user_id) = 16),
	chat_id INTEGER NOT NULL UNIQUE,
	linked_at INTEGER NOT NULL,
	permissions TEXT CHECK (json_valid(permissions) AND json_type(permissions) = 'array'),
	permissions_checked_at INTEGER NOT NULL,
	access_tried_at INTEGER,
	display_name TEXT NOT NULL,
	dead_at INTEGER,
	tg_username TEXT,
	tg_first_name TEXT
) STRICT;

INSERT INTO telegram_links (user_id, chat_id, linked_at, permissions, permissions_checked_at, access_tried_at, display_name, dead_at,
	tg_username, tg_first_name)
SELECT d.user_id, t.chat_id, d.created_at, a.permissions, a.confirmed_at, a.tried_at, a.display_name, t.dead_at,
	CASE WHEN t.title LIKE '@%' THEN substr(t.title, 2) END, CASE WHEN t.title NOT LIKE '@%' THEN t.title END
FROM telegram_destinations t
JOIN alert_destinations d ON d.id = t.destination_id
JOIN alert_access a ON a.user_id = d.user_id
WHERE t.kind = 'private';

CREATE TABLE telegram_rules (
	user_id BLOB NOT NULL CHECK (length(user_id) = 16),
	rule TEXT NOT NULL CHECK (rule IN ('new_lead', 'contact_overdue', 'payment_received', 'source_silent', 'booked', 'access_requested')),
	enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
	PRIMARY KEY (user_id, rule)
) STRICT;

INSERT INTO telegram_rules (user_id, rule, enabled)
SELECT d.user_id, r.rule, r.enabled
FROM alert_rules r
JOIN alert_destinations d ON d.id = r.destination_id
JOIN telegram_destinations t ON t.destination_id = d.id AND t.kind = 'private'
WHERE r.rule IN ('new_lead', 'contact_overdue', 'payment_received', 'source_silent', 'booked', 'access_requested');

CREATE TABLE telegram_outbox_old (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	user_id BLOB NOT NULL CHECK (length(user_id) = 16),
	chat_id INTEGER NOT NULL,
	rule TEXT NOT NULL,
	event_id BLOB NOT NULL,
	buttons TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(buttons) AND json_type(buttons) = 'array'),
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
	UNIQUE (rule, event_id, chat_id),
	CHECK ((state = 'pending') = (text_sealed IS NOT NULL)),
	CHECK ((text_sealed IS NULL) = (data_key_fp IS NULL))
) STRICT;

INSERT INTO telegram_outbox_old (id, user_id, chat_id, rule, event_id, buttons, text_sealed, data_key_fp, state, attempts,
	next_attempt_at, leased_until, attempted_at, sent_at, tg_message_id, last_error, created_at)
SELECT o.id, o.user_id, o.chat_id, o.rule, o.event_id, o.buttons, o.text_sealed, o.data_key_fp, o.state, o.attempts,
	o.next_attempt_at, o.leased_until, o.attempted_at, o.sent_at, o.tg_message_id, o.last_error, o.created_at
FROM telegram_outbox o JOIN telegram_destinations t ON t.destination_id = o.destination_id AND t.kind = 'private';

INSERT INTO sqlite_sequence (name, seq)
SELECT 'telegram_outbox_old', seq FROM sqlite_sequence WHERE name = 'telegram_outbox'
	AND NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'telegram_outbox_old');
UPDATE sqlite_sequence SET seq = max(seq, (SELECT seq FROM sqlite_sequence WHERE name = 'telegram_outbox'))
WHERE name = 'telegram_outbox_old' AND EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'telegram_outbox');

DROP TABLE telegram_outbox;
ALTER TABLE telegram_outbox_old RENAME TO telegram_outbox;

CREATE INDEX telegram_outbox_due ON telegram_outbox (next_attempt_at, id) WHERE state = 'pending';
CREATE INDEX telegram_outbox_by_attempt ON telegram_outbox (attempted_at);
CREATE INDEX telegram_outbox_by_chat ON telegram_outbox (chat_id, attempted_at);

DROP INDEX alert_fanout_by_time;
ALTER TABLE alert_fanout RENAME TO telegram_fanout;
CREATE INDEX telegram_fanout_by_time ON telegram_fanout (fanned_at);

ALTER TABLE telegram_poller ADD COLUMN notes_after INTEGER NOT NULL DEFAULT 0;
UPDATE telegram_poller SET notes_after = (SELECT after FROM alert_feeds WHERE service = 'sites');

DROP TABLE alert_feeds;
DROP TABLE telegram_destinations;
DROP TABLE alert_rules;
DROP TABLE alert_destinations;
DROP TABLE alert_access;

-- People's gmails, the GMBs they manage and their tokens, moved from review_archive. Nothing is
-- deleted: a gmail or a GMB gets removed_at, and the ledger is append-only.

-- Who concierge has told the conductor of, upserted at every GetMe asked afresh.
CREATE TABLE users (
	user_id BLOB PRIMARY KEY CHECK (length(user_id) = 16),
	email TEXT NOT NULL,
	name TEXT NOT NULL,
	first_seen INTEGER NOT NULL,
	last_seen INTEGER NOT NULL
) STRICT;

CREATE TRIGGER users_no_delete BEFORE DELETE ON users
BEGIN
	SELECT RAISE(ABORT, 'users are never deleted: DELETE refused');
END;

CREATE TABLE gmails (
	-- The id every service stores; never reused.
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	owner BLOB NOT NULL REFERENCES users (user_id),
	gmail TEXT NOT NULL CHECK (gmail = lower(trim(gmail)) AND gmail NOT GLOB '*[ 	]*' AND length(gmail) - length(replace(gmail, '@', '')) = 1),
	created_at INTEGER NOT NULL,
	removed_at INTEGER
) STRICT;

CREATE UNIQUE INDEX gmails_owner_gmail ON gmails (owner, gmail) WHERE removed_at IS NULL;

CREATE TRIGGER gmails_no_delete BEFORE DELETE ON gmails
BEGIN
	SELECT RAISE(ABORT, 'gmails are never deleted, only removed: DELETE refused');
END;

CREATE TRIGGER gmails_only_removed BEFORE UPDATE ON gmails
WHEN NEW.id IS NOT OLD.id OR NEW.owner IS NOT OLD.owner OR NEW.gmail IS NOT OLD.gmail OR NEW.created_at IS NOT OLD.created_at
	OR OLD.removed_at IS NOT NULL
BEGIN
	SELECT RAISE(ABORT, 'a gmail is only removed, once: UPDATE refused');
END;

CREATE TABLE gmbs (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	gmail_id INTEGER NOT NULL REFERENCES gmails (id),
	place_id TEXT NOT NULL CHECK (length(place_id) > 0),
	name TEXT,
	address TEXT,
	gbp_account TEXT,
	gbp_location TEXT,
	created_at INTEGER NOT NULL,
	removed_at INTEGER
) STRICT;

CREATE UNIQUE INDEX gmbs_gmail_place ON gmbs (gmail_id, place_id) WHERE removed_at IS NULL;

CREATE TRIGGER gmbs_no_delete BEFORE DELETE ON gmbs
BEGIN
	SELECT RAISE(ABORT, 'GMBs are never deleted, only removed: DELETE refused');
END;

CREATE TRIGGER gmbs_only_removed BEFORE UPDATE ON gmbs
WHEN NEW.id IS NOT OLD.id OR NEW.gmail_id IS NOT OLD.gmail_id OR NEW.place_id IS NOT OLD.place_id OR NEW.name IS NOT OLD.name
	OR NEW.address IS NOT OLD.address OR NEW.gbp_account IS NOT OLD.gbp_account OR NEW.gbp_location IS NOT OLD.gbp_location
	OR NEW.created_at IS NOT OLD.created_at OR OLD.removed_at IS NOT NULL
BEGIN
	SELECT RAISE(ABORT, 'a GMB is only removed, once: UPDATE refused');
END;

-- A balance is the sum of its user's rows. A charge or a kick-back of a service names it and
-- what it charged for (`key`, e.g. run:42).
CREATE TABLE token_ledger (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	user_id BLOB NOT NULL REFERENCES users (user_id),
	at INTEGER NOT NULL,
	delta INTEGER NOT NULL,
	kind TEXT NOT NULL CHECK (kind IN ('accrual', 'add', 'set', 'charge', 'kickback')),
	service TEXT,
	key TEXT,
	label TEXT,
	by_email TEXT,
	note TEXT,
	CHECK ((service IS NULL) = (key IS NULL)),
	CHECK (kind != 'charge' OR service IS NOT NULL)
) STRICT;

CREATE INDEX token_ledger_user ON token_ledger (user_id, id);

CREATE TRIGGER token_ledger_no_delete BEFORE DELETE ON token_ledger
BEGIN
	SELECT RAISE(ABORT, 'the token ledger is append-only: DELETE refused');
END;

CREATE TRIGGER token_ledger_no_update BEFORE UPDATE ON token_ledger
BEGIN
	SELECT RAISE(ABORT, 'the token ledger is append-only: UPDATE refused');
END;

-- What a user is owed until they claim it: a charge kicked back (a service's), or an admin's grant.
CREATE TABLE token_kickbacks (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	user_id BLOB NOT NULL REFERENCES users (user_id),
	service TEXT,
	key TEXT,
	label TEXT,
	amount INTEGER NOT NULL CHECK (amount > 0),
	at INTEGER NOT NULL,
	claimed_at INTEGER,
	by_email TEXT,
	note TEXT,
	CHECK ((service IS NOT NULL AND key IS NOT NULL AND by_email IS NULL AND note IS NULL)
		OR (service IS NULL AND key IS NULL AND by_email IS NOT NULL AND note IS NOT NULL))
) STRICT;

CREATE INDEX token_kickbacks_pending ON token_kickbacks (user_id) WHERE claimed_at IS NULL;

CREATE TRIGGER token_kickbacks_no_delete BEFORE DELETE ON token_kickbacks
BEGIN
	SELECT RAISE(ABORT, 'kick-backs are never deleted, only claimed: DELETE refused');
END;

CREATE TRIGGER token_kickbacks_only_claimed BEFORE UPDATE ON token_kickbacks
WHEN NEW.id IS NOT OLD.id OR NEW.user_id IS NOT OLD.user_id OR NEW.service IS NOT OLD.service OR NEW.key IS NOT OLD.key
	OR NEW.label IS NOT OLD.label OR NEW.amount IS NOT OLD.amount OR NEW.at IS NOT OLD.at OR NEW.by_email IS NOT OLD.by_email
	OR NEW.note IS NOT OLD.note OR OLD.claimed_at IS NOT NULL
BEGIN
	SELECT RAISE(ABORT, 'a kick-back is only claimed, once: UPDATE refused');
END;

-- The last of a service's charges in the ledger. review_archive's row is written by the import:
-- until then tokens and GMBs are closed.
CREATE TABLE charge_cursors (
	service TEXT PRIMARY KEY,
	after INTEGER NOT NULL CHECK (after >= 0)
) STRICT;

CREATE TRIGGER charge_cursors_forward BEFORE UPDATE ON charge_cursors
WHEN NEW.after < OLD.after OR NEW.service IS NOT OLD.service
BEGIN
	SELECT RAISE(ABORT, 'a charge cursor only moves forward: UPDATE refused');
END;

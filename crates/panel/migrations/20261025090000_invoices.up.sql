-- The invoices the panel asks banking's ScopeLedger for (scope `sa`, `scope.v1.InvoiceService`),
-- and the position of banking's money events the payments are confirmed from. Banking is the
-- authority; a row mirrors what it last said. New tables only: the build before this one never
-- names them, and nothing here touches the journal or `payments`.

-- One row per invoice an operator asked for. The row is stored before banking is called
-- (`creating`, no `invoice_id`), so a retry under the same idempotency key resends the same
-- terms. Times are microseconds since the epoch, as everywhere in this file.
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

-- Where the next `MoneyEvents.Pull` starts (exclusive). One row, moved in the same transaction
-- as the invoices the pulled page marked.
CREATE TABLE invoice_cursor (
	id INTEGER PRIMARY KEY CHECK (id = 1),
	position INTEGER NOT NULL CHECK (position >= 0)
) STRICT;

INSERT INTO invoice_cursor (id, position) VALUES (1, 0);

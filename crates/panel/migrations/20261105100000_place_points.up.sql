-- Where a GMB's place is, as Places last answered: keyed by place, so a GMB row stays as added.
CREATE TABLE place_points (
	place_id TEXT PRIMARY KEY,
	lat REAL NOT NULL,
	lng REAL NOT NULL
) STRICT;

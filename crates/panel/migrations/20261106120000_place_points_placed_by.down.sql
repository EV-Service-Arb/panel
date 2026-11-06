CREATE TABLE place_points_old (
	place_id TEXT PRIMARY KEY,
	lat REAL NOT NULL,
	lng REAL NOT NULL
) STRICT;
INSERT INTO place_points_old SELECT place_id, lat, lng FROM place_points WHERE placed_by IS NULL;
DROP TABLE place_points;
ALTER TABLE place_points_old RENAME TO place_points;

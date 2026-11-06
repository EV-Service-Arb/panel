-- Who placed a point by hand on the map; NULL: Places' own.
ALTER TABLE place_points ADD COLUMN placed_by BLOB REFERENCES users (user_id);

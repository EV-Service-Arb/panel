-- The GMBs their owner switched off; one absent here is on. review_archive inherits the switch
-- through the snapshot.
CREATE TABLE gmbs_off (
	gmb_id INTEGER PRIMARY KEY REFERENCES gmbs (id)
) STRICT;

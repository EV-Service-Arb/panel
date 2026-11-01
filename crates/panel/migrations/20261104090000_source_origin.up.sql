-- A site key's origin: where its landing answers `/.well-known/sa-site`. Set when the key is
-- issued, or once later for a key issued before origins (NULL → a value), never changed after.
ALTER TABLE sources ADD COLUMN origin TEXT CHECK (origin IS NULL OR kind = 'site');

DROP TRIGGER sources_only_revoked;
CREATE TRIGGER sources_only_revoked BEFORE UPDATE ON sources
WHEN NEW.key_id IS NOT OLD.key_id OR NEW.kind IS NOT OLD.kind OR NEW.brand_ids IS NOT OLD.brand_ids
	OR NEW.secret_sealed IS NOT OLD.secret_sealed OR NEW.data_key_fp IS NOT OLD.data_key_fp OR NEW.created_at IS NOT OLD.created_at
	OR (NEW.origin IS NOT OLD.origin AND OLD.origin IS NOT NULL)
BEGIN
	SELECT RAISE(ABORT, 'a source only ever gets revoked, and its origin set once: UPDATE refused');
END;

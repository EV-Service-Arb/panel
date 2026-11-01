DROP TRIGGER sources_only_revoked;
CREATE TRIGGER sources_only_revoked BEFORE UPDATE ON sources
WHEN NEW.key_id IS NOT OLD.key_id OR NEW.kind IS NOT OLD.kind OR NEW.brand_ids IS NOT OLD.brand_ids
	OR NEW.secret_sealed IS NOT OLD.secret_sealed OR NEW.data_key_fp IS NOT OLD.data_key_fp OR NEW.created_at IS NOT OLD.created_at
BEGIN
	SELECT RAISE(ABORT, 'a source only ever gets revoked: UPDATE refused');
END;

ALTER TABLE sources DROP COLUMN origin;

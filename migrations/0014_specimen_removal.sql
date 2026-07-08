-- Soft delete: a curator can take a specimen down from the gallery.
-- NULL = live (visible). Set to now() on removal; cleared to NULL on restore.
-- The row is kept so ingest's ON CONFLICT (rkey) DO NOTHING can't resurrect it.
ALTER TABLE specimens ADD COLUMN removed_at TIMESTAMPTZ;
ALTER TABLE specimens ADD COLUMN removed_by TEXT;

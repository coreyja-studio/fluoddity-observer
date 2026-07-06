-- Still images join the expedition record. A specimen is one post; image
-- posts carry up to four images, so those live in a child table (position
-- 0 duplicates the specimen's own cid/file for uniform rendering).
ALTER TABLE specimens
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'video'
        CHECK (kind IN ('video', 'image'));

CREATE TABLE specimen_images (
    rkey TEXT NOT NULL REFERENCES specimens (rkey) ON DELETE CASCADE,
    position INT NOT NULL,
    cid TEXT NOT NULL,
    file TEXT,
    alt TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (rkey, position)
);

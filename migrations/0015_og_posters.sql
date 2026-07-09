-- Vault key (og/{rkey}.jpg) for a generated 1200x630 OG poster (mid-clip frame).
-- Set by `gen-posters` after ffmpeg extraction + Bunny upload.
-- NULL means no OG poster has been generated yet.
ALTER TABLE specimens ADD COLUMN og_poster_key TEXT;

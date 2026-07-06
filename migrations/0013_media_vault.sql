-- The vault: Bunny Storage keys for the museum's own copies of each video.
-- pds_key is the original blob pulled from the artist's PDS and mirrored to
-- the CDN; master_key is a render-node master the artist uploaded himself.
-- Serving preference is master, then PDS copy, then the Bluesky CDN.
ALTER TABLE specimens ADD COLUMN pds_key TEXT;
ALTER TABLE specimens ADD COLUMN master_key TEXT;

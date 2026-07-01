-- The expedition record: every specimen from the artist's archive.
CREATE TABLE specimens (
    rkey TEXT PRIMARY KEY,
    cid TEXT NOT NULL,
    -- Path relative to the media dir; NULL when the blob hasn't been pulled
    -- locally (freshly ingested specimens are served from the Bluesky CDN).
    file TEXT,
    caption TEXT NOT NULL DEFAULT '',
    collected_on DATE NOT NULL,
    url TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The editorial layer: rooms, membership, lineage families, margin notes.
CREATE TABLE rooms (
    slug TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    position INT NOT NULL
);

CREATE TABLE room_specimens (
    room_slug TEXT NOT NULL REFERENCES rooms (slug) ON DELETE CASCADE,
    rkey TEXT NOT NULL REFERENCES specimens (rkey) ON DELETE CASCADE,
    position INT NOT NULL,
    PRIMARY KEY (room_slug, rkey)
);

CREATE TABLE families (
    slug TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    position INT NOT NULL
);

CREATE TABLE family_members (
    family_slug TEXT NOT NULL REFERENCES families (slug) ON DELETE CASCADE,
    rkey TEXT NOT NULL REFERENCES specimens (rkey) ON DELETE CASCADE,
    position INT NOT NULL,
    PRIMARY KEY (family_slug, rkey)
);

CREATE TABLE margin_notes (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    rkey TEXT NOT NULL REFERENCES specimens (rkey) ON DELETE CASCADE,
    handle TEXT NOT NULL,
    note TEXT NOT NULL,
    position INT NOT NULL DEFAULT 0
);

CREATE INDEX margin_notes_rkey_idx ON margin_notes (rkey);

-- Single-row gallery configuration: the artist and the origin wish.
CREATE TABLE gallery_meta (
    only_row BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (only_row),
    artist_handle TEXT NOT NULL,
    artist_did TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    origin_handle TEXT NOT NULL,
    origin_text TEXT NOT NULL,
    origin_url TEXT NOT NULL
);

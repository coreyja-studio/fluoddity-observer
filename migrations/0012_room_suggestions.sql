-- Room registration suggestions: when the bot hangs a room answer on a
-- thread that actually holds specimens, it files the thread here so a
-- curator can decide whether it belongs on the homepage. Declined rows
-- are kept so the same thread isn't re-suggested.
CREATE TABLE room_suggestions (
    suggestion_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    author_did TEXT NOT NULL,
    rkey TEXT NOT NULL,
    author_handle TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    hung_count INT NOT NULL DEFAULT 0,
    source_uri TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'declined')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by TEXT,
    UNIQUE (author_did, rkey)
);

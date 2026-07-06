-- The suggestion box: community members tag specimens from inside Bluesky
-- (hashtags in replies to / quote-posts of the original post). Suggestions
-- queue for the curator's desk; declined rows are kept so a daily
-- re-harvest can't resurrect them.
CREATE TABLE tag_suggestions (
    suggestion_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    rkey TEXT NOT NULL REFERENCES specimens (rkey) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    suggester_did TEXT NOT NULL,
    suggester_handle TEXT NOT NULL DEFAULT '',
    via TEXT NOT NULL DEFAULT 'reply' CHECK (via IN ('reply', 'quote', 'mention')),
    source_uri TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'declined')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by TEXT,
    UNIQUE (rkey, tag, suggester_did)
);

CREATE INDEX tag_suggestions_pending ON tag_suggestions (created_at)
    WHERE status = 'pending';

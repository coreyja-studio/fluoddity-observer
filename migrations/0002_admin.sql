-- The curation roster: DIDs allowed into the admin portal.
CREATE TABLE curators (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL DEFAULT '',
    -- 'artist' (tier 0) or 'curator' (tier 1); the ladder grows later.
    role TEXT NOT NULL DEFAULT 'curator' CHECK (role IN ('artist', 'curator')),
    added_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Our own login sessions, minted after Bluesky OAuth verifies a DID.
CREATE TABLE admin_sessions (
    token TEXT PRIMARY KEY,
    did TEXT NOT NULL REFERENCES curators (did) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

-- Transient stores the atproto OAuth client needs mid-flow. State rows live
-- for minutes; session rows are deleted right after the callback because we
-- only use OAuth for identity, never to act on the user's PDS.
CREATE TABLE oauth_states (
    key TEXT PRIMARY KEY,
    data JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE oauth_sessions (
    did TEXT PRIMARY KEY,
    data JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

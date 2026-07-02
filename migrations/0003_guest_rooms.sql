-- Guest rooms: Bluesky threads registered as rooms in the gallery. The
-- thread IS the room — its author is the curator, their post text the wall
-- labels. Registration only controls what the gallery's front page lists;
-- any thread renders at /guest/{author}/{rkey} regardless.
CREATE TABLE guest_rooms (
    author_did TEXT NOT NULL,
    rkey TEXT NOT NULL,
    author_handle TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    added_by TEXT NOT NULL REFERENCES curators (did),
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (author_did, rkey)
);

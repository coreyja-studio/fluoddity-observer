-- Tags generalize the hard-coded lineage families. A tag has a kind
-- ('lineage' tags get evolution-strip treatment; plain 'tag' is a motif or
-- anything else) and a source ('curator' via the desk, 'post' parsed from
-- #hashtags in the artist's captions, 'community' reserved for later).
CREATE TABLE specimen_tags (
    rkey TEXT NOT NULL REFERENCES specimens (rkey) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'tag' CHECK (kind IN ('tag', 'lineage')),
    source TEXT NOT NULL DEFAULT 'curator' CHECK (source IN ('curator', 'post', 'community')),
    added_by TEXT NOT NULL DEFAULT '',
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (rkey, tag)
);

CREATE INDEX specimen_tags_tag_idx ON specimen_tags (tag);

-- Carry the curated lineage families over as lineage tags, then retire the
-- bespoke tables.
INSERT INTO specimen_tags (rkey, tag, kind, source)
SELECT fm.rkey, fm.family_slug, 'lineage', 'curator'
FROM family_members fm;

DROP TABLE family_members;

DROP TABLE families;

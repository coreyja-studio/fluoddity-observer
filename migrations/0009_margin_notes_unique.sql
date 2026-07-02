-- The margin-note refresh job re-fetches quote-posts; identical notes must
-- upsert, not duplicate. Dedupe anything already doubled, then constrain.
DELETE FROM margin_notes a
USING margin_notes b
WHERE a.id > b.id
  AND a.rkey = b.rkey
  AND a.handle = b.handle
  AND a.note = b.note;

CREATE UNIQUE INDEX margin_notes_unique_idx ON margin_notes (rkey, handle, note);

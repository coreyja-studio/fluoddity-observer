-- Ledger of mentions the gallery bot has answered — restarts and re-polls
-- must never double-reply.
CREATE TABLE bot_replies (
    mention_uri TEXT PRIMARY KEY,
    reply_uri TEXT NOT NULL DEFAULT '',
    replied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

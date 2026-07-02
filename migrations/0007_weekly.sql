-- Ledger of posted weekly wrap-ups (week_start = that week's Monday).
-- One thread per completed week, ever; silent weeks never get a row.
CREATE TABLE bot_weekly (
    week_start DATE PRIMARY KEY,
    root_uri TEXT NOT NULL DEFAULT '',
    posted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- cja framework tables: durable job queue + cron bookkeeping.
-- Mirrors cja's own migrations (AddJobsTable, AddCrons, AddJobErrorTracking,
-- AddDeadLetterJobs) so the framework's workers have their substrate.
CREATE TABLE IF NOT EXISTS jobs (
    job_id UUID PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    payload JSONB NOT NULL,
    priority INT NOT NULL,
    run_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_at TIMESTAMPTZ,
    locked_by TEXT,
    context TEXT NOT NULL,
    error_count INTEGER NOT NULL DEFAULT 0,
    last_error_message TEXT,
    last_failed_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS crons (
    cron_id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    last_run_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE UNIQUE INDEX idx_crons_name ON crons (name);

CREATE TABLE IF NOT EXISTS dead_letter_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    original_job_id UUID NOT NULL,
    name TEXT NOT NULL,
    payload JSONB NOT NULL,
    context TEXT NOT NULL,
    priority INT NOT NULL,
    error_count INTEGER NOT NULL,
    last_error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    failed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

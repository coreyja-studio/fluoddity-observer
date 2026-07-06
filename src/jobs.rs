//! Durable background jobs (cja job system). The cron registry (cron.rs)
//! enqueues these on their schedules; the job worker runs them with
//! automatic retries, exponential backoff, and a dead-letter queue.

use serde::{Deserialize, Serialize};

use crate::{AppState, bot, ingest, margin_notes, suggestions};

fn http_client() -> cja::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("paperclips-gallery/0.1 (fluoddity field guide)")
        .build()
        .map_err(|e| cja::color_eyre::eyre::eyre!("building http client: {e}"))
}

fn to_eyre(err: anyhow::Error) -> cja::color_eyre::Report {
    cja::color_eyre::eyre::eyre!("{err:?}")
}

/// Poll the artist's feed and archive any new video posts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestPoll;

#[async_trait::async_trait]
impl cja::jobs::Job<AppState> for IngestPoll {
    const NAME: &'static str = "IngestPoll";

    async fn run(&self, state: AppState) -> cja::Result<()> {
        let added = ingest::poll_once(&state.pool, &http_client()?)
            .await
            .map_err(to_eyre)?;
        if !added.is_empty() {
            tracing::info!(count = added.len(), rkeys = ?added, "ingest: new specimens collected");
        }
        Ok(())
    }
}

/// Answer Bluesky mentions of the gallery account with room links.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessMentions;

#[async_trait::async_trait]
impl cja::jobs::Job<AppState> for ProcessMentions {
    const NAME: &'static str = "ProcessMentions";

    async fn run(&self, state: AppState) -> cja::Result<()> {
        // Guarded here too in case the job was enqueued before the bot's
        // credentials were removed.
        let Some(cfg) = bot::BotConfig::from_env() else {
            return Ok(());
        };
        let replied = bot::poll_once(&state.pool, &http_client()?, &state.threads, &cfg)
            .await
            .map_err(to_eyre)?;
        if replied > 0 {
            tracing::info!(replies = replied, "bot: answered asks");
        }
        Ok(())
    }
}

/// Post the weekly wrap-up thread when a completed week is due. A no-op
/// almost always (ledger row exists, week incomplete, or silence rules).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyWrapup;

#[async_trait::async_trait]
impl cja::jobs::Job<AppState> for WeeklyWrapup {
    const NAME: &'static str = "WeeklyWrapup";

    async fn run(&self, state: AppState) -> cja::Result<()> {
        let Some(cfg) = bot::BotConfig::from_env() else {
            return Ok(());
        };
        let dry_run = std::env::var("PCG_BOT_DRY_RUN").is_ok();
        if let Some(picks) = bot::weekly_once(&state.pool, &http_client()?, &cfg, dry_run)
            .await
            .map_err(to_eyre)?
        {
            tracing::info!(picks, dry_run, "bot: weekly wrap-up handled");
        }
        Ok(())
    }
}

/// Re-pull quote-posts for every specimen; new community commentary
/// becomes margin notes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshMarginNotes;

#[async_trait::async_trait]
impl cja::jobs::Job<AppState> for RefreshMarginNotes {
    const NAME: &'static str = "RefreshMarginNotes";

    async fn run(&self, state: AppState) -> cja::Result<()> {
        let added = margin_notes::refresh_once(&state.pool, &http_client()?)
            .await
            .map_err(to_eyre)?;
        if added > 0 {
            tracing::info!(added, "margin notes refreshed");
        }
        Ok(())
    }
}

/// Harvest community hashtags (replies + quote-posts) into the
/// suggestion box; the artist's own reply-tags land directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestSuggestions;

#[async_trait::async_trait]
impl cja::jobs::Job<AppState> for HarvestSuggestions {
    const NAME: &'static str = "HarvestSuggestions";

    async fn run(&self, state: AppState) -> cja::Result<()> {
        let stats = suggestions::harvest_once(&state.pool, &http_client()?)
            .await
            .map_err(to_eyre)?;
        if stats.suggested > 0 || stats.artist_tagged > 0 {
            tracing::info!(
                suggested = stats.suggested,
                artist_tagged = stats.artist_tagged,
                "suggestion harvest complete"
            );
        }
        Ok(())
    }
}

cja::impl_job_registry!(
    AppState,
    IngestPoll,
    ProcessMentions,
    WeeklyWrapup,
    RefreshMarginNotes,
    HarvestSuggestions
);

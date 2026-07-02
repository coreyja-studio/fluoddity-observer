//! Cron schedules (cja cron system): each tick enqueues a durable job, so
//! the actual work inherits the job worker's retries and dead-lettering.

use std::time::Duration;

use cja::cron::{CronRegistry, Worker};

use crate::{AppState, bot, jobs};

fn registry() -> CronRegistry<AppState> {
    let mut registry = CronRegistry::new();

    let poll_secs: u64 = std::env::var("PCG_POLL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    if poll_secs > 0 {
        registry.register_job(
            jobs::IngestPoll,
            Some("poll the artist's feed for new specimens"),
            Duration::from_secs(poll_secs),
        );
    }

    if bot::BotConfig::from_env().is_some() {
        let bot_secs: u64 = std::env::var("PCG_BOT_POLL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        registry.register_job(
            jobs::ProcessMentions,
            Some("answer gallery mentions with room links"),
            Duration::from_secs(bot_secs),
        );
        registry.register_job(
            jobs::WeeklyWrapup,
            Some("post the weekly wrap-up when a completed week is due"),
            Duration::from_secs(60 * 60),
        );
    } else {
        tracing::info!("gallery bot not configured — mention/weekly crons not registered");
    }

    registry
}

pub async fn run_cron(state: AppState) -> cja::Result<()> {
    Worker::new(state, registry())
        .run(cja::jobs::CancellationToken::new())
        .await?;
    Ok(())
}

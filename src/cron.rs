//! Cron schedules (cja cron system): each tick enqueues a durable job, so
//! the actual work inherits the job worker's retries and dead-lettering.

use std::time::Duration;

use cja::cron::{CronRegistry, Worker};

use crate::{AppState, bot, jobs};

pub fn registry() -> CronRegistry<AppState> {
    let poll_secs: u64 = std::env::var("PCG_POLL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let bot_secs = bot::BotConfig::from_env().map(|_| {
        std::env::var("PCG_BOT_POLL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60)
    });
    registry_for(poll_secs, bot_secs)
}

fn registry_for(poll_secs: u64, bot_secs: Option<u64>) -> CronRegistry<AppState> {
    let mut registry = CronRegistry::new();
    if poll_secs > 0 {
        registry.register_job(
            jobs::IngestPoll,
            Some("poll the artist's feed for new specimens"),
            Duration::from_secs(poll_secs),
        );
    }

    registry.register_job(
        jobs::RefreshMarginNotes,
        Some("re-pull quote-posts so the margins stay current"),
        Duration::from_secs(24 * 60 * 60),
    );

    registry.register_job(
        jobs::HarvestSuggestions,
        Some("harvest community hashtags into the suggestion box"),
        Duration::from_secs(24 * 60 * 60),
    );

    if let Some(bot_secs) = bot_secs {
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

/// Runs until `shutdown` is cancelled; a tick already in flight finishes
/// enqueueing first.
pub async fn run_cron(
    state: AppState,
    registry: CronRegistry<AppState>,
    shutdown: cja::jobs::CancellationToken,
) -> cja::Result<()> {
    Worker::new(state, registry).run(shutdown).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_intervals_and_default_registry_are_complete() {
        let default = registry_for(300, None).entries();
        assert_eq!(default.len(), 3);
        assert!(default.contains(&("IngestPoll", "300s".to_string())));
        assert!(default.contains(&("RefreshMarginNotes", "86400s".to_string())));
        assert!(default.contains(&("HarvestSuggestions", "86400s".to_string())));
        let with_bot = registry_for(300, Some(60)).entries();
        assert_eq!(with_bot.len(), 5);
        assert!(with_bot.contains(&("ProcessMentions", "60s".to_string())));
        assert!(with_bot.contains(&("WeeklyWrapup", "3600s".to_string())));
        let no_ingest = registry_for(0, None).entries();
        assert_eq!(no_ingest.len(), 2);
        assert!(!no_ingest.iter().any(|(name, _)| *name == "IngestPoll"));
    }
}

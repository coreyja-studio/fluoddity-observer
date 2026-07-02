//! First-pass 2d/3d classification of the archive.
//!
//! The artist worked in 2D Fluoddity until late May 2026, then moved to 3D
//! (path-traced) — but kept posting occasional 2D pieces. This heuristic
//! tags every specimen `2d` or `3d` so the archive can be filtered by
//! dimension; it is deliberately correctable: tags are ordinary curator
//! tags (added_by marks the heuristic), and the inline tag editor on any
//! specimen page overrides a wrong call. The artist gets the final say.

use sqlx::PgPool;

/// The first fully-3D era post; before this date, 3D only appears when the
/// caption says so.
const THREE_D_ERA: &str = "2026-05-30";

/// Classify one specimen. Caption keywords win; the era boundary decides
/// the rest.
pub fn classify(caption: &str, date: &str) -> &'static str {
    let lower = caption.to_lowercase();
    // Explicit mentions are the strongest signal, and "2d" wins over "3d"
    // when both appear ("this is the same config in 2d").
    if lower.contains("2d") {
        return "2d";
    }
    if lower.contains("3d") {
        return "3d";
    }
    // Unambiguous 3D-only vocabulary from his captions.
    const THREE_D_WORDS: [&str; 6] = [
        "path trac",
        "raytrac",
        "ray trac",
        "optix",
        "denois",
        "depth of field",
    ];
    if THREE_D_WORDS.iter().any(|w| lower.contains(w)) {
        return "3d";
    }
    if date < THREE_D_ERA { "2d" } else { "3d" }
}

/// Apply the heuristic across the archive. Never overwrites an existing
/// dimension tag (a curator's correction is final); returns how many
/// specimens were newly classified.
pub async fn classify_archive(pool: &PgPool) -> anyhow::Result<usize> {
    let specimens = sqlx::query!(
        r#"SELECT s.rkey, s.caption, s.collected_on
           FROM specimens s
           WHERE NOT EXISTS (
               SELECT 1 FROM specimen_tags t
               WHERE t.rkey = s.rkey AND t.tag IN ('2d', '3d')
           )
           ORDER BY s.collected_on, s.rkey"#
    )
    .fetch_all(pool)
    .await?;

    let mut added = 0;
    for s in &specimens {
        let dimension = classify(&s.caption, &s.collected_on.format("%Y-%m-%d").to_string());
        let inserted = sqlx::query!(
            "INSERT INTO specimen_tags (rkey, tag, kind, source, added_by)
             VALUES ($1, $2, 'tag', 'curator', 'survey-heuristic')
             ON CONFLICT (rkey, tag) DO NOTHING",
            s.rkey,
            dimension,
        )
        .execute(pool)
        .await?;
        added += inserted.rows_affected() as usize;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_keywords_beat_the_era_boundary() {
        // 2D posted deep in the 3D era.
        assert_eq!(
            classify("This is the same config in 2d.", "2026-06-01"),
            "2d"
        );
        // 3D mentioned before the era boundary.
        assert_eq!(
            classify("my most recent attempt at 3d fluoddity", "2026-05-29"),
            "3d"
        );
        // "2d" wins when both appear.
        assert_eq!(
            classify("2d fluoddity vs the 3d version", "2026-06-10"),
            "2d"
        );
    }

    #[test]
    fn three_d_vocabulary_counts() {
        assert_eq!(
            classify(
                "Swimming pools with the new realtime path tracer.",
                "2026-06-15"
            ),
            "3d"
        );
        assert_eq!(classify("2spp+denoiser, realtime mode", "2026-06-30"), "3d");
    }

    #[test]
    fn era_boundary_decides_the_unlabelled() {
        assert_eq!(classify("Hatman!", "2026-05-26"), "2d");
        assert_eq!(classify("Koosh", "2026-06-18"), "3d");
    }
}

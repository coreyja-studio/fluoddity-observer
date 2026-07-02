//! Margin-note refresh: quote-posts of the artist's work keep arriving
//! after a specimen is archived. A daily job re-pulls quotes for every
//! specimen and appends any new voices worth keeping — the community's
//! commentary stays current without anyone tending it.

use anyhow::Context;
use sqlx::PgPool;

const PUBLIC_API: &str = "https://public.api.bsky.app";
/// Wall space is finite: at most this many notes per specimen.
const MAX_NOTES_PER_SPECIMEN: i64 = 6;

/// The editorial filter for what belongs in a margin. Link-only posts and
/// the artist quoting himself are noise, not commentary.
pub fn keep_note(handle: &str, text: &str, artist_handle: &str) -> bool {
    let text = text.trim();
    if handle == artist_handle {
        return false;
    }
    if text.len() < 4 {
        return false;
    }
    if text.starts_with("bsky.app/") || text.starts_with("http") {
        return false;
    }
    true
}

/// Quote-posts for one post, filtered to margin-worthy notes:
/// (handle, text) pairs.
pub fn parse_quotes(response: &serde_json::Value, artist_handle: &str) -> Vec<(String, String)> {
    let Some(posts) = response.get("posts").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    posts
        .iter()
        .filter_map(|post| {
            let handle = post.get("author")?.get("handle")?.as_str()?;
            let text = post.get("record")?.get("text")?.as_str()?.trim();
            keep_note(handle, text, artist_handle).then(|| (handle.to_string(), text.to_string()))
        })
        .collect()
}

/// Re-fetch quotes for every specimen and insert any new notes, oldest
/// specimens first. Returns the number of notes added.
pub async fn refresh_once(pool: &PgPool, client: &reqwest::Client) -> anyhow::Result<usize> {
    let meta = sqlx::query!("SELECT artist_did, artist_handle FROM gallery_meta")
        .fetch_one(pool)
        .await
        .context("gallery_meta is empty — run import first")?;

    let specimens = sqlx::query!("SELECT rkey FROM specimens ORDER BY collected_on, rkey")
        .fetch_all(pool)
        .await?;

    let mut added = 0;
    for specimen in &specimens {
        let uri = format!(
            "at://{}/app.bsky.feed.post/{}",
            meta.artist_did, specimen.rkey
        );
        let response = client
            .get(format!("{PUBLIC_API}/xrpc/app.bsky.feed.getQuotes"))
            .query(&[("uri", uri.as_str()), ("limit", "25")])
            .send()
            .await?;
        if !response.status().is_success() {
            tracing::warn!(rkey = %specimen.rkey, status = %response.status(), "getQuotes failed; skipping");
            continue;
        }
        let body: serde_json::Value = response.json().await?;

        for (handle, text) in parse_quotes(&body, &meta.artist_handle) {
            let inserted = sqlx::query!(
                r#"INSERT INTO margin_notes (rkey, handle, note, position)
                   SELECT $1, $2, $3, COALESCE(MAX(position) + 1, 0)
                   FROM margin_notes WHERE rkey = $1
                   HAVING count(*) < $4
                   ON CONFLICT (rkey, handle, note) DO NOTHING"#,
                specimen.rkey,
                handle,
                text,
                MAX_NOTES_PER_SPECIMEN,
            )
            .execute(pool)
            .await?;
            if inserted.rows_affected() > 0 {
                tracing::info!(rkey = %specimen.rkey, %handle, "new margin note collected");
                added += 1;
            }
        }
        // Gentle on the public API: ~4 requests/second across ~250 posts.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editorial_filter_keeps_commentary_and_drops_noise() {
        let artist = "all-paperclips.bsky.social";
        assert!(keep_note("fan.test", "Shoggoth found", artist));
        assert!(
            !keep_note(artist, "quoting myself", artist),
            "artist self-quotes"
        );
        assert!(
            !keep_note("fan.test", "bsky.app/profile/x", artist),
            "link-only"
        );
        assert!(
            !keep_note("fan.test", "https://elsewhere", artist),
            "url-only"
        );
        assert!(!keep_note("fan.test", "ok", artist), "too short");
    }

    #[test]
    fn quotes_parse_and_filter() {
        let response = serde_json::json!({
            "posts": [
                {"author": {"handle": "fan.test"}, "record": {"text": "cloud dragons"}},
                {"author": {"handle": "all-paperclips.bsky.social"}, "record": {"text": "self reply quote"}},
                {"author": {"handle": "linker.test"}, "record": {"text": "bsky.app/profile/nope"}}
            ]
        });
        let notes = parse_quotes(&response, "all-paperclips.bsky.social");
        assert_eq!(
            notes,
            vec![("fan.test".to_string(), "cloud dragons".to_string())]
        );
    }
}

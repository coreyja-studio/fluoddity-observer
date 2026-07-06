//! The suggestion box: the community tags specimens without leaving
//! Bluesky. #hashtags in replies to (and quote-posts of) a specimen's
//! original post are harvested daily as pending suggestions for the
//! curator's desk. The artist's own reply-hashtags skip the queue and
//! land directly — his word is the taxonomy.

use anyhow::Context;
use sqlx::PgPool;

const PUBLIC_API: &str = "https://public.api.bsky.app";

/// One community post that might carry tag suggestions.
#[derive(Debug, PartialEq)]
pub struct Harvested {
    pub author_did: String,
    pub author_handle: String,
    pub text: String,
    pub uri: String,
}

fn harvested_from_post(post: &serde_json::Value) -> Option<Harvested> {
    let author = post.get("author")?;
    Some(Harvested {
        author_did: author.get("did")?.as_str()?.to_string(),
        author_handle: author
            .get("handle")
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string(),
        text: post.get("record")?.get("text")?.as_str()?.to_string(),
        uri: post.get("uri")?.as_str()?.to_string(),
    })
}

/// Every reply post under a getPostThread response, any depth. The root
/// post itself is excluded — its hashtags are already ingest's job.
pub fn parse_thread_replies(thread: &serde_json::Value) -> Vec<Harvested> {
    fn walk(node: &serde_json::Value, out: &mut Vec<Harvested>, is_root: bool) {
        if !is_root
            && let Some(post) = node.get("post")
            && let Some(h) = harvested_from_post(post)
        {
            out.push(h);
        }
        if let Some(replies) = node.get("replies").and_then(|r| r.as_array()) {
            for reply in replies {
                walk(reply, out, false);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(root) = thread.get("thread") {
        walk(root, &mut out, true);
    }
    out
}

/// Quote-posts from a getQuotes response.
pub fn parse_quote_posts(response: &serde_json::Value) -> Vec<Harvested> {
    response
        .get("posts")
        .and_then(|p| p.as_array())
        .map(|posts| posts.iter().filter_map(harvested_from_post).collect())
        .unwrap_or_default()
}

pub struct HarvestStats {
    pub suggested: usize,
    pub artist_tagged: usize,
}

/// File one harvested post's hashtags for one specimen. Artist posts tag
/// directly; everyone else queues for the desk. Tags the specimen already
/// carries are skipped either way.
async fn file_post(
    pool: &PgPool,
    rkey: &str,
    artist_did: &str,
    via: &str,
    h: &Harvested,
    stats: &mut HarvestStats,
) -> anyhow::Result<()> {
    for tag in crate::catalog::extract_hashtags(&h.text) {
        let already = sqlx::query!(
            "SELECT 1 AS one FROM specimen_tags WHERE rkey = $1 AND tag = $2",
            rkey,
            tag,
        )
        .fetch_optional(pool)
        .await?
        .is_some();
        if already {
            continue;
        }
        if h.author_did == artist_did {
            sqlx::query!(
                "INSERT INTO specimen_tags (rkey, tag, kind, source, added_by)
                 VALUES ($1, $2, 'tag', 'post', 'artist-reply')
                 ON CONFLICT (rkey, tag) DO NOTHING",
                rkey,
                tag,
            )
            .execute(pool)
            .await?;
            tracing::info!(%rkey, %tag, "artist reply-tag applied directly");
            stats.artist_tagged += 1;
        } else {
            let inserted = sqlx::query!(
                "INSERT INTO tag_suggestions
                     (rkey, tag, suggester_did, suggester_handle, via, source_uri)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (rkey, tag, suggester_did) DO NOTHING",
                rkey,
                tag,
                h.author_did,
                h.author_handle,
                via,
                h.uri,
            )
            .execute(pool)
            .await?;
            if inserted.rows_affected() > 0 {
                tracing::info!(%rkey, %tag, suggester = %h.author_handle, %via, "tag suggestion filed");
                stats.suggested += 1;
            }
        }
    }
    Ok(())
}

/// Sweep the archive: harvest reply and quote hashtags for every specimen.
pub async fn harvest_once(pool: &PgPool, client: &reqwest::Client) -> anyhow::Result<HarvestStats> {
    let meta = sqlx::query!("SELECT artist_did FROM gallery_meta")
        .fetch_one(pool)
        .await
        .context("gallery_meta is empty — run import first")?;

    let specimens = sqlx::query!("SELECT rkey FROM specimens ORDER BY collected_on, rkey")
        .fetch_all(pool)
        .await?;

    let mut stats = HarvestStats {
        suggested: 0,
        artist_tagged: 0,
    };
    for specimen in &specimens {
        let uri = format!(
            "at://{}/app.bsky.feed.post/{}",
            meta.artist_did, specimen.rkey
        );

        let thread = client
            .get(format!("{PUBLIC_API}/xrpc/app.bsky.feed.getPostThread"))
            .query(&[("uri", uri.as_str()), ("depth", "10")])
            .send()
            .await?;
        if thread.status().is_success() {
            let body: serde_json::Value = thread.json().await?;
            for h in parse_thread_replies(&body) {
                file_post(
                    pool,
                    &specimen.rkey,
                    &meta.artist_did,
                    "reply",
                    &h,
                    &mut stats,
                )
                .await?;
            }
        } else {
            tracing::warn!(rkey = %specimen.rkey, status = %thread.status(), "getPostThread failed; skipping");
        }

        let quotes = client
            .get(format!("{PUBLIC_API}/xrpc/app.bsky.feed.getQuotes"))
            .query(&[("uri", uri.as_str()), ("limit", "25")])
            .send()
            .await?;
        if quotes.status().is_success() {
            let body: serde_json::Value = quotes.json().await?;
            for h in parse_quote_posts(&body) {
                file_post(
                    pool,
                    &specimen.rkey,
                    &meta.artist_did,
                    "quote",
                    &h,
                    &mut stats,
                )
                .await?;
            }
        } else {
            tracing::warn!(rkey = %specimen.rkey, status = %quotes.status(), "getQuotes failed; skipping");
        }

        // Gentle on the public API, same pacing as the margin-note sweep.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Ok(stats)
}

/// One pending row for the curator's desk.
pub struct PendingSuggestion {
    pub suggestion_id: i64,
    pub rkey: String,
    pub tag: String,
    pub suggester_handle: String,
    pub suggester_did: String,
    pub via: String,
}

pub async fn pending(pool: &PgPool) -> anyhow::Result<Vec<PendingSuggestion>> {
    Ok(sqlx::query_as!(
        PendingSuggestion,
        r#"SELECT suggestion_id, rkey, tag, suggester_handle, suggester_did, via
           FROM tag_suggestions WHERE status = 'pending' ORDER BY created_at"#
    )
    .fetch_all(pool)
    .await?)
}

/// Approve or decline one suggestion. Approval places the tag with
/// community provenance — the suggester's handle goes on the wall label.
pub async fn resolve(
    pool: &PgPool,
    suggestion_id: i64,
    approve: bool,
    curator_did: &str,
) -> anyhow::Result<Option<String>> {
    let mut tx = pool.begin().await?;
    let Some(row) = sqlx::query!(
        "UPDATE tag_suggestions
         SET status = $2, resolved_at = now(), resolved_by = $3
         WHERE suggestion_id = $1 AND status = 'pending'
         RETURNING rkey, tag, suggester_did, suggester_handle",
        suggestion_id,
        if approve { "approved" } else { "declined" },
        curator_did,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    if approve {
        let credit = if row.suggester_handle.is_empty() {
            row.suggester_did.clone()
        } else {
            row.suggester_handle.clone()
        };
        sqlx::query!(
            "INSERT INTO specimen_tags (rkey, tag, kind, source, added_by)
             VALUES ($1, $2, 'tag', 'community', $3)
             ON CONFLICT (rkey, tag) DO NOTHING",
            row.rkey,
            row.tag,
            credit,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some(row.rkey))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(did: &str, handle: &str, text: &str, rkey: &str) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("at://{did}/app.bsky.feed.post/{rkey}"),
            "author": {"did": did, "handle": handle},
            "record": {"text": text},
        })
    }

    #[test]
    fn thread_replies_walk_all_depths_and_skip_the_root() {
        let thread = serde_json::json!({
            "thread": {
                "post": post("did:plc:artist", "artist.test", "the original #root", "3mroot"),
                "replies": [
                    {
                        "post": post("did:plc:fan", "fan.test", "#jellyfish wow", "3mr1"),
                        "replies": [
                            {"post": post("did:plc:artist", "artist.test", "#flatland actually", "3mr2")}
                        ]
                    },
                    {"post": post("did:plc:other", "other.test", "no tags here", "3mr3")}
                ]
            }
        });
        let found = parse_thread_replies(&thread);
        assert_eq!(found.len(), 3, "root excluded, nested reply included");
        assert_eq!(found[0].author_handle, "fan.test");
        assert_eq!(found[1].text, "#flatland actually");
    }

    #[test]
    fn quote_posts_parse() {
        let response = serde_json::json!({
            "posts": [post("did:plc:fan", "fan.test", "this one is #livingmetal", "3mq1")]
        });
        let found = parse_quote_posts(&response);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].uri, "at://did:plc:fan/app.bsky.feed.post/3mq1");
        assert!(parse_quote_posts(&serde_json::json!({})).is_empty());
    }

    #[sqlx::test]
    async fn harvest_files_queue_and_artist_tags_directly(pool: PgPool) {
        sqlx::query!(
            "INSERT INTO gallery_meta
                 (only_row, artist_handle, artist_did, artist_name,
                  origin_handle, origin_text, origin_url)
             VALUES (TRUE, 'artist.test', 'did:plc:artist', 'A', 'w', 'wish', 'u')"
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO specimens (rkey, cid, caption, collected_on, url)
             VALUES ('3ma', 'bafy-a', 'Jellyfish! #existing', '2026-06-04', 'https://x')"
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO specimen_tags (rkey, tag, kind, source) VALUES ('3ma', 'existing', 'tag', 'post')"
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut stats = HarvestStats {
            suggested: 0,
            artist_tagged: 0,
        };
        // A fan suggests two tags, one of which the specimen already has.
        let fan = Harvested {
            author_did: "did:plc:fan".into(),
            author_handle: "fan.test".into(),
            text: "#existing #medusae".into(),
            uri: "at://did:plc:fan/app.bsky.feed.post/3mf".into(),
        };
        file_post(&pool, "3ma", "did:plc:artist", "reply", &fan, &mut stats)
            .await
            .unwrap();
        // The artist replies with a tag of his own.
        let artist = Harvested {
            author_did: "did:plc:artist".into(),
            author_handle: "artist.test".into(),
            text: "these are #softbodies".into(),
            uri: "at://did:plc:artist/app.bsky.feed.post/3mr".into(),
        };
        file_post(&pool, "3ma", "did:plc:artist", "reply", &artist, &mut stats)
            .await
            .unwrap();
        // Rerun: nothing new files twice.
        file_post(&pool, "3ma", "did:plc:artist", "reply", &fan, &mut stats)
            .await
            .unwrap();

        assert_eq!(stats.suggested, 1, "existing tag skipped, rerun deduped");
        assert_eq!(stats.artist_tagged, 1);

        let queue = pending(&pool).await.unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].tag, "medusae");
        assert_eq!(queue[0].via, "reply");

        let direct = sqlx::query!(
            "SELECT source, added_by FROM specimen_tags WHERE rkey = '3ma' AND tag = 'softbodies'"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(direct.source, "post");
        assert_eq!(direct.added_by, "artist-reply");
    }

    #[sqlx::test]
    async fn approve_places_tag_with_community_credit(pool: PgPool) {
        sqlx::query!(
            "INSERT INTO specimens (rkey, cid, caption, collected_on, url)
             VALUES ('3ma', 'bafy-a', 'Jellyfish!', '2026-06-04', 'https://x')"
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO tag_suggestions (rkey, tag, suggester_did, suggester_handle)
             VALUES ('3ma', 'medusae', 'did:plc:fan', 'fan.test'),
                    ('3ma', 'noise', 'did:plc:spam', 'spam.test')"
        )
        .execute(&pool)
        .await
        .unwrap();
        let ids: Vec<i64> =
            sqlx::query!("SELECT suggestion_id, tag FROM tag_suggestions ORDER BY suggestion_id")
                .fetch_all(&pool)
                .await
                .unwrap()
                .iter()
                .map(|r| r.suggestion_id)
                .collect();

        let rkey = resolve(&pool, ids[0], true, "did:plc:curator")
            .await
            .unwrap();
        assert_eq!(rkey.as_deref(), Some("3ma"));
        resolve(&pool, ids[1], false, "did:plc:curator")
            .await
            .unwrap();
        // Resolving twice is a no-op.
        assert!(
            resolve(&pool, ids[0], false, "did:plc:curator")
                .await
                .unwrap()
                .is_none()
        );

        let tag = sqlx::query!(
            "SELECT source, added_by FROM specimen_tags WHERE rkey = '3ma' AND tag = 'medusae'"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tag.source, "community");
        assert_eq!(tag.added_by, "fan.test");

        assert!(pending(&pool).await.unwrap().is_empty());
        let declined =
            sqlx::query!("SELECT count(*) AS \"n!\" FROM specimen_tags WHERE tag = 'noise'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(declined.n, 0, "declined suggestions place no tag");
    }
}

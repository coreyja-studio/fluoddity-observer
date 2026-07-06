//! Live ingest: poll the artist's Bluesky feed for new media posts (videos
//! and still images) and add them to the archive. Freshly discovered
//! specimens have no local file, so the site serves them straight from the
//! Bluesky CDNs — visible on the next page load, no restart, no downloads.

use anyhow::Context;
use sqlx::PgPool;

const PUBLIC_API: &str = "https://public.api.bsky.app";

/// A media post discovered in the artist's feed.
#[derive(Debug, PartialEq)]
pub struct Discovered {
    pub rkey: String,
    /// The video blob cid, or the first image's cid.
    pub cid: String,
    /// "video" or "image", matching specimens.kind.
    pub kind: &'static str,
    /// (cid, alt) per image, in post order; empty for videos.
    pub images: Vec<(String, String)>,
    pub caption: String,
    /// ISO date (YYYY-MM-DD).
    pub collected_on: String,
    pub url: String,
}

/// Extract the artist's own media posts from a getAuthorFeed response.
/// Skips reposts (feed items with a `reason`) and anything not authored by
/// the artist or without a video/image embed in the record.
pub fn parse_feed(
    feed: &serde_json::Value,
    artist_did: &str,
    artist_handle: &str,
) -> Vec<Discovered> {
    let Some(items) = feed.get("feed").and_then(|f| f.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| item.get("reason").is_none())
        .filter_map(|item| {
            let post = item.get("post")?;
            if post.get("author")?.get("did")?.as_str()? != artist_did {
                return None;
            }
            let record = post.get("record")?;
            let embed = record.get("embed")?;
            let (kind, cid, images) = match embed.get("$type")?.as_str()? {
                "app.bsky.embed.video" => {
                    let cid = embed.get("video")?.get("ref")?.get("$link")?.as_str()?;
                    ("video", cid.to_string(), Vec::new())
                }
                "app.bsky.embed.images" => {
                    let images: Vec<(String, String)> = embed
                        .get("images")?
                        .as_array()?
                        .iter()
                        .filter_map(|img| {
                            let cid = img.get("image")?.get("ref")?.get("$link")?.as_str()?;
                            let alt = img.get("alt").and_then(|a| a.as_str()).unwrap_or("");
                            Some((cid.to_string(), alt.to_string()))
                        })
                        .collect();
                    let first = images.first()?.0.clone();
                    ("image", first, images)
                }
                _ => return None,
            };
            let rkey = post.get("uri")?.as_str()?.rsplit('/').next()?;
            let created_at = record.get("createdAt")?.as_str()?;
            if created_at.len() < 10 {
                return None;
            }
            Some(Discovered {
                rkey: rkey.to_string(),
                cid,
                kind,
                images,
                caption: record
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                collected_on: created_at[..10].to_string(),
                url: format!("https://bsky.app/profile/{artist_handle}/post/{rkey}"),
            })
        })
        .collect()
}

/// Fetch the artist's latest media posts and insert any the archive hasn't
/// seen. Returns the rkeys of newly added specimens.
pub async fn poll_once(pool: &PgPool, client: &reqwest::Client) -> anyhow::Result<Vec<String>> {
    let meta = sqlx::query!("SELECT artist_did, artist_handle FROM gallery_meta")
        .fetch_one(pool)
        .await
        .context("gallery_meta is empty — run import first")?;

    let feed: serde_json::Value = client
        .get(format!("{PUBLIC_API}/xrpc/app.bsky.feed.getAuthorFeed"))
        .query(&[
            ("actor", meta.artist_did.as_str()),
            ("filter", "posts_with_media"),
            ("limit", "50"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let discovered = parse_feed(&feed, &meta.artist_did, &meta.artist_handle);
    let mut added = Vec::new();
    for d in &discovered {
        let collected_on = chrono::NaiveDate::parse_from_str(&d.collected_on, "%Y-%m-%d")
            .with_context(|| format!("bad createdAt on {}", d.rkey))?;
        let inserted = sqlx::query!(
            "INSERT INTO specimens (rkey, cid, kind, caption, collected_on, url)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (rkey) DO NOTHING",
            d.rkey,
            d.cid,
            d.kind,
            d.caption,
            collected_on,
            d.url,
        )
        .execute(pool)
        .await?;
        if inserted.rows_affected() > 0 {
            for (i, (cid, alt)) in d.images.iter().enumerate() {
                sqlx::query!(
                    "INSERT INTO specimen_images (rkey, position, cid, alt)
                     VALUES ($1, $2, $3, $4)
                     ON CONFLICT (rkey, position) DO NOTHING",
                    d.rkey,
                    i as i32,
                    cid,
                    alt,
                )
                .execute(pool)
                .await?;
            }
            for tag in crate::catalog::extract_hashtags(&d.caption) {
                sqlx::query!(
                    "INSERT INTO specimen_tags (rkey, tag, kind, source)
                     VALUES ($1, $2, 'tag', 'post')
                     ON CONFLICT (rkey, tag) DO NOTHING",
                    d.rkey,
                    tag,
                )
                .execute(pool)
                .await?;
            }
            added.push(d.rkey.clone());
        }
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTIST_DID: &str = "did:plc:rvkbczphgh3krtnopp2oobty";
    const ARTIST_HANDLE: &str = "all-paperclips.bsky.social";

    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!("../fixtures/author_feed.json")).unwrap()
    }

    #[test]
    fn parses_video_posts_from_author_feed() {
        let found = parse_feed(&fixture(), ARTIST_DID, ARTIST_HANDLE);
        assert_eq!(found.len(), 4);
        let first = &found[0];
        assert_eq!(first.rkey, "3mpkvz63rxs2g");
        assert_eq!(
            first.cid,
            "bafkreigewcyr7uxk7wlsabwu6qkognyvzn4fj32nesli2zsgyjflvqirva"
        );
        assert_eq!(first.kind, "video");
        assert!(first.images.is_empty());
        assert_eq!(first.collected_on, "2026-07-01");
        assert!(first.caption.starts_with("The RTX 5060"));
        assert_eq!(
            first.url,
            "https://bsky.app/profile/all-paperclips.bsky.social/post/3mpkvz63rxs2g"
        );
    }

    #[test]
    fn parses_image_posts_from_author_feed() {
        let found = parse_feed(&fixture(), ARTIST_DID, ARTIST_HANDLE);
        let stills = found.iter().find(|d| d.kind == "image").unwrap();
        assert_eq!(stills.rkey, "3mimgtest2g");
        assert_eq!(
            stills.cid, "bafkreiimgone",
            "first image is the primary cid"
        );
        assert_eq!(
            stills.images,
            vec![
                ("bafkreiimgone".to_string(), "a koosh mid-bloom".to_string()),
                ("bafkreiimgtwo".to_string(), String::new()),
            ]
        );
        assert_eq!(stills.caption, "Two stills from the overnight run");
    }

    #[test]
    fn skips_posts_from_other_authors() {
        assert!(parse_feed(&fixture(), "did:plc:someone-else", ARTIST_HANDLE).is_empty());
    }

    #[test]
    fn skips_reposts() {
        let mut feed = fixture();
        for item in feed["feed"].as_array_mut().unwrap() {
            item["reason"] = serde_json::json!({"$type": "app.bsky.feed.defs#reasonRepost"});
        }
        assert!(parse_feed(&feed, ARTIST_DID, ARTIST_HANDLE).is_empty());
    }

    #[test]
    fn tolerates_malformed_feed() {
        assert!(parse_feed(&serde_json::json!({}), ARTIST_DID, ARTIST_HANDLE).is_empty());
        assert!(
            parse_feed(
                &serde_json::json!({"feed": [{}]}),
                ARTIST_DID,
                ARTIST_HANDLE
            )
            .is_empty()
        );
    }

    #[sqlx::test]
    async fn poll_inserts_only_new_specimens(pool: PgPool) {
        sqlx::query!(
            "INSERT INTO gallery_meta
                 (only_row, artist_handle, artist_did, artist_name,
                  origin_handle, origin_text, origin_url)
             VALUES (TRUE, $1, $2, 'Test', 'w', 'wish', 'u')",
            ARTIST_HANDLE,
            ARTIST_DID,
        )
        .execute(&pool)
        .await
        .unwrap();

        // Simulate the insert phase of poll_once directly from the fixture —
        // no network in tests.
        let discovered = parse_feed(&fixture(), ARTIST_DID, ARTIST_HANDLE);
        for d in &discovered {
            let collected_on =
                chrono::NaiveDate::parse_from_str(&d.collected_on, "%Y-%m-%d").unwrap();
            sqlx::query!(
                "INSERT INTO specimens (rkey, cid, kind, caption, collected_on, url)
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (rkey) DO NOTHING",
                d.rkey,
                d.cid,
                d.kind,
                d.caption,
                collected_on,
                d.url,
            )
            .execute(&pool)
            .await
            .unwrap();
            for (i, (cid, alt)) in d.images.iter().enumerate() {
                sqlx::query!(
                    "INSERT INTO specimen_images (rkey, position, cid, alt)
                     VALUES ($1, $2, $3, $4) ON CONFLICT (rkey, position) DO NOTHING",
                    d.rkey,
                    i as i32,
                    cid,
                    alt,
                )
                .execute(&pool)
                .await
                .unwrap();
            }
        }

        let catalog = crate::db::load_catalog(&pool).await.unwrap();
        assert_eq!(catalog.archive.len(), 4);
        let fresh = catalog.archive.get("3mpkvz63rxs2g").unwrap();
        assert!(
            fresh.file.is_none(),
            "ingested specimens have no local file"
        );
        let stills = catalog.archive.get("3mimgtest2g").unwrap();
        assert_eq!(stills.kind, crate::catalog::MediaKind::Image);
        assert_eq!(stills.images.len(), 2);
    }
}

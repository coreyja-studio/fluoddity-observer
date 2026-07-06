//! Cold storage: pull original-quality blobs for specimens the local media
//! archive doesn't hold yet. Live-ingested specimens serve from the Bluesky
//! CDN, which is fine until it isn't — the whole premise of the guide is
//! that timelines scroll away and archives don't.
//!
//! Runs wherever the media dir lives (not on Fly — no volume there):
//! `paperclips-gallery pull-media` with DATABASE_URL pointed at the
//! database whose ledger should record the pulled files. Blobs come from
//! the artist's own PDS via com.atproto.sync.getBlob — the original bytes,
//! not the CDN's re-encode.

use anyhow::Context;
use futures_util::StreamExt;
use sqlx::PgPool;
use tokio::io::AsyncWriteExt;

/// Refuse blobs larger than this — the loops are a few MB each; anything
/// past this is a mistake, not a specimen.
const MAX_BLOB_BYTES: u64 = 256 * 1024 * 1024;

pub struct PullStats {
    pub pulled: usize,
    pub failed: usize,
}

/// Resolve a DID to its PDS endpoint via plc.directory. Never assume
/// bsky.social — the artist can move hosts without telling us.
pub async fn resolve_pds(client: &reqwest::Client, did: &str) -> anyhow::Result<String> {
    let doc: serde_json::Value = client
        .get(format!("https://plc.directory/{did}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    pds_from_did_doc(&doc).context("DID document has no atproto_pds service")
}

fn pds_from_did_doc(doc: &serde_json::Value) -> Option<String> {
    doc.get("service")?
        .as_array()?
        .iter()
        .find(|s| {
            s.get("id").and_then(|i| i.as_str()) == Some("#atproto_pds")
                || s.get("type").and_then(|t| t.as_str()) == Some("AtprotoPersonalDataServer")
        })?
        .get("serviceEndpoint")?
        .as_str()
        .map(str::to_string)
}

/// Extension for a blob's Content-Type; the archive's ledger keeps real
/// file types, not whatever the URL claimed.
fn extension_for(content_type: &str) -> &'static str {
    match content_type.split(';').next().unwrap_or("").trim() {
        "video/mp4" => "mp4",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}

/// Archive-relative path for a pulled blob. The full cid goes in the name:
/// this archive has already seen two blobs share an 8-char prefix.
fn blob_path(kind: &str, rkey: &str, cid: &str, ext: &str) -> String {
    let dir = if kind == "video" { "videos" } else { "images" };
    format!("{dir}/{rkey}_{cid}.{ext}")
}

/// Download one blob to the media dir (temp file + rename, so a dropped
/// connection never leaves a truncated specimen). Returns the
/// archive-relative path.
async fn pull_blob(
    client: &reqwest::Client,
    pds: &str,
    did: &str,
    media_dir: &str,
    kind: &str,
    rkey: &str,
    cid: &str,
) -> anyhow::Result<String> {
    let response = client
        .get(format!("{pds}/xrpc/com.atproto.sync.getBlob"))
        .query(&[("did", did), ("cid", cid)])
        .send()
        .await?
        .error_for_status()?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let rel = blob_path(kind, rkey, cid, extension_for(&content_type));
    let dest = std::path::Path::new(media_dir).join(&rel);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp = dest.with_extension("part");
    let mut out = tokio::fs::File::create(&tmp).await?;
    let mut written: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        written += chunk.len() as u64;
        if written > MAX_BLOB_BYTES {
            drop(out);
            tokio::fs::remove_file(&tmp).await.ok();
            anyhow::bail!("blob {cid} exceeds {MAX_BLOB_BYTES} bytes");
        }
        out.write_all(&chunk).await?;
    }
    out.flush().await?;
    drop(out);
    tokio::fs::rename(&tmp, &dest).await?;
    Ok(rel)
}

/// Pull every blob the ledger says is missing locally: video specimens and
/// individual images with no `file`. Records each pulled path back onto
/// the row, so a rerun only fetches what's still missing.
pub async fn pull_media(
    pool: &PgPool,
    client: &reqwest::Client,
    media_dir: &str,
) -> anyhow::Result<PullStats> {
    let meta = sqlx::query!("SELECT artist_did FROM gallery_meta")
        .fetch_one(pool)
        .await
        .context("gallery_meta is empty — run import first")?;
    let pds = resolve_pds(client, &meta.artist_did).await?;
    tracing::info!(%pds, "resolved the artist's PDS");

    let mut stats = PullStats {
        pulled: 0,
        failed: 0,
    };

    let videos = sqlx::query!(
        "SELECT rkey, cid FROM specimens
         WHERE kind = 'video' AND file IS NULL ORDER BY collected_on, rkey"
    )
    .fetch_all(pool)
    .await?;
    for row in &videos {
        match pull_blob(
            client,
            &pds,
            &meta.artist_did,
            media_dir,
            "video",
            &row.rkey,
            &row.cid,
        )
        .await
        {
            Ok(rel) => {
                sqlx::query!(
                    "UPDATE specimens SET file = $1 WHERE rkey = $2",
                    rel,
                    row.rkey
                )
                .execute(pool)
                .await?;
                tracing::info!(rkey = %row.rkey, file = %rel, "pulled video blob");
                stats.pulled += 1;
            }
            Err(err) => {
                tracing::warn!(?err, rkey = %row.rkey, "video blob pull failed");
                stats.failed += 1;
            }
        }
    }

    let images = sqlx::query!(
        "SELECT rkey, position, cid FROM specimen_images
         WHERE file IS NULL ORDER BY rkey, position"
    )
    .fetch_all(pool)
    .await?;
    for row in &images {
        match pull_blob(
            client,
            &pds,
            &meta.artist_did,
            media_dir,
            "image",
            &row.rkey,
            &row.cid,
        )
        .await
        {
            Ok(rel) => {
                sqlx::query!(
                    "UPDATE specimen_images SET file = $1 WHERE rkey = $2 AND position = $3",
                    rel,
                    row.rkey,
                    row.position
                )
                .execute(pool)
                .await?;
                // The first image's file is mirrored onto the specimen,
                // matching what import records.
                if row.position == 0 {
                    sqlx::query!(
                        "UPDATE specimens SET file = $1 WHERE rkey = $2 AND file IS NULL",
                        rel,
                        row.rkey
                    )
                    .execute(pool)
                    .await?;
                }
                tracing::info!(rkey = %row.rkey, file = %rel, "pulled image blob");
                stats.pulled += 1;
            }
            Err(err) => {
                tracing::warn!(?err, rkey = %row.rkey, "image blob pull failed");
                stats.failed += 1;
            }
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pds_resolves_from_did_doc() {
        let doc = serde_json::json!({
            "service": [
                {"id": "#other", "type": "SomethingElse", "serviceEndpoint": "https://nope.example"},
                {"id": "#atproto_pds", "type": "AtprotoPersonalDataServer",
                 "serviceEndpoint": "https://pds.example.host"}
            ]
        });
        assert_eq!(
            pds_from_did_doc(&doc).as_deref(),
            Some("https://pds.example.host")
        );
        assert_eq!(pds_from_did_doc(&serde_json::json!({})), None);
    }

    #[test]
    fn blob_paths_keep_the_full_cid() {
        assert_eq!(
            blob_path("video", "3mabc", "bafkreiverylongcid", "mp4"),
            "videos/3mabc_bafkreiverylongcid.mp4"
        );
        assert_eq!(
            blob_path("image", "3mabc", "bafkreiother", "jpg"),
            "images/3mabc_bafkreiother.jpg"
        );
    }

    #[test]
    fn extensions_follow_content_type() {
        assert_eq!(extension_for("video/mp4"), "mp4");
        assert_eq!(extension_for("image/jpeg; charset=binary"), "jpg");
        assert_eq!(extension_for("application/octet-stream"), "bin");
    }
}

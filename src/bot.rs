//! The gallery's Bluesky presence.
//!
//! Mention the gallery account anywhere in a thread and the bot replies with
//! that thread's live room link — the zero-friction path from "I made a
//! curation thread" to "here's my room."
//!
//! Transport: the bot polls its own notifications (app.bsky.notification.
//! listNotifications) rather than tailing Jetstream. Jetstream can only
//! filter by author DID, so catching mentions would mean consuming the whole
//! network's post stream; notifications deliver exactly our mentions and
//! queue up across downtime. A Jetstream listener can slot in later if we
//! ever need to react to posts that don't mention us.

use anyhow::Context;
use sqlx::PgPool;

use crate::threads::{ThreadFetcher, ThreadRoom};

/// Authenticated session with the bot account's PDS.
struct Session {
    access_jwt: String,
    did: String,
}

pub struct BotConfig {
    pub handle: String,
    pub password: String,
    pub pds: String,
    pub public_url: String,
}

impl BotConfig {
    /// Read from PCG_BOT_* env; None (bot disabled) unless handle+password
    /// are both present.
    pub fn from_env() -> Option<Self> {
        let handle = std::env::var("PCG_BOT_HANDLE").ok()?;
        let password = std::env::var("PCG_BOT_PASSWORD").ok()?;
        Some(Self {
            handle,
            password,
            pds: std::env::var("PCG_BOT_PDS").unwrap_or_else(|_| "https://bsky.social".to_string()),
            public_url: public_url(),
        })
    }
}

pub fn public_url() -> String {
    std::env::var("PCG_PUBLIC_URL").unwrap_or_else(|_| "http://127.0.0.1:4601".to_string())
}

async fn login(client: &reqwest::Client, cfg: &BotConfig) -> anyhow::Result<Session> {
    let body: serde_json::Value = client
        .post(format!("{}/xrpc/com.atproto.server.createSession", cfg.pds))
        .json(&serde_json::json!({
            "identifier": cfg.handle,
            "password": cfg.password,
        }))
        .send()
        .await?
        .error_for_status()
        .context("bot login failed — check PCG_BOT_HANDLE/PCG_BOT_PASSWORD")?
        .json()
        .await?;
    Ok(Session {
        access_jwt: body
            .get("accessJwt")
            .and_then(|t| t.as_str())
            .context("createSession response missing accessJwt")?
            .to_string(),
        did: body
            .get("did")
            .and_then(|d| d.as_str())
            .context("createSession response missing did")?
            .to_string(),
    })
}

/// A mention worth answering: where it happened and the thread it lives in.
#[derive(Debug, PartialEq)]
pub struct Ask {
    pub mention_uri: String,
    pub mention_cid: String,
    /// Thread root (the room head): uri + cid.
    pub root_uri: String,
    pub root_cid: String,
}

/// Pull answerable mentions out of a listNotifications response.
pub fn parse_mentions(notifications: &serde_json::Value) -> Vec<Ask> {
    let Some(items) = notifications
        .get("notifications")
        .and_then(|n| n.as_array())
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|n| n.get("reason").and_then(|r| r.as_str()) == Some("mention"))
        .filter_map(|n| {
            let uri = n.get("uri")?.as_str()?.to_string();
            let cid = n.get("cid")?.as_str()?.to_string();
            let record = n.get("record")?;
            let (root_uri, root_cid) = match record
                .get("reply")
                .and_then(|r| r.get("root"))
                .and_then(|root| {
                    Some((
                        root.get("uri")?.as_str()?.to_string(),
                        root.get("cid")?.as_str()?.to_string(),
                    ))
                }) {
                Some(root) => root,
                // A top-level mention IS its own thread root.
                None => (uri.clone(), cid.clone()),
            };
            Some(Ask {
                mention_uri: uri,
                mention_cid: cid,
                root_uri,
                root_cid,
            })
        })
        .collect()
}

/// (author, rkey) from an at-uri, for any collection.
pub fn split_at_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("at://")?;
    let mut parts = rest.splitn(3, '/');
    let (did, _collection, rkey) = (parts.next()?, parts.next()?, parts.next()?);
    Some((did.to_string(), rkey.to_string()))
}

/// Reply text plus link facets (byte-offset based) for a room answer.
pub fn compose_reply(
    public_url: &str,
    room: &ThreadRoom,
    hung_count: usize,
) -> (String, serde_json::Value) {
    let room_url = format!("{public_url}/room/{}/{}", room.author_handle, room.rkey);
    let text = if hung_count > 0 {
        format!(
            "Your room is hung — {hung_count} specimen{} from the expedition record:\n\n{room_url}\n\nIt renders live from this thread. Add quote-posts and the room grows.",
            if hung_count == 1 { "" } else { "s" },
        )
    } else {
        format!(
            "This thread is a room waiting to happen — quote-post or link the artist's work here and the specimens will hang themselves:\n\n{room_url}",
        )
    };
    let start = text
        .find(&room_url)
        .expect("room url is always embedded in the reply text");
    let facets = serde_json::json!([{
        "index": {
            "byteStart": start,
            "byteEnd": start + room_url.len(),
        },
        "features": [{
            "$type": "app.bsky.richtext.facet#link",
            "uri": room_url,
        }],
    }]);
    (text, facets)
}

/// One notifications poll: answer new mentions, mark them replied.
/// Returns the number of replies posted.
pub async fn poll_once(
    pool: &PgPool,
    client: &reqwest::Client,
    threads: &ThreadFetcher,
    cfg: &BotConfig,
) -> anyhow::Result<usize> {
    let session = login(client, cfg).await?;

    let notifications: serde_json::Value = client
        .get(format!(
            "{}/xrpc/app.bsky.notification.listNotifications",
            cfg.pds
        ))
        .query(&[("limit", "50")])
        .bearer_auth(&session.access_jwt)
        .send()
        .await?
        .error_for_status()
        .context("listNotifications failed")?
        .json()
        .await?;

    let meta = sqlx::query!("SELECT artist_did, artist_handle FROM gallery_meta")
        .fetch_one(pool)
        .await
        .context("gallery_meta is empty — run import first")?;

    let mut replied = 0;
    for ask in parse_mentions(&notifications) {
        let seen = sqlx::query!(
            "SELECT mention_uri FROM bot_replies WHERE mention_uri = $1",
            ask.mention_uri
        )
        .fetch_optional(pool)
        .await?
        .is_some();
        if seen {
            continue;
        }

        let Some((root_author, root_rkey)) = split_at_uri(&ask.root_uri) else {
            continue;
        };
        let Some(room) = threads
            .fetch(
                &root_author,
                &root_rkey,
                &meta.artist_did,
                &meta.artist_handle,
            )
            .await?
        else {
            tracing::warn!(uri = %ask.root_uri, "mentioned thread did not resolve");
            continue;
        };

        // Count entries that actually exist in the archive.
        let rkeys: Vec<String> = room
            .entries
            .iter()
            .map(|e| e.specimen_rkey.clone())
            .collect();
        let hung_count = sqlx::query!(
            "SELECT count(*) AS \"count!\" FROM specimens WHERE rkey = ANY($1)",
            &rkeys,
        )
        .fetch_one(pool)
        .await?
        .count as usize;

        let (text, facets) = compose_reply(&cfg.public_url, &room, hung_count);
        let record = serde_json::json!({
            "$type": "app.bsky.feed.post",
            "text": text,
            "facets": facets,
            "createdAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "reply": {
                "root": {"uri": ask.root_uri, "cid": ask.root_cid},
                "parent": {"uri": ask.mention_uri, "cid": ask.mention_cid},
            },
        });
        let created: serde_json::Value = client
            .post(format!("{}/xrpc/com.atproto.repo.createRecord", cfg.pds))
            .bearer_auth(&session.access_jwt)
            .json(&serde_json::json!({
                "repo": session.did,
                "collection": "app.bsky.feed.post",
                "record": record,
            }))
            .send()
            .await?
            .error_for_status()
            .context("posting reply failed")?
            .json()
            .await?;
        let reply_uri = created
            .get("uri")
            .and_then(|u| u.as_str())
            .unwrap_or_default();

        sqlx::query!(
            "INSERT INTO bot_replies (mention_uri, reply_uri) VALUES ($1, $2)
             ON CONFLICT (mention_uri) DO NOTHING",
            ask.mention_uri,
            reply_uri,
        )
        .execute(pool)
        .await?;
        tracing::info!(mention = %ask.mention_uri, reply = %reply_uri, "answered a room ask");
        replied += 1;
    }

    // Clear the notification badge for everything we just walked.
    let _ = client
        .post(format!("{}/xrpc/app.bsky.notification.updateSeen", cfg.pds))
        .bearer_auth(&session.access_jwt)
        .json(&serde_json::json!({
            "seenAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }))
        .send()
        .await;

    Ok(replied)
}

/// Background poll loop, spawned alongside the web server when the bot is
/// configured.
pub async fn run(pool: PgPool, threads: std::sync::Arc<ThreadFetcher>, cfg: BotConfig) {
    let client = match reqwest::Client::builder()
        .user_agent("paperclips-gallery-bot/0.1 (fluoddity field guide)")
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(?err, "bot disabled: failed to build http client");
            return;
        }
    };
    let interval_secs: u64 = std::env::var("PCG_BOT_POLL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match poll_once(&pool, &client, &threads, &cfg).await {
            Ok(0) => tracing::debug!("bot: no new asks"),
            Ok(n) => tracing::info!(replies = n, "bot: answered asks"),
            Err(err) => tracing::warn!(?err, "bot poll failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifications_fixture() -> serde_json::Value {
        serde_json::json!({
            "notifications": [
                {
                    "uri": "at://did:plc:guest/app.bsky.feed.post/3mention",
                    "cid": "bafy-mention",
                    "reason": "mention",
                    "record": {
                        "text": "@gallery hang this!",
                        "reply": {
                            "root": {"uri": "at://did:plc:guest/app.bsky.feed.post/3root", "cid": "bafy-root"},
                            "parent": {"uri": "at://did:plc:guest/app.bsky.feed.post/3parent", "cid": "bafy-parent"}
                        }
                    }
                },
                {
                    "uri": "at://did:plc:guest/app.bsky.feed.post/3toplevel",
                    "cid": "bafy-top",
                    "reason": "mention",
                    "record": {"text": "@gallery look at my new thread"}
                },
                {
                    "uri": "at://did:plc:other/app.bsky.feed.post/3like",
                    "cid": "bafy-like",
                    "reason": "like",
                    "record": {}
                }
            ]
        })
    }

    #[test]
    fn mentions_parse_with_thread_roots() {
        let asks = parse_mentions(&notifications_fixture());
        assert_eq!(asks.len(), 2, "likes are ignored");
        // Reply inside a thread points at the thread root.
        assert_eq!(
            asks[0].root_uri,
            "at://did:plc:guest/app.bsky.feed.post/3root"
        );
        assert_eq!(asks[0].root_cid, "bafy-root");
        // A top-level mention is its own root.
        assert_eq!(asks[1].root_uri, asks[1].mention_uri);
        assert_eq!(asks[1].root_cid, "bafy-top");
    }

    #[test]
    fn at_uris_split() {
        assert_eq!(
            split_at_uri("at://did:plc:abc/app.bsky.feed.post/3xyz"),
            Some(("did:plc:abc".to_string(), "3xyz".to_string()))
        );
        assert_eq!(split_at_uri("https://nope"), None);
    }

    #[test]
    fn replies_embed_room_link_with_correct_facet_bytes() {
        let room = ThreadRoom {
            author_did: "did:plc:guest".into(),
            author_handle: "guest.test".into(),
            author_display: "A Guest".into(),
            rkey: "3root".into(),
            title: "my favorites".into(),
            intro: "a tour".into(),
            entries: Vec::new(),
        };
        let (text, facets) = compose_reply("https://fluoddity.example", &room, 4);
        assert!(text.contains("4 specimens"));
        let url = "https://fluoddity.example/room/guest.test/3root";
        assert!(text.contains(url));
        let start = facets[0]["index"]["byteStart"].as_u64().unwrap() as usize;
        let end = facets[0]["index"]["byteEnd"].as_u64().unwrap() as usize;
        assert_eq!(&text.as_bytes()[start..end], url.as_bytes());

        let (empty_text, _) = compose_reply("https://fluoddity.example", &room, 0);
        assert!(empty_text.contains("waiting to happen"));
    }

    #[test]
    fn facet_bytes_survive_multibyte_text() {
        let room = ThreadRoom {
            author_did: "did:plc:guest".into(),
            author_handle: "gäst.test".into(),
            author_display: "Gäst".into(),
            rkey: "3root".into(),
            title: "…".into(),
            intro: String::new(),
            entries: Vec::new(),
        };
        let (text, facets) = compose_reply("https://fluoddity.example", &room, 1);
        let start = facets[0]["index"]["byteStart"].as_u64().unwrap() as usize;
        let end = facets[0]["index"]["byteEnd"].as_u64().unwrap() as usize;
        let url = format!(
            "https://fluoddity.example/room/{}/3root",
            room.author_handle
        );
        assert_eq!(&text.as_bytes()[start..end], url.as_bytes());
    }
}

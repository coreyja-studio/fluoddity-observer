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
    /// The mention post's text — commands and hashtags live here.
    pub text: String,
    pub author_did: String,
    pub author_handle: String,
    /// The post this mention replies to, when it is a reply.
    pub parent_uri: Option<String>,
}

/// What a mention is asking for.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// "!help" anywhere in the text.
    Help,
    /// Hashtags in a reply to one of the artist's posts: tag suggestions
    /// for that specimen.
    SuggestTags {
        specimen_rkey: String,
        tags: Vec<String>,
    },
    /// The default: answer with the thread's room link.
    RoomLink,
}

/// Classify a mention. Pure — the caller still checks the specimen exists.
pub fn classify_ask(ask: &Ask, artist_did: &str) -> Command {
    if ask.text.to_lowercase().contains("!help") {
        return Command::Help;
    }
    let tags = crate::catalog::extract_hashtags(&ask.text);
    if !tags.is_empty()
        && let Some(parent) = &ask.parent_uri
        && let Some((did, rkey)) = split_at_uri(parent)
        && did == artist_did
    {
        return Command::SuggestTags {
            specimen_rkey: rkey,
            tags,
        };
    }
    Command::RoomLink
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
            let author = n.get("author")?;
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
                text: record
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                author_did: author.get("did")?.as_str()?.to_string(),
                author_handle: author
                    .get("handle")
                    .and_then(|h| h.as_str())
                    .unwrap_or("")
                    .to_string(),
                parent_uri: record
                    .get("reply")
                    .and_then(|r| r.get("parent"))
                    .and_then(|p| p.get("uri"))
                    .and_then(|u| u.as_str())
                    .map(str::to_string),
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

/// The bot's self-introduction, for "!help".
pub fn compose_help(public_url: &str) -> String {
    format!(
        "I keep the field guide at {public_url}.\n\n\
         Mention me in a thread of the artist's work and I'll hang it as a room.\n\
         Reply to one of his posts with #hashtags (and mention me) and I'll file \
         them for the curator's desk.\n\n\
         That's all I know how to do — the rest is the survey's job."
    )
}

/// Acknowledgement for tag suggestions filed via mention.
pub fn compose_tag_ack(tags: &[String]) -> String {
    let listed = tags
        .iter()
        .map(|t| format!("#{t}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "Noted — {listed} filed for the curator's desk. \
         If the survey takes {} up, your name goes on the wall label.",
        if tags.len() == 1 { "it" } else { "them" },
    )
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

        let mut command = classify_ask(&ask, &meta.artist_did);
        // A tag ask only holds if the parent post is actually a specimen.
        if let Command::SuggestTags { specimen_rkey, .. } = &command {
            let exists = sqlx::query!("SELECT rkey FROM specimens WHERE rkey = $1", specimen_rkey)
                .fetch_optional(pool)
                .await?
                .is_some();
            if !exists {
                command = Command::RoomLink;
            }
        }

        let (text, facets) = match &command {
            Command::Help => (compose_help(&cfg.public_url), None),
            Command::SuggestTags {
                specimen_rkey,
                tags,
            } => {
                let harvested = crate::suggestions::Harvested {
                    author_did: ask.author_did.clone(),
                    author_handle: ask.author_handle.clone(),
                    text: ask.text.clone(),
                    uri: ask.mention_uri.clone(),
                };
                crate::suggestions::file_mention(pool, specimen_rkey, &meta.artist_did, &harvested)
                    .await?;
                (compose_tag_ack(tags), None)
            }
            Command::RoomLink => {
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

                // A thread with real specimens is homepage material — file
                // it for the curator's desk (idempotent; skips registered).
                if hung_count > 0 {
                    crate::suggestions::file_room(pool, &room, hung_count, &ask.mention_uri)
                        .await?;
                }

                let (text, facets) = compose_reply(&cfg.public_url, &room, hung_count);
                (text, Some(facets))
            }
        };

        let mut record = serde_json::json!({
            "$type": "app.bsky.feed.post",
            "text": text,
            "createdAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "reply": {
                "root": {"uri": ask.root_uri, "cid": ask.root_cid},
                "parent": {"uri": ask.mention_uri, "cid": ask.mention_cid},
            },
        });
        if let Some(facets) = facets {
            record["facets"] = facets;
        }
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
        tracing::info!(mention = %ask.mention_uri, reply = %reply_uri, ?command, "answered an ask");
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
                    "author": {"did": "did:plc:guest", "handle": "guest.test"},
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
                    "author": {"did": "did:plc:guest", "handle": "guest.test"},
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
        // Author and parent carry through for command classification.
        assert_eq!(asks[0].author_handle, "guest.test");
        assert_eq!(
            asks[0].parent_uri.as_deref(),
            Some("at://did:plc:guest/app.bsky.feed.post/3parent")
        );
        assert_eq!(asks[1].parent_uri, None);
    }

    fn ask(text: &str, parent_uri: Option<&str>) -> Ask {
        Ask {
            mention_uri: "at://did:plc:fan/app.bsky.feed.post/3m".into(),
            mention_cid: "bafy-m".into(),
            root_uri: "at://did:plc:artist/app.bsky.feed.post/3root".into(),
            root_cid: "bafy-root".into(),
            text: text.into(),
            author_did: "did:plc:fan".into(),
            author_handle: "fan.test".into(),
            parent_uri: parent_uri.map(str::to_string),
        }
    }

    #[test]
    fn asks_classify_by_text_and_parent() {
        const ARTIST: &str = "did:plc:artist";
        // !help anywhere wins, any case.
        assert_eq!(
            classify_ask(&ask("what do you do? !HELP", None), ARTIST),
            Command::Help
        );
        // Hashtags in a reply to an artist post file as suggestions.
        assert_eq!(
            classify_ask(
                &ask(
                    "@gallery #jellyfish #flatland",
                    Some("at://did:plc:artist/app.bsky.feed.post/3spec")
                ),
                ARTIST
            ),
            Command::SuggestTags {
                specimen_rkey: "3spec".into(),
                tags: vec!["jellyfish".into(), "flatland".into()],
            }
        );
        // Hashtags replying to a NON-artist post are not tag asks.
        assert_eq!(
            classify_ask(
                &ask(
                    "#jellyfish",
                    Some("at://did:plc:someone/app.bsky.feed.post/3x")
                ),
                ARTIST
            ),
            Command::RoomLink
        );
        // No hashtags, no !help: the classic room ask.
        assert_eq!(
            classify_ask(&ask("hang my thread please", None), ARTIST),
            Command::RoomLink
        );
    }

    #[test]
    fn help_and_tag_ack_read_right() {
        let help = compose_help("https://fluoddity.example");
        assert!(help.contains("https://fluoddity.example"));
        assert!(help.contains("curator's desk"));

        let one = compose_tag_ack(&["jellyfish".to_string()]);
        assert!(one.contains("#jellyfish"), "{one}");
        assert!(one.contains("takes it up"));
        let two = compose_tag_ack(&["a".to_string(), "b".to_string()]);
        assert!(two.contains("#a #b"));
        assert!(two.contains("takes them up"));
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

// ---- the weekly wrap-up ----
//
// Once a week (when a completed Monday–Sunday week hasn't been wrapped yet),
// the bot posts a short thread: the most-liked specimens collected that
// week, each entry quote-posting the original so the loop plays in-feed.
// Up to three; fewer on slow weeks; nothing at all when the artist was
// silent or nothing drew a like — the gallery must never read as a content
// quota.

const WEEKLY_MAX_PICKS: usize = 3;

/// A specimen that made the week's wrap-up.
#[derive(Debug)]
pub struct WeeklyPick {
    pub rkey: String,
    pub label: String,
    pub likes: i64,
    /// The original post, for the quote embed.
    pub uri: String,
    pub cid: String,
}

/// The last completed Monday..=Sunday week strictly before `today`.
pub fn last_completed_week(today: chrono::NaiveDate) -> (chrono::NaiveDate, chrono::NaiveDate) {
    use chrono::Datelike as _;
    let this_monday = today - chrono::Days::new(today.weekday().num_days_from_monday() as u64);
    let week_start = this_monday - chrono::Days::new(7);
    (week_start, week_start + chrono::Days::new(6))
}

/// Rank a week's specimens: most liked first, ties to the newer post; keep
/// only liked ones; at most WEEKLY_MAX_PICKS.
pub fn select_picks(mut candidates: Vec<WeeklyPick>) -> Vec<WeeklyPick> {
    candidates.retain(|c| c.likes > 0);
    candidates.sort_by(|a, b| b.likes.cmp(&a.likes).then(b.rkey.cmp(&a.rkey)));
    candidates.truncate(WEEKLY_MAX_PICKS);
    candidates
}

/// One wrap-up thread entry: text, link facets, and the quoted post ref.
pub struct WeeklyEntry {
    pub text: String,
    pub facets: serde_json::Value,
    pub quoted_uri: String,
    pub quoted_cid: String,
}

/// Compose the wrap-up thread: root text, then one entry per pick.
pub fn compose_weekly(
    public_url: &str,
    total_collected: usize,
    picks: &[WeeklyPick],
) -> (String, Vec<WeeklyEntry>) {
    let archive_url = format!("{public_url}/archive");
    let root = format!(
        "The week in Fluoddity — {total_collected} new specimen{} joined the expedition record. The most beheld:\n\n{archive_url}",
        if total_collected == 1 { "" } else { "s" },
    );
    let entries = picks
        .iter()
        .enumerate()
        .map(|(i, pick)| {
            let page_url = format!("{public_url}/specimen/{}", pick.rkey);
            let text = format!(
                "{}. “{}” — {} like{}\n\n{page_url}",
                i + 1,
                pick.label,
                pick.likes,
                if pick.likes == 1 { "" } else { "s" },
            );
            let start = text.find(&page_url).expect("page url embedded in entry");
            let facets = serde_json::json!([{
                "index": {"byteStart": start, "byteEnd": start + page_url.len()},
                "features": [{"$type": "app.bsky.richtext.facet#link", "uri": page_url}],
            }]);
            WeeklyEntry {
                text,
                facets,
                quoted_uri: pick.uri.clone(),
                quoted_cid: pick.cid.clone(),
            }
        })
        .collect();
    (root, entries)
}

/// Post the wrap-up for the last completed week if it's due. Returns the
/// number of picks posted, or None when staying silent (already posted, no
/// new specimens, or nothing drew a like).
pub async fn weekly_once(
    pool: &PgPool,
    client: &reqwest::Client,
    cfg: &BotConfig,
    dry_run: bool,
) -> anyhow::Result<Option<usize>> {
    let today = chrono::Utc::now().date_naive();
    let (week_start, week_end) = last_completed_week(today);

    let already = sqlx::query!(
        "SELECT week_start FROM bot_weekly WHERE week_start = $1",
        week_start
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if already {
        return Ok(None);
    }

    let specimens = sqlx::query!(
        "SELECT rkey, caption FROM specimens
         WHERE collected_on BETWEEN $1 AND $2 AND removed_at IS NULL",
        week_start,
        week_end,
    )
    .fetch_all(pool)
    .await?;
    if specimens.is_empty() {
        // A silent week gets no wrap-up — and no ledger row, so a late
        // ingest of that week's posts can still produce one.
        return Ok(None);
    }
    let total_collected = specimens.len();

    let meta = sqlx::query!("SELECT artist_did FROM gallery_meta")
        .fetch_one(pool)
        .await?;

    // Like counts via public getPosts (25 uris per call, no auth needed).
    let mut candidates = Vec::new();
    for chunk in specimens.chunks(25) {
        let uris: Vec<String> = chunk
            .iter()
            .map(|s| format!("at://{}/app.bsky.feed.post/{}", meta.artist_did, s.rkey))
            .collect();
        let response: serde_json::Value = client
            .get("https://public.api.bsky.app/xrpc/app.bsky.feed.getPosts")
            .query(
                &uris
                    .iter()
                    .map(|u| ("uris", u.as_str()))
                    .collect::<Vec<_>>(),
            )
            .send()
            .await?
            .error_for_status()
            .context("getPosts for weekly likes failed")?
            .json()
            .await?;
        let Some(posts) = response.get("posts").and_then(|p| p.as_array()) else {
            continue;
        };
        for post in posts {
            let Some(uri) = post.get("uri").and_then(|u| u.as_str()) else {
                continue;
            };
            let Some((_, rkey)) = split_at_uri(uri) else {
                continue;
            };
            let Some(row) = chunk.iter().find(|s| s.rkey == rkey) else {
                continue;
            };
            candidates.push(WeeklyPick {
                label: crate::catalog::ellipsize(
                    row.caption.lines().next().unwrap_or("untitled"),
                    64,
                ),
                rkey,
                likes: post.get("likeCount").and_then(|l| l.as_i64()).unwrap_or(0),
                uri: uri.to_string(),
                cid: post
                    .get("cid")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }

    let picks = select_picks(candidates);
    if picks.is_empty() {
        return Ok(None);
    }

    let (root_text, entries) = compose_weekly(&cfg.public_url, total_collected, &picks);
    if dry_run {
        tracing::info!(week = %week_start, root = %root_text, "weekly wrap-up (dry run)");
        for entry in &entries {
            tracing::info!(entry = %entry.text, quoting = %entry.quoted_uri, "weekly entry (dry run)");
        }
        return Ok(Some(entries.len()));
    }

    let session = login(client, cfg).await?;
    let now = || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let post = |record: serde_json::Value| {
        let client = client.clone();
        let pds = cfg.pds.clone();
        let jwt = session.access_jwt.clone();
        let did = session.did.clone();
        async move {
            let created: serde_json::Value = client
                .post(format!("{pds}/xrpc/com.atproto.repo.createRecord"))
                .bearer_auth(jwt)
                .json(&serde_json::json!({
                    "repo": did,
                    "collection": "app.bsky.feed.post",
                    "record": record,
                }))
                .send()
                .await?
                .error_for_status()
                .context("posting wrap-up entry failed")?
                .json()
                .await?;
            anyhow::Ok((
                created["uri"].as_str().unwrap_or_default().to_string(),
                created["cid"].as_str().unwrap_or_default().to_string(),
            ))
        }
    };

    let root = post(serde_json::json!({
        "$type": "app.bsky.feed.post",
        "text": root_text,
        "createdAt": now(),
    }))
    .await?;

    let mut parent = root.clone();
    for entry in &entries {
        parent = post(serde_json::json!({
            "$type": "app.bsky.feed.post",
            "text": entry.text,
            "facets": entry.facets,
            "createdAt": now(),
            "reply": {
                "root": {"uri": root.0, "cid": root.1},
                "parent": {"uri": parent.0, "cid": parent.1},
            },
            "embed": {
                "$type": "app.bsky.embed.record",
                "record": {"uri": entry.quoted_uri, "cid": entry.quoted_cid},
            },
        }))
        .await?;
    }

    sqlx::query!(
        "INSERT INTO bot_weekly (week_start, root_uri) VALUES ($1, $2)
         ON CONFLICT (week_start) DO NOTHING",
        week_start,
        root.0,
    )
    .execute(pool)
    .await?;
    tracing::info!(week = %week_start, root = %root.0, picks = entries.len(), "weekly wrap-up posted");
    Ok(Some(entries.len()))
}

#[cfg(test)]
mod weekly_tests {
    use super::*;

    fn pick(rkey: &str, likes: i64) -> WeeklyPick {
        WeeklyPick {
            rkey: rkey.into(),
            label: format!("specimen {rkey}"),
            likes,
            uri: format!("at://did:plc:artist/app.bsky.feed.post/{rkey}"),
            cid: "bafy-post".into(),
        }
    }

    #[test]
    fn weeks_are_monday_through_sunday_and_completed() {
        // Wednesday 2026-07-01 → previous week Mon 22 Jun ..= Sun 28 Jun.
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let (start, end) = last_completed_week(today);
        assert_eq!(start, chrono::NaiveDate::from_ymd_opt(2026, 6, 22).unwrap());
        assert_eq!(end, chrono::NaiveDate::from_ymd_opt(2026, 6, 28).unwrap());
        // A Monday wraps the week that just ended yesterday.
        let monday = chrono::NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        let (start, end) = last_completed_week(monday);
        assert_eq!(start, chrono::NaiveDate::from_ymd_opt(2026, 6, 29).unwrap());
        assert_eq!(end, chrono::NaiveDate::from_ymd_opt(2026, 7, 5).unwrap());
    }

    #[test]
    fn picks_top_three_by_likes_and_drop_unliked() {
        let picks = select_picks(vec![
            pick("3a", 2),
            pick("3b", 9),
            pick("3c", 0),
            pick("3d", 5),
            pick("3e", 3),
        ]);
        let rkeys: Vec<_> = picks.iter().map(|p| p.rkey.as_str()).collect();
        assert_eq!(rkeys, ["3b", "3d", "3e"]);
    }

    #[test]
    fn slow_weeks_get_fewer_and_silence_gets_none() {
        assert_eq!(select_picks(vec![pick("3a", 1)]).len(), 1);
        assert!(select_picks(vec![pick("3a", 0)]).is_empty());
        assert!(select_picks(Vec::new()).is_empty());
    }

    #[test]
    fn wrap_up_thread_composes_with_quote_embeds() {
        let picks = vec![pick("3b", 9), pick("3d", 5)];
        let (root, entries) = compose_weekly("https://fluoddity.example", 7, &picks);
        assert!(root.contains("7 new specimens"));
        assert!(root.contains("https://fluoddity.example/archive"));
        assert_eq!(entries.len(), 2);
        let entry = &entries[0];
        assert!(entry.text.starts_with("1. “specimen 3b” — 9 likes"));
        assert!(entry.quoted_uri.ends_with("/3b"));
        let start = entry.facets[0]["index"]["byteStart"].as_u64().unwrap() as usize;
        let end = entry.facets[0]["index"]["byteEnd"].as_u64().unwrap() as usize;
        assert_eq!(
            &entry.text.as_bytes()[start..end],
            b"https://fluoddity.example/specimen/3b"
        );
    }
}

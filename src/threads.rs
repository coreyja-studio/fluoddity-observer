//! Bluesky threads as rooms.
//!
//! A guest room is just a thread: the author is the curator, their posts are
//! the walk through the room, and their words are the wall labels. We render
//! live from `getPostThread` (with a short cache) — the curator edits their
//! room by editing their thread.
//!
//! Reference extraction is zero-trust: only the artist's specimens render.
//! From each post *by the thread author* we accept:
//!   1. the post itself, when the author IS the artist and it embeds a video
//!      (the artist's own threads become rooms for free);
//!   2. quote-posts of the artist's posts;
//!   3. links to `bsky.app/profile/<artist>/post/<rkey>`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;

const PUBLIC_API: &str = "https://public.api.bsky.app";
const CACHE_TTL: Duration = Duration::from_secs(300);

/// A thread parsed into room form.
#[derive(Debug, Clone)]
pub struct ThreadRoom {
    pub author_did: String,
    pub author_handle: String,
    pub author_display: String,
    pub rkey: String,
    /// First line of the root post, ellipsized — the room's name.
    pub title: String,
    /// Root post text, verbatim — the room's introduction.
    pub intro: String,
    pub entries: Vec<ThreadEntry>,
}

/// One specimen hung in a thread room.
#[derive(Debug, Clone)]
pub struct ThreadEntry {
    /// rkey of the artist's post — resolved against the archive at render.
    pub specimen_rkey: String,
    /// The thread author's post text, verbatim — their wall label.
    pub note: String,
}

impl ThreadRoom {
    pub fn thread_url(&self) -> String {
        format!(
            "https://bsky.app/profile/{}/post/{}",
            self.author_handle, self.rkey
        )
    }
}

/// Parse a getPostThread response. Returns None when the thread is missing
/// or malformed; a valid thread with zero artist references yields an empty
/// `entries` list (the room exists, nothing is hung).
pub fn parse_thread(
    response: &serde_json::Value,
    artist_did: &str,
    artist_handle: &str,
) -> Option<ThreadRoom> {
    let root = response.get("thread")?;
    let root_post = root.get("post")?;
    let author = root_post.get("author")?;
    let author_did = author.get("did")?.as_str()?.to_string();
    let author_handle = author.get("handle")?.as_str()?.to_string();
    let author_display = author
        .get("displayName")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
        .unwrap_or(&author_handle)
        .to_string();
    let rkey = root_post
        .get("uri")?
        .as_str()?
        .rsplit('/')
        .next()?
        .to_string();
    let intro = root_post
        .get("record")
        .and_then(|r| r.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let mut entries = Vec::new();
    collect(root, &author_did, artist_did, artist_handle, &mut entries);

    Some(ThreadRoom {
        title: crate::catalog::ellipsize(intro.lines().next().unwrap_or(""), 60),
        author_did,
        author_handle,
        author_display,
        rkey,
        intro,
        entries,
    })
}

/// Depth-first walk in thread order, keeping only the author's own posts.
fn collect(
    node: &serde_json::Value,
    author_did: &str,
    artist_did: &str,
    artist_handle: &str,
    entries: &mut Vec<ThreadEntry>,
) {
    let Some(post) = node.get("post") else { return };
    let post_author = post
        .get("author")
        .and_then(|a| a.get("did"))
        .and_then(|d| d.as_str())
        .unwrap_or("");
    if post_author == author_did {
        let note = post
            .get("record")
            .and_then(|r| r.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        for rkey in extract_refs(post, artist_did, artist_handle) {
            if !entries.iter().any(|e| e.specimen_rkey == rkey) {
                entries.push(ThreadEntry {
                    specimen_rkey: rkey,
                    note: note.to_string(),
                });
            }
        }
    }
    if let Some(replies) = node.get("replies").and_then(|r| r.as_array()) {
        for reply in replies {
            collect(reply, author_did, artist_did, artist_handle, entries);
        }
    }
}

/// Artist-post rkeys referenced by one post.
fn extract_refs(post: &serde_json::Value, artist_did: &str, artist_handle: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let Some(record) = post.get("record") else {
        return refs;
    };
    let post_author = post
        .get("author")
        .and_then(|a| a.get("did"))
        .and_then(|d| d.as_str())
        .unwrap_or("");

    if let Some(embed) = record.get("embed") {
        let embed_type = embed.get("$type").and_then(|t| t.as_str()).unwrap_or("");
        // Rule 1: the artist's own video post is itself a specimen.
        if embed_type == "app.bsky.embed.video"
            && post_author == artist_did
            && let Some(rkey) = post
                .get("uri")
                .and_then(|u| u.as_str())
                .and_then(|u| u.rsplit('/').next())
        {
            refs.push(rkey.to_string());
        }
        // Rule 2: quote-posts of the artist's posts.
        let quoted_uri = match embed_type {
            "app.bsky.embed.record" => embed.get("record").and_then(|r| r.get("uri")),
            "app.bsky.embed.recordWithMedia" => embed
                .get("record")
                .and_then(|r| r.get("record"))
                .and_then(|r| r.get("uri")),
            _ => None,
        };
        if let Some(uri) = quoted_uri.and_then(|u| u.as_str())
            && let Some(rkey) = artist_post_rkey_from_at_uri(uri, artist_did)
        {
            refs.push(rkey);
        }
    }

    // Rule 3: bsky.app links to the artist's posts (via rich-text facets).
    if let Some(facets) = record.get("facets").and_then(|f| f.as_array()) {
        for facet in facets {
            let Some(features) = facet.get("features").and_then(|f| f.as_array()) else {
                continue;
            };
            for feature in features {
                if feature.get("$type").and_then(|t| t.as_str())
                    != Some("app.bsky.richtext.facet#link")
                {
                    continue;
                }
                if let Some(uri) = feature.get("uri").and_then(|u| u.as_str())
                    && let Some(rkey) =
                        artist_post_rkey_from_web_url(uri, artist_did, artist_handle)
                {
                    refs.push(rkey);
                }
            }
        }
    }

    refs.dedup();
    refs
}

fn artist_post_rkey_from_at_uri(uri: &str, artist_did: &str) -> Option<String> {
    let rest = uri.strip_prefix("at://")?;
    let mut parts = rest.splitn(3, '/');
    let (did, collection, rkey) = (parts.next()?, parts.next()?, parts.next()?);
    (did == artist_did && collection == "app.bsky.feed.post").then(|| rkey.to_string())
}

fn artist_post_rkey_from_web_url(
    uri: &str,
    artist_did: &str,
    artist_handle: &str,
) -> Option<String> {
    let rest = uri
        .strip_prefix("https://bsky.app/profile/")
        .or_else(|| uri.strip_prefix("https://staging.bsky.app/profile/"))?;
    let (who, rest) = rest.split_once("/post/")?;
    let rkey = rest.split(['?', '/', '#']).next()?;
    (who == artist_did || who == artist_handle).then(|| rkey.to_string())
}

type CacheKey = (String, String);
type CacheEntry = (Instant, Arc<ThreadRoom>);

/// Live thread fetcher with a small TTL cache.
pub struct ThreadFetcher {
    client: reqwest::Client,
    cache: tokio::sync::Mutex<HashMap<CacheKey, CacheEntry>>,
}

impl ThreadFetcher {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            cache: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Fetch (or reuse) a thread room. `author` may be a handle or a DID.
    pub async fn fetch(
        &self,
        author: &str,
        rkey: &str,
        artist_did: &str,
        artist_handle: &str,
    ) -> anyhow::Result<Option<Arc<ThreadRoom>>> {
        let key = (author.to_string(), rkey.to_string());
        {
            let cache = self.cache.lock().await;
            if let Some((at, room)) = cache.get(&key)
                && at.elapsed() < CACHE_TTL
            {
                return Ok(Some(room.clone()));
            }
        }

        let did = if author.starts_with("did:") {
            author.to_string()
        } else {
            let resolved: serde_json::Value = self
                .client
                .get(format!(
                    "{PUBLIC_API}/xrpc/com.atproto.identity.resolveHandle"
                ))
                .query(&[("handle", author)])
                .send()
                .await?
                .error_for_status()
                .context("resolving thread author handle")?
                .json()
                .await?;
            match resolved.get("did").and_then(|d| d.as_str()) {
                Some(did) => did.to_string(),
                None => return Ok(None),
            }
        };

        let uri = format!("at://{did}/app.bsky.feed.post/{rkey}");
        let response = self
            .client
            .get(format!("{PUBLIC_API}/xrpc/app.bsky.feed.getPostThread"))
            .query(&[("uri", uri.as_str()), ("depth", "100")])
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST
            || response.status() == reqwest::StatusCode::NOT_FOUND
        {
            return Ok(None);
        }
        let body: serde_json::Value = response
            .error_for_status()
            .context("fetching thread")?
            .json()
            .await?;

        let Some(room) = parse_thread(&body, artist_did, artist_handle) else {
            return Ok(None);
        };
        let room = Arc::new(room);
        self.cache
            .lock()
            .await
            .insert(key, (Instant::now(), room.clone()));
        Ok(Some(room))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTIST_DID: &str = "did:plc:rvkbczphgh3krtnopp2oobty";
    const ARTIST_HANDLE: &str = "all-paperclips.bsky.social";

    fn artist_spine() -> serde_json::Value {
        serde_json::from_str(include_str!("../fixtures/thread_artist_spine.json")).unwrap()
    }

    #[test]
    fn artist_self_thread_becomes_a_room() {
        let room = parse_thread(&artist_spine(), ARTIST_DID, ARTIST_HANDLE).unwrap();
        assert_eq!(room.author_handle, ARTIST_HANDLE);
        assert_eq!(room.rkey, "3mmsgt3pqpc2f");
        assert!(room.title.starts_with("The hardest part"));
        // The spine has 11 of the artist's video posts; replies by others
        // (and their quotes of non-artist posts) are excluded.
        assert_eq!(room.entries.len(), 11);
        assert_eq!(room.entries[0].specimen_rkey, "3mmsgt3pqpc2f");
        assert_eq!(room.entries.last().unwrap().specimen_rkey, "3mmsgt3pumi2f");
        assert!(room.entries[0].note.starts_with("The hardest part"));
    }

    #[test]
    fn guest_thread_quotes_and_links_resolve_to_artist_posts() {
        let guest = serde_json::json!({
            "thread": {
                "post": {
                    "uri": "at://did:plc:guest/app.bsky.feed.post/3root",
                    "author": {"did": "did:plc:guest", "handle": "guest.test", "displayName": "A Guest"},
                    "record": {"text": "my favorite fluoddities — a tour\nwith notes"}
                },
                "replies": [{
                    "post": {
                        "uri": "at://did:plc:guest/app.bsky.feed.post/3quote",
                        "author": {"did": "did:plc:guest", "handle": "guest.test"},
                        "record": {
                            "text": "the shoggoth one, obviously",
                            "embed": {"$type": "app.bsky.embed.record",
                                      "record": {"uri": format!("at://{}/app.bsky.feed.post/3mphrfiy5hs2c", ARTIST_DID)}}
                        }
                    },
                    "replies": [{
                        "post": {
                            "uri": "at://did:plc:guest/app.bsky.feed.post/3link",
                            "author": {"did": "did:plc:guest", "handle": "guest.test"},
                            "record": {
                                "text": "and the jellyfish bsky.app/profile/...",
                                "facets": [{"features": [{"$type": "app.bsky.richtext.facet#link",
                                    "uri": format!("https://bsky.app/profile/{}/post/3mnilf6tunc2i", ARTIST_HANDLE)}],
                                    "index": {"byteStart": 0, "byteEnd": 1}}]
                            }
                        }
                    }, {
                        "post": {
                            "uri": "at://did:plc:interloper/app.bsky.feed.post/3evil",
                            "author": {"did": "did:plc:interloper", "handle": "interloper.test"},
                            "record": {
                                "text": "look at MY thing instead",
                                "embed": {"$type": "app.bsky.embed.record",
                                          "record": {"uri": "at://did:plc:interloper/app.bsky.feed.post/3notart"}}
                            }
                        }
                    }]
                }]
            }
        });
        let room = parse_thread(&guest, ARTIST_DID, ARTIST_HANDLE).unwrap();
        assert_eq!(room.title, "my favorite fluoddities — a tour");
        assert_eq!(room.author_display, "A Guest");
        let rkeys: Vec<_> = room
            .entries
            .iter()
            .map(|e| e.specimen_rkey.as_str())
            .collect();
        assert_eq!(rkeys, ["3mphrfiy5hs2c", "3mnilf6tunc2i"]);
        assert_eq!(room.entries[0].note, "the shoggoth one, obviously");
    }

    #[test]
    fn quotes_of_non_artist_posts_are_ignored() {
        let thread = serde_json::json!({
            "thread": {"post": {
                "uri": "at://did:plc:guest/app.bsky.feed.post/3root",
                "author": {"did": "did:plc:guest", "handle": "guest.test"},
                "record": {
                    "text": "quoting someone else",
                    "embed": {"$type": "app.bsky.embed.record",
                              "record": {"uri": "at://did:plc:other/app.bsky.feed.post/3xyz"}}
                }
            }}
        });
        let room = parse_thread(&thread, ARTIST_DID, ARTIST_HANDLE).unwrap();
        assert!(room.entries.is_empty());
    }

    #[test]
    fn web_url_matching_requires_artist() {
        assert_eq!(
            artist_post_rkey_from_web_url(
                "https://bsky.app/profile/all-paperclips.bsky.social/post/3abc?ref=x",
                ARTIST_DID,
                ARTIST_HANDLE
            ),
            Some("3abc".to_string())
        );
        assert_eq!(
            artist_post_rkey_from_web_url(
                "https://bsky.app/profile/someone-else.bsky.social/post/3abc",
                ARTIST_DID,
                ARTIST_HANDLE
            ),
            None
        );
    }

    #[test]
    fn malformed_thread_is_none() {
        assert!(parse_thread(&serde_json::json!({}), ARTIST_DID, ARTIST_HANDLE).is_none());
    }
}

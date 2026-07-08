//! Dispatches from the expedition: an Atom feed of the newest sightings,
//! plus a sitemap so the permanent pages are findable. Field guides
//! predate feed readers by a couple of centuries, but they'd have loved
//! them.

use crate::catalog::Catalog;
use crate::db::ThreadRoomRow;

/// How many sightings the feed carries.
const FEED_ENTRIES: usize = 30;

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// The Atom feed: newest sightings first, the artist's words verbatim.
pub fn atom(catalog: &Catalog, public_url: &str) -> String {
    let newest: Vec<_> = catalog
        .archive
        .all()
        .iter()
        .rev()
        .take(FEED_ENTRIES)
        .collect();
    let updated = newest
        .first()
        .map(|s| s.date.as_str())
        .unwrap_or("2026-01-01");
    let artist = xml_escape(&catalog.editorial.artist.name);

    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);
    out.push('\n');
    out.push_str(r#"<feed xmlns="http://www.w3.org/2005/Atom">"#);
    out.push_str(&format!(
        "<title>Fluoddity — dispatches from the expedition</title>\
         <subtitle>new specimens from a universe that only exists inside one GPU</subtitle>\
         <id>{public_url}/</id>\
         <link href=\"{public_url}/feed.xml\" rel=\"self\"/>\
         <link href=\"{public_url}/\"/>\
         <updated>{updated}T00:00:00Z</updated>\
         <author><name>{artist}</name></author>",
    ));
    for s in newest {
        let url = format!("{public_url}/specimen/{}", s.rkey);
        out.push_str(&format!(
            "<entry>\
             <title>{title}</title>\
             <id>{url}</id>\
             <link href=\"{url}\"/>\
             <published>{date}T00:00:00Z</published>\
             <updated>{date}T00:00:00Z</updated>\
             <summary>{summary}</summary>\
             </entry>",
            title = xml_escape(&s.label()),
            date = s.date,
            summary = xml_escape(s.caption.trim()),
        ));
    }
    out.push_str("</feed>");
    out
}

/// Every permanent page: the index, the archive, the colophon, every
/// specimen, every tag, every registered room.
pub fn sitemap(catalog: &Catalog, rooms: &[ThreadRoomRow], public_url: &str) -> String {
    let mut urls: Vec<String> = vec![
        format!("{public_url}/"),
        format!("{public_url}/archive"),
        format!("{public_url}/colophon"),
    ];
    for s in catalog.archive.all() {
        urls.push(format!("{public_url}/specimen/{}", s.rkey));
    }
    let mut tags: Vec<&str> = catalog
        .editorial
        .tags
        .values()
        .flatten()
        .map(|t| t.tag.as_str())
        .collect();
    tags.sort_unstable();
    tags.dedup();
    for tag in tags {
        urls.push(format!("{public_url}/tag/{tag}"));
    }
    for room in rooms {
        urls.push(format!(
            "{public_url}/room/{}/{}",
            room.author_handle, room.rkey
        ));
    }

    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);
    out.push('\n');
    out.push_str(r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
    for url in urls {
        out.push_str(&format!("<url><loc>{}</loc></url>", xml_escape(&url)));
    }
    out.push_str("</urlset>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Archive, Artist, Editorial, MediaKind, Origin, Specimen};
    use std::collections::HashMap;

    fn catalog() -> Catalog {
        let specimens = (1..=40)
            .map(|i| Specimen {
                rkey: format!("3m{i:03}"),
                cid: "bafy".into(),
                kind: MediaKind::Video,
                file: None,
                pds_key: None,
                master_key: None,
                og_poster_key: None,
                caption: format!("Sighting <#{i}> & counting"),
                date: format!("2026-06-{:02}", (i % 28) + 1),
                url: "https://bsky.app/x".into(),
                images: Vec::new(),
            })
            .collect();
        Catalog {
            archive: Archive::new(specimens),
            editorial: Editorial {
                artist: Artist {
                    handle: "artist.test".into(),
                    did: "did:plc:artist".into(),
                    name: "Oops & Co".into(),
                },
                origin: Origin {
                    handle: "o".into(),
                    text: "wish".into(),
                    url: "u".into(),
                },
                tags: HashMap::new(),
                margin_notes: HashMap::new(),
            },
        }
    }

    #[test]
    fn atom_feed_is_escaped_and_capped() {
        let feed = atom(&catalog(), "https://fluoddity.example");
        assert!(feed.starts_with(r#"<?xml version="1.0" encoding="utf-8"?>"#));
        assert_eq!(feed.matches("<entry>").count(), FEED_ENTRIES);
        assert!(feed.contains("Oops &amp; Co"));
        assert!(feed.contains("&lt;#"), "captions are escaped");
        assert!(!feed.contains("<#"), "no raw angle brackets from captions");
        assert!(feed.contains("https://fluoddity.example/feed.xml"));
    }

    #[test]
    fn sitemap_lists_every_permanent_page() {
        let rooms = vec![ThreadRoomRow {
            author_did: "did:plc:g".into(),
            author_handle: "guest.test".into(),
            rkey: "3mroom".into(),
            title: "t".into(),
        }];
        let map = sitemap(&catalog(), &rooms, "https://fluoddity.example");
        assert_eq!(map.matches("<url>").count(), 3 + 40 + 1);
        assert!(map.contains("https://fluoddity.example/specimen/3m001"));
        assert!(map.contains("https://fluoddity.example/room/guest.test/3mroom"));
    }
}

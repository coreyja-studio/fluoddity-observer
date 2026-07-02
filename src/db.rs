use std::collections::HashMap;

use anyhow::Context;
use sqlx::PgPool;

use crate::catalog::{Archive, Artist, Catalog, Editorial, MarginNote, Origin, Specimen, Tag};

/// Load the full catalog (archive + editorial layer) from the database.
///
/// The whole dataset is a few hundred rows, so each request assembles a fresh
/// snapshot — admin edits and freshly ingested specimens are visible
/// immediately, with no cache to invalidate.
pub async fn load_catalog(pool: &PgPool) -> anyhow::Result<Catalog> {
    let meta = sqlx::query!(
        "SELECT artist_handle, artist_did, artist_name, origin_handle, origin_text, origin_url
         FROM gallery_meta"
    )
    .fetch_one(pool)
    .await
    .context("gallery_meta is empty — run `paperclips-gallery import` to seed the database")?;

    let specimens = sqlx::query!(
        "SELECT rkey, cid, file, caption, collected_on, url
         FROM specimens
         ORDER BY collected_on, rkey"
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| Specimen {
        rkey: row.rkey,
        cid: row.cid,
        file: row.file,
        caption: row.caption,
        date: row.collected_on.format("%Y-%m-%d").to_string(),
        url: row.url,
    })
    .collect();

    let mut tags: HashMap<String, Vec<Tag>> = HashMap::new();
    for row in sqlx::query!("SELECT rkey, tag, kind, source FROM specimen_tags ORDER BY rkey, tag")
        .fetch_all(pool)
        .await?
    {
        tags.entry(row.rkey).or_default().push(Tag {
            tag: row.tag,
            kind: row.kind,
            source: row.source,
        });
    }

    let mut margin_notes: HashMap<String, Vec<MarginNote>> = HashMap::new();
    for row in sqlx::query!("SELECT rkey, handle, note FROM margin_notes ORDER BY rkey, position")
        .fetch_all(pool)
        .await?
    {
        margin_notes.entry(row.rkey).or_default().push(MarginNote {
            handle: row.handle,
            text: row.note,
        });
    }

    Ok(Catalog {
        archive: Archive::new(specimens),
        editorial: Editorial {
            artist: Artist {
                handle: meta.artist_handle,
                did: meta.artist_did,
                name: meta.artist_name,
            },
            origin: Origin {
                handle: meta.origin_handle,
                text: meta.origin_text,
                url: meta.origin_url,
            },
            tags,
            margin_notes,
        },
    })
}

/// One row of the archive pull's metadata.jsonl.
#[derive(serde::Deserialize)]
struct MetadataRow {
    file: String,
    kind: String,
    cid: String,
    rkey: String,
    caption: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    url: String,
}

/// Import-time shape of catalog.json (the flat-file editorial seed).
#[derive(serde::Deserialize)]
struct ImportSeed {
    artist: Artist,
    origin: Origin,
    #[serde(default)]
    families: Vec<FamilySeed>,
    #[serde(default)]
    margin_notes: HashMap<String, Vec<MarginNote>>,
}

/// Legacy family entry — imported as a lineage tag named by its slug.
#[derive(serde::Deserialize)]
struct FamilySeed {
    slug: String,
    rkeys: Vec<String>,
}

pub struct ImportStats {
    pub specimens: usize,
    pub lineage_tags: usize,
    pub margin_notes: usize,
}

/// Seed the database from the flat-file era: `metadata.jsonl` (archive) and
/// `catalog.json` (editorial layer). Idempotent — specimens upsert, and the
/// editorial tables are replaced wholesale.
pub async fn import(
    pool: &PgPool,
    metadata_path: &str,
    catalog_path: &str,
) -> anyhow::Result<ImportStats> {
    let metadata = std::fs::read_to_string(metadata_path)
        .with_context(|| format!("reading archive metadata from {metadata_path}"))?;
    let rows: Vec<MetadataRow> = metadata
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).context("parsing metadata.jsonl row"))
        .collect::<anyhow::Result<_>>()?;

    let editorial_raw = std::fs::read_to_string(catalog_path)
        .with_context(|| format!("reading catalog from {catalog_path}"))?;
    let seed: ImportSeed = serde_json::from_str(&editorial_raw).context("parsing catalog.json")?;

    let mut tx = pool.begin().await?;

    let mut specimen_count = 0;
    for row in rows.iter().filter(|r| r.kind == "video") {
        let collected_on = chrono::NaiveDate::parse_from_str(
            &row.created_at[..10.min(row.created_at.len())],
            "%Y-%m-%d",
        )
        .with_context(|| format!("bad createdAt for {}", row.rkey))?;
        sqlx::query!(
            "INSERT INTO specimens (rkey, cid, file, caption, collected_on, url)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (rkey) DO UPDATE
             SET cid = EXCLUDED.cid, file = EXCLUDED.file,
                 caption = EXCLUDED.caption, collected_on = EXCLUDED.collected_on,
                 url = EXCLUDED.url",
            row.rkey,
            row.cid,
            row.file,
            row.caption,
            collected_on,
            row.url,
        )
        .execute(&mut *tx)
        .await?;
        specimen_count += 1;
    }

    // Margin notes are replaced wholesale from the seed; tags are additive
    // (curator- and post-sourced tags must survive a reimport).
    sqlx::query!("DELETE FROM margin_notes")
        .execute(&mut *tx)
        .await?;

    let mut lineage_tag_count = 0;
    for family in &seed.families {
        for rkey in &family.rkeys {
            let inserted = sqlx::query!(
                "INSERT INTO specimen_tags (rkey, tag, kind, source)
                 VALUES ($1, $2, 'lineage', 'curator')
                 ON CONFLICT (rkey, tag) DO NOTHING",
                rkey,
                family.slug,
            )
            .execute(&mut *tx)
            .await?;
            lineage_tag_count += inserted.rows_affected() as usize;
        }
    }

    // The artist can tag specimens from inside his own posts: #hashtags in
    // captions become post-sourced tags.
    for row in rows.iter().filter(|r| r.kind == "video") {
        for tag in crate::catalog::extract_hashtags(&row.caption) {
            sqlx::query!(
                "INSERT INTO specimen_tags (rkey, tag, kind, source)
                 VALUES ($1, $2, 'tag', 'post')
                 ON CONFLICT (rkey, tag) DO NOTHING",
                row.rkey,
                tag,
            )
            .execute(&mut *tx)
            .await?;
        }
    }

    let mut note_count = 0;
    for (rkey, notes) in &seed.margin_notes {
        for (i, note) in notes.iter().enumerate() {
            sqlx::query!(
                "INSERT INTO margin_notes (rkey, handle, note, position) VALUES ($1, $2, $3, $4)",
                rkey,
                note.handle,
                note.text,
                i as i32,
            )
            .execute(&mut *tx)
            .await?;
            note_count += 1;
        }
    }

    sqlx::query!(
        "INSERT INTO gallery_meta
             (only_row, artist_handle, artist_did, artist_name,
              origin_handle, origin_text, origin_url)
         VALUES (TRUE, $1, $2, $3, $4, $5, $6)
         ON CONFLICT (only_row) DO UPDATE
         SET artist_handle = EXCLUDED.artist_handle, artist_did = EXCLUDED.artist_did,
             artist_name = EXCLUDED.artist_name, origin_handle = EXCLUDED.origin_handle,
             origin_text = EXCLUDED.origin_text, origin_url = EXCLUDED.origin_url",
        seed.artist.handle,
        seed.artist.did,
        seed.artist.name,
        seed.origin.handle,
        seed.origin.text,
        seed.origin.url,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(ImportStats {
        specimens: specimen_count,
        lineage_tags: lineage_tag_count,
        margin_notes: note_count,
    })
}

/// A registered thread room. Rooms authored by the artist are the museum's
/// first-class rooms; others hang as guest rooms.
pub struct ThreadRoomRow {
    pub author_did: String,
    pub author_handle: String,
    pub rkey: String,
    pub title: String,
}

pub async fn thread_rooms(pool: &PgPool) -> anyhow::Result<Vec<ThreadRoomRow>> {
    Ok(sqlx::query_as!(
        ThreadRoomRow,
        "SELECT author_did, author_handle, rkey, title FROM thread_rooms ORDER BY added_at"
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixtures(dir: &std::path::Path) -> (String, String) {
        let metadata = concat!(
            r#"{"file":"videos/a.mp4","kind":"video","mime":"video/mp4","cid":"bafy-a","rkey":"3ma","caption":"Jellyfish!","alt":"","createdAt":"2026-06-04T12:00:00.000Z","url":"https://bsky.app/profile/x/post/3ma"}"#,
            "\n",
            r#"{"file":"images/skip.jpg","kind":"image","mime":"image/jpeg","cid":"bafy-img","rkey":"3mimg","caption":"not a video","alt":"","createdAt":"2026-06-05T12:00:00.000Z","url":"https://bsky.app/profile/x/post/3mimg"}"#,
            "\n",
            r#"{"file":"videos/b.mp4","kind":"video","mime":"video/mp4","cid":"bafy-b","rkey":"3mb","caption":"Party hats","alt":"","createdAt":"2026-06-05T12:00:00.000Z","url":"https://bsky.app/profile/x/post/3mb"}"#,
            "\n",
        );
        let catalog = r#"{
            "artist": {"handle": "artist.test", "did": "did:plc:test", "name": "Test Artist"},
            "origin": {"handle": "wisher.test", "text": "wish", "url": "https://bsky.app/wish"},
            "rooms": [{"slug": "the-medusae", "title": "The Medusae", "description": "soft bodies", "rkeys": ["3mb", "3ma"]}],
            "families": [{"slug": "jelly-line", "title": "The Jelly Line", "rkeys": ["3ma", "3mb"]}],
            "margin_notes": {"3ma": [{"handle": "fan.test", "text": "Shoggoth found"}]}
        }"#;
        let meta_path = dir.join("metadata.jsonl");
        let cat_path = dir.join("catalog.json");
        std::fs::write(&meta_path, metadata).unwrap();
        std::fs::write(&cat_path, catalog).unwrap();
        (
            meta_path.to_str().unwrap().to_string(),
            cat_path.to_str().unwrap().to_string(),
        )
    }

    #[sqlx::test]
    async fn import_then_load_roundtrips(pool: PgPool) {
        let dir = std::env::temp_dir().join(format!("pcg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (meta_path, cat_path) = write_fixtures(&dir);

        let stats = import(&pool, &meta_path, &cat_path).await.unwrap();
        assert_eq!(stats.specimens, 2, "images are skipped");
        assert_eq!(stats.margin_notes, 1);

        // Idempotent: importing again must not duplicate anything.
        import(&pool, &meta_path, &cat_path).await.unwrap();

        let catalog = load_catalog(&pool).await.unwrap();
        assert_eq!(catalog.archive.len(), 2);
        assert_eq!(catalog.editorial.artist.did, "did:plc:test");

        let jelly = catalog.archive.get("3ma").unwrap();
        assert_eq!(jelly.caption, "Jellyfish!");
        assert_eq!(jelly.date, "2026-06-04");
        assert_eq!(jelly.file.as_deref(), Some("videos/a.mp4"));
        assert_eq!(catalog.notes_of("3ma")[0].text, "Shoggoth found");
        let jelly_tags = catalog.tags_of("3mb");
        assert_eq!(jelly_tags[0].tag, "jelly-line");
        assert_eq!(jelly_tags[0].kind, "lineage");
        assert_eq!(catalog.tagged("jelly-line").len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }
}

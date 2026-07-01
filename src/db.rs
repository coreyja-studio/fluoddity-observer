use std::collections::HashMap;

use anyhow::Context;
use sqlx::PgPool;

use crate::catalog::{
    Archive, Artist, Catalog, Editorial, Family, MarginNote, Origin, Room, Specimen,
};

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

    let mut room_rkeys: HashMap<String, Vec<String>> = HashMap::new();
    for row in
        sqlx::query!("SELECT room_slug, rkey FROM room_specimens ORDER BY room_slug, position")
            .fetch_all(pool)
            .await?
    {
        room_rkeys.entry(row.room_slug).or_default().push(row.rkey);
    }

    let rooms = sqlx::query!("SELECT slug, title, description FROM rooms ORDER BY position")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| Room {
            rkeys: room_rkeys.remove(&row.slug).unwrap_or_default(),
            slug: row.slug,
            title: row.title,
            description: row.description,
        })
        .collect();

    let mut family_rkeys: HashMap<String, Vec<String>> = HashMap::new();
    for row in
        sqlx::query!("SELECT family_slug, rkey FROM family_members ORDER BY family_slug, position")
            .fetch_all(pool)
            .await?
    {
        family_rkeys
            .entry(row.family_slug)
            .or_default()
            .push(row.rkey);
    }

    let families = sqlx::query!("SELECT slug, title FROM families ORDER BY position")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| Family {
            rkeys: family_rkeys.remove(&row.slug).unwrap_or_default(),
            slug: row.slug,
            title: row.title,
        })
        .collect();

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
            rooms,
            families,
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

pub struct ImportStats {
    pub specimens: usize,
    pub rooms: usize,
    pub families: usize,
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
    let editorial: Editorial =
        serde_json::from_str(&editorial_raw).context("parsing catalog.json")?;

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

    // The editorial layer is replaced wholesale; the seed file is the source
    // of truth at import time.
    sqlx::query!("DELETE FROM margin_notes")
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM room_specimens")
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM family_members")
        .execute(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM rooms").execute(&mut *tx).await?;
    sqlx::query!("DELETE FROM families")
        .execute(&mut *tx)
        .await?;

    for (i, room) in editorial.rooms.iter().enumerate() {
        sqlx::query!(
            "INSERT INTO rooms (slug, title, description, position) VALUES ($1, $2, $3, $4)",
            room.slug,
            room.title,
            room.description,
            i as i32,
        )
        .execute(&mut *tx)
        .await?;
        for (j, rkey) in room.rkeys.iter().enumerate() {
            sqlx::query!(
                "INSERT INTO room_specimens (room_slug, rkey, position) VALUES ($1, $2, $3)",
                room.slug,
                rkey,
                j as i32,
            )
            .execute(&mut *tx)
            .await?;
        }
    }

    for (i, family) in editorial.families.iter().enumerate() {
        sqlx::query!(
            "INSERT INTO families (slug, title, position) VALUES ($1, $2, $3)",
            family.slug,
            family.title,
            i as i32,
        )
        .execute(&mut *tx)
        .await?;
        for (j, rkey) in family.rkeys.iter().enumerate() {
            sqlx::query!(
                "INSERT INTO family_members (family_slug, rkey, position) VALUES ($1, $2, $3)",
                family.slug,
                rkey,
                j as i32,
            )
            .execute(&mut *tx)
            .await?;
        }
    }

    let mut note_count = 0;
    for (rkey, notes) in &editorial.margin_notes {
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
        editorial.artist.handle,
        editorial.artist.did,
        editorial.artist.name,
        editorial.origin.handle,
        editorial.origin.text,
        editorial.origin.url,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(ImportStats {
        specimens: specimen_count,
        rooms: editorial.rooms.len(),
        families: editorial.families.len(),
        margin_notes: note_count,
    })
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
        assert_eq!(stats.rooms, 1);
        assert_eq!(stats.margin_notes, 1);

        // Idempotent: importing again must not duplicate anything.
        import(&pool, &meta_path, &cat_path).await.unwrap();

        let catalog = load_catalog(&pool).await.unwrap();
        assert_eq!(catalog.archive.len(), 2);
        assert_eq!(catalog.editorial.artist.did, "did:plc:test");

        let room = catalog.room("the-medusae").unwrap();
        let members: Vec<_> = catalog
            .room_specimens(room)
            .map(|s| s.rkey.as_str())
            .collect();
        assert_eq!(members, ["3mb", "3ma"], "curated order preserved");

        let jelly = catalog.archive.get("3ma").unwrap();
        assert_eq!(jelly.caption, "Jellyfish!");
        assert_eq!(jelly.date, "2026-06-04");
        assert_eq!(jelly.file.as_deref(), Some("videos/a.mp4"));
        assert_eq!(catalog.notes_of("3ma")[0].text, "Shoggoth found");
        assert_eq!(catalog.families_of("3mb")[0].title, "The Jelly Line");
        assert_eq!(catalog.room_of("3ma").unwrap().slug, "the-medusae");

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// A registered guest room (a Bluesky thread hung on the front page).
pub struct GuestRoomRow {
    pub author_did: String,
    pub author_handle: String,
    pub rkey: String,
    pub title: String,
}

pub async fn guest_rooms(pool: &PgPool) -> anyhow::Result<Vec<GuestRoomRow>> {
    Ok(sqlx::query_as!(
        GuestRoomRow,
        "SELECT author_did, author_handle, rkey, title FROM guest_rooms ORDER BY added_at"
    )
    .fetch_all(pool)
    .await?)
}

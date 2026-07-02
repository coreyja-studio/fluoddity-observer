use std::collections::HashMap;

use anyhow::Context;
use serde::Deserialize;

/// The full expedition record: every video specimen from the artist's
/// archive (`metadata.jsonl`), chronological. Source of truth for specimen
/// data — every entry gets a durable page whether curated or not.
#[derive(Debug)]
pub struct Archive {
    specimens: Vec<Specimen>,
    by_rkey: HashMap<String, usize>,
}

/// The editorial layer (`catalog.json`): rooms, lineage families, and margin
/// notes, all referencing archive specimens by rkey.
#[derive(Debug, Deserialize)]
pub struct Editorial {
    pub artist: Artist,
    pub origin: Origin,
    pub rooms: Vec<Room>,
    pub families: Vec<Family>,
    #[serde(default)]
    pub margin_notes: HashMap<String, Vec<MarginNote>>,
}

#[derive(Debug)]
pub struct Catalog {
    pub archive: Archive,
    pub editorial: Editorial,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artist {
    pub handle: String,
    pub did: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Room {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub rkeys: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Specimen {
    pub rkey: String,
    pub cid: String,
    pub file: String,
    pub caption: String,
    pub date: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarginNote {
    pub handle: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct Family {
    pub slug: String,
    pub title: String,
    pub rkeys: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Origin {
    pub handle: String,
    pub text: String,
    pub url: String,
}

/// One row of the archive's metadata.jsonl.
#[derive(Debug, Deserialize)]
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

impl Archive {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading archive metadata from {path}"))?;
        let mut specimens: Vec<Specimen> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<MetadataRow>(l).context("parsing metadata.jsonl row"))
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .filter(|row| row.kind == "video")
            .map(|row| Specimen {
                rkey: row.rkey,
                cid: row.cid,
                file: row.file,
                caption: row.caption,
                date: row.created_at.chars().take(10).collect(),
                url: row.url,
            })
            .collect();
        specimens.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.rkey.cmp(&b.rkey)));
        let by_rkey = specimens
            .iter()
            .enumerate()
            .map(|(i, s)| (s.rkey.clone(), i))
            .collect();
        Ok(Self { specimens, by_rkey })
    }

    pub fn get(&self, rkey: &str) -> Option<&Specimen> {
        self.by_rkey.get(rkey).map(|&i| &self.specimens[i])
    }

    pub fn all(&self) -> &[Specimen] {
        &self.specimens
    }

    pub fn len(&self) -> usize {
        self.specimens.len()
    }
}

impl Catalog {
    pub fn load(catalog_path: &str, metadata_path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(catalog_path)
            .with_context(|| format!("reading catalog from {catalog_path}"))?;
        let editorial: Editorial = serde_json::from_str(&raw).context("parsing catalog.json")?;
        let archive = Archive::load(metadata_path)?;
        for room in &editorial.rooms {
            for rkey in &room.rkeys {
                anyhow::ensure!(
                    archive.get(rkey).is_some(),
                    "room {} references rkey {rkey} missing from archive",
                    room.slug
                );
            }
        }
        Ok(Self { archive, editorial })
    }

    pub fn room(&self, slug: &str) -> Option<&Room> {
        self.editorial.rooms.iter().find(|r| r.slug == slug)
    }

    pub fn room_specimens<'a>(&'a self, room: &'a Room) -> impl Iterator<Item = &'a Specimen> {
        room.rkeys.iter().filter_map(|k| self.archive.get(k))
    }

    /// The room a specimen hangs in, if the survey has classified it.
    pub fn room_of(&self, rkey: &str) -> Option<&Room> {
        self.editorial
            .rooms
            .iter()
            .find(|r| r.rkeys.iter().any(|k| k == rkey))
    }

    /// Lineage families this specimen belongs to.
    pub fn families_of(&self, rkey: &str) -> Vec<&Family> {
        self.editorial
            .families
            .iter()
            .filter(|f| f.rkeys.iter().any(|k| k == rkey))
            .collect()
    }

    pub fn notes_of(&self, rkey: &str) -> &[MarginNote] {
        self.editorial
            .margin_notes
            .get(rkey)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

impl Specimen {
    /// Grid label derived from the caption itself — always the artist's own
    /// words, ellipsized at a word boundary when the note runs long.
    pub fn label(&self) -> String {
        let first_line = self.caption.lines().next().unwrap_or("").trim();
        if first_line.is_empty() {
            return format!("Untitled · {}", pretty_date(&self.date));
        }
        const MAX: usize = 48;
        if first_line.chars().count() <= MAX {
            return first_line.to_string();
        }
        let mut label = String::new();
        for word in first_line.split_whitespace() {
            if label.chars().count() + word.chars().count() + 1 > MAX {
                break;
            }
            if !label.is_empty() {
                label.push(' ');
            }
            label.push_str(word);
        }
        label.push('…');
        label
    }

    /// True when the caption is short enough that the label already shows all
    /// of it — the specimen page then skips the redundant full-caption block.
    pub fn label_is_full_caption(&self) -> bool {
        self.caption.trim() == self.label()
    }
}

pub fn pretty_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let mut parts = iso.splitn(3, '-');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return iso.to_string();
    };
    let month = m
        .parse::<usize>()
        .ok()
        .and_then(|m| MONTHS.get(m.wrapping_sub(1)))
        .copied()
        .unwrap_or(m);
    let day = d.trim_start_matches('0');
    format!("{day} {month} {y}")
}

/// "2026-06" → "June 2026", for archive month headings.
pub fn pretty_month(year_month: &str) -> String {
    let mut parts = year_month.splitn(2, '-');
    let (Some(y), Some(m)) = (parts.next(), parts.next()) else {
        return year_month.to_string();
    };
    let pretty = pretty_date(&format!("{y}-{m}-01"));
    pretty
        .split_once(' ')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or(pretty)
}

pub fn roman(n: usize) -> &'static str {
    const NUMERALS: [&str; 12] = [
        "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII",
    ];
    NUMERALS.get(n.wrapping_sub(1)).copied().unwrap_or("—")
}

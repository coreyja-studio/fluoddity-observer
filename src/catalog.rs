use std::collections::HashMap;

use serde::Deserialize;

/// The full expedition record: every video specimen from the artist's
/// archive, chronological. Source of truth for specimen data — every entry
/// gets a durable page whether curated or not.
#[derive(Debug)]
pub struct Archive {
    specimens: Vec<Specimen>,
    by_rkey: HashMap<String, usize>,
}

/// The editorial layer: rooms, lineage families, and margin notes, all
/// referencing archive specimens by rkey.
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
    /// Path relative to the media dir; `None` when the blob hasn't been
    /// pulled locally — such specimens are served from the Bluesky CDN even
    /// in local media mode.
    pub file: Option<String>,
    pub caption: String,
    /// ISO date (YYYY-MM-DD) the post was collected.
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

impl Archive {
    pub fn new(mut specimens: Vec<Specimen>) -> Self {
        specimens.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.rkey.cmp(&b.rkey)));
        let by_rkey = specimens
            .iter()
            .enumerate()
            .map(|(i, s)| (s.rkey.clone(), i))
            .collect();
        Self { specimens, by_rkey }
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

    pub fn is_empty(&self) -> bool {
        self.specimens.is_empty()
    }
}

impl Catalog {
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
        ellipsize(first_line, 48)
    }

    /// True when the caption is short enough that the label already shows all
    /// of it — the specimen page then skips the redundant full-caption block.
    pub fn label_is_full_caption(&self) -> bool {
        self.caption.trim() == self.label()
    }
}

/// Truncate to a word boundary within `max` chars, appending an ellipsis.
pub fn ellipsize(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for word in text.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.push('…');
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn specimen(caption: &str) -> Specimen {
        Specimen {
            rkey: "3mtest".into(),
            cid: "bafytest".into(),
            file: None,
            caption: caption.into(),
            date: "2026-06-04".into(),
            url: "https://example.test".into(),
        }
    }

    #[test]
    fn short_caption_is_the_label() {
        let s = specimen("Jellyfish!");
        assert_eq!(s.label(), "Jellyfish!");
        assert!(s.label_is_full_caption());
    }

    #[test]
    fn long_caption_ellipsizes_at_word_boundary() {
        let s = specimen(
            "Ouroboros tentacle dissolves into bowler hat jellyfish and jumping anemones.",
        );
        let label = s.label();
        assert!(label.ends_with('…'), "label: {label}");
        assert!(label.chars().count() <= 49);
        assert!(!s.label_is_full_caption());
    }

    #[test]
    fn empty_caption_gets_untitled_label() {
        let s = specimen("");
        assert_eq!(s.label(), "Untitled · 4 June 2026");
    }

    #[test]
    fn dates_render_naturalist_style() {
        assert_eq!(pretty_date("2026-06-04"), "4 June 2026");
        assert_eq!(pretty_month("2026-06"), "June 2026");
    }

    #[test]
    fn archive_sorts_and_indexes() {
        let mut a = specimen("a");
        a.rkey = "b-later".into();
        a.date = "2026-06-05".into();
        let mut b = specimen("b");
        b.rkey = "a-earlier".into();
        let archive = Archive::new(vec![a, b]);
        assert_eq!(archive.all()[0].rkey, "a-earlier");
        assert_eq!(archive.get("b-later").unwrap().date, "2026-06-05");
    }
}

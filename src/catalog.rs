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
#[derive(Debug)]
pub struct Editorial {
    pub artist: Artist,
    pub origin: Origin,
    /// Tags per specimen rkey; kind 'lineage' gets evolution-strip treatment.
    pub tags: HashMap<String, Vec<Tag>>,
    pub margin_notes: HashMap<String, Vec<MarginNote>>,
}

#[derive(Debug, Clone)]
pub struct Tag {
    pub tag: String,
    pub kind: String,
    pub source: String,
    /// Who placed it: a curator DID, a suggester's handle (community), or
    /// a marker like 'artist-reply' / 'survey-heuristic'.
    pub added_by: String,
}

impl Tag {
    /// Wall-label provenance, shown on the tag chip.
    pub fn provenance(&self) -> String {
        match self.source.as_str() {
            "post" if self.added_by == "artist-reply" => {
                "the artist's own tag, from a reply".to_string()
            }
            "post" => "the artist's own tag".to_string(),
            "community" => format!("suggested by @{}, taken up by the survey", self.added_by),
            _ if self.added_by == "survey-heuristic" => {
                "a survey guess — corrections welcome".to_string()
            }
            _ => "placed by the survey".to_string(),
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Image,
}

impl MediaKind {
    pub fn from_db(kind: &str) -> Self {
        match kind {
            "image" => Self::Image,
            _ => Self::Video,
        }
    }
}

/// One image of an image specimen (a post carries up to four).
#[derive(Debug, Clone, PartialEq)]
pub struct SpecimenImage {
    pub cid: String,
    /// Path relative to the media dir; `None` when not pulled locally.
    pub file: Option<String>,
    pub alt: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Specimen {
    pub rkey: String,
    pub cid: String,
    pub kind: MediaKind,
    /// Path relative to the media dir; `None` when the blob hasn't been
    /// pulled locally — such specimens are served from the Bluesky CDN even
    /// in local media mode.
    pub file: Option<String>,
    /// Vault key (pds/…) of the PDS original synced to the Bunny CDN;
    /// `None` until pull-media has mirrored it.
    pub pds_key: Option<String>,
    /// Vault key (masters/…) of the render-node master the artist uploaded;
    /// beats the PDS copy wherever full quality is served.
    pub master_key: Option<String>,
    pub caption: String,
    /// ISO date (YYYY-MM-DD) the post was collected.
    pub date: String,
    pub url: String,
    /// All images of an image specimen, in post order; empty for videos.
    pub images: Vec<SpecimenImage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarginNote {
    pub handle: String,
    pub text: String,
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

    /// The chronological neighbors of a specimen: (previous, next).
    pub fn neighbors(&self, rkey: &str) -> (Option<&Specimen>, Option<&Specimen>) {
        let Some(&i) = self.by_rkey.get(rkey) else {
            return (None, None);
        };
        (
            i.checked_sub(1).map(|p| &self.specimens[p]),
            self.specimens.get(i + 1),
        )
    }
}

impl Catalog {
    pub fn tags_of(&self, rkey: &str) -> &[Tag] {
        self.editorial
            .tags
            .get(rkey)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Distinct lineage tags with member counts, ordered by tag.
    pub fn lineage_tags(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for tags in self.editorial.tags.values() {
            for t in tags.iter().filter(|t| t.kind == "lineage") {
                *counts.entry(t.tag.as_str()).or_default() += 1;
            }
        }
        let mut out: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(t, c)| (t.to_string(), c))
            .collect();
        out.sort();
        out
    }

    /// All specimens carrying a tag, in archive (chronological) order.
    pub fn tagged(&self, tag: &str) -> Vec<&Specimen> {
        self.archive
            .all()
            .iter()
            .filter(|s| self.tags_of(&s.rkey).iter().any(|t| t.tag == tag))
            .collect()
    }

    /// The kind of a tag, if any specimen carries it.
    pub fn tag_kind(&self, tag: &str) -> Option<&str> {
        self.editorial
            .tags
            .values()
            .flatten()
            .find(|t| t.tag == tag)
            .map(|t| t.kind.as_str())
    }

    /// Consult the index: every whitespace-separated term must appear in a
    /// specimen's caption, tags, or image alt text (case-insensitive).
    /// Newest sightings first.
    pub fn search(&self, query: &str) -> Vec<&Specimen> {
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        self.archive
            .all()
            .iter()
            .rev()
            .filter(|s| {
                let mut haystack = s.caption.to_lowercase();
                for tag in self.tags_of(&s.rkey) {
                    haystack.push(' ');
                    haystack.push_str(&tag_display(&tag.tag));
                }
                for img in &s.images {
                    haystack.push(' ');
                    haystack.push_str(&img.alt.to_lowercase());
                }
                terms.iter().all(|t| haystack.contains(t.as_str()))
            })
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

/// "the-cortex-line" → "the cortex line", for tag display.
pub fn tag_display(tag: &str) -> String {
    tag.replace('-', " ")
}

/// Normalize free text into a tag slug.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for c in text.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// #hashtags in a caption, slugified — the artist can tag specimens from
/// inside his own posts.
pub fn extract_hashtags(text: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for word in text.split_whitespace() {
        if let Some(raw) = word.strip_prefix('#') {
            let tag = slugify(raw);
            if !tag.is_empty() && !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    tags
}

/// Naturalist's note for the time between two sightings, for lineage
/// charts: "later that day", "the next day", "12 days pass". Empty when
/// either date fails to parse.
pub fn days_between(earlier: &str, later: &str) -> String {
    let (Ok(a), Ok(b)) = (
        chrono::NaiveDate::parse_from_str(earlier, "%Y-%m-%d"),
        chrono::NaiveDate::parse_from_str(later, "%Y-%m-%d"),
    ) else {
        return String::new();
    };
    match (b - a).num_days() {
        i64::MIN..=0 => "later that day".to_string(),
        1 => "the next day".to_string(),
        n => format!("{n} days pass"),
    }
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
            kind: MediaKind::Video,
            file: None,
            pds_key: None,
            master_key: None,
            caption: caption.into(),
            date: "2026-06-04".into(),
            url: "https://example.test".into(),
            images: Vec::new(),
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
    fn hashtags_extract_and_slugify() {
        assert_eq!(
            extract_hashtags("Koosh Rapture #koosh #LivingMetal!"),
            vec!["koosh".to_string(), "livingmetal".to_string()]
        );
        assert!(extract_hashtags("no tags here").is_empty());
        assert_eq!(slugify("The Cortex Line"), "the-cortex-line");
        assert_eq!(tag_display("the-cortex-line"), "the cortex line");
    }

    #[test]
    fn day_gaps_read_like_field_notes() {
        assert_eq!(days_between("2026-06-04", "2026-06-04"), "later that day");
        assert_eq!(days_between("2026-06-04", "2026-06-05"), "the next day");
        assert_eq!(days_between("2026-06-04", "2026-06-16"), "12 days pass");
        assert_eq!(days_between("junk", "2026-06-16"), "");
    }

    #[test]
    fn search_matches_captions_tags_and_alt() {
        let mut jelly = specimen("Bowler hat jellyfish parade");
        jelly.rkey = "3mjelly".into();
        jelly.date = "2026-06-01".into();
        let mut koosh = specimen("Untitled overnight run");
        koosh.rkey = "3mkoosh".into();
        koosh.date = "2026-06-10".into();
        koosh.kind = MediaKind::Image;
        koosh.images = vec![SpecimenImage {
            cid: "bafy-k".into(),
            file: None,
            alt: "a koosh mid-bloom".into(),
        }];
        let mut tags = HashMap::new();
        tags.insert(
            "3mjelly".to_string(),
            vec![Tag {
                tag: "the-jelly-line".into(),
                kind: "lineage".into(),
                source: "curator".into(),
                added_by: "".into(),
            }],
        );
        let catalog = Catalog {
            archive: Archive::new(vec![jelly, koosh]),
            editorial: Editorial {
                artist: Artist {
                    handle: "a".into(),
                    did: "did:plc:a".into(),
                    name: "A".into(),
                },
                origin: Origin {
                    handle: "o".into(),
                    text: "wish".into(),
                    url: "u".into(),
                },
                tags,
                margin_notes: HashMap::new(),
            },
        };

        // Caption terms AND together, case-insensitive.
        let hits = catalog.search("BOWLER jellyfish");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rkey, "3mjelly");
        // Tag words match through tag_display (dashes become spaces).
        assert_eq!(catalog.search("jelly line").len(), 1);
        // Image alt text is searchable.
        assert_eq!(catalog.search("koosh bloom")[0].rkey, "3mkoosh");
        // Newest first, empty query finds nothing.
        assert_eq!(catalog.search("run parade").len(), 0);
        assert!(catalog.search("   ").is_empty());
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

        let (prev, next) = archive.neighbors("a-earlier");
        assert!(prev.is_none());
        assert_eq!(next.unwrap().rkey, "b-later");
        let (prev, next) = archive.neighbors("b-later");
        assert_eq!(prev.unwrap().rkey, "a-earlier");
        assert!(next.is_none());
        assert_eq!(archive.neighbors("missing"), (None, None));
    }
}

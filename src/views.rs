use maud::{DOCTYPE, Markup, html};

use crate::{
    SharedState,
    catalog::{Family, Room, Specimen, pretty_date, pretty_month, roman},
};

const FONTS: &str = "https://fonts.googleapis.com/css2?family=IM+Fell+English:ital@0;1&family=EB+Garamond:ital,wght@0,400;0,500;1,400;1,500&family=Caveat:wght@400;600&display=swap";

fn base(state: &SharedState, title: &str, body: Markup) -> Markup {
    let cdn_mode = state.media_mode == crate::MediaMode::Cdn;
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                meta name="description" content="A field guide to Fluoddity — a universe that only exists inside one GPU. As observed by Oops! All Paperclips.";
                link rel="preconnect" href="https://fonts.googleapis.com";
                link rel="preconnect" href="https://fonts.gstatic.com" crossorigin;
                link rel="stylesheet" href=(FONTS);
                link rel="stylesheet" href="/static/style.css";
                @if cdn_mode {
                    script src="https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js" defer {}
                }
            }
            body {
                (body)
                div #behold aria-hidden="true" {
                    video muted loop playsinline {}
                    p .behold-hint { "tap anywhere to return to the notebook" }
                }
                script src="/static/gallery.js" defer {}
            }
        }
    }
}

/// A looping specimen video. Clicking it opens the full-bleed behold view.
fn specimen_video(state: &SharedState, s: &Specimen) -> Markup {
    let (src, hls, poster) = state.video_sources(s);
    html! {
        video .specimen-video
            src=(src)
            poster=(poster)
            data-hls=[hls]
            muted loop playsinline autoplay
            preload="metadata"
            aria-label=(s.label()) {}
    }
}

fn page_header(state: &SharedState, plate: &str) -> Markup {
    let artist = &state.catalog.editorial.artist;
    html! {
        header .masthead {
            p .plate-no { (plate) }
            h1 { a href="/" { "Fluoddity" } }
            p .subtitle { "a field guide to a universe that only exists inside one GPU" }
            p .observed-by {
                "as observed by "
                a href=(format!("https://bsky.app/profile/{}", artist.handle)) { (artist.name) }
            }
            p .fleuron aria-hidden="true" { "❦ ❦ ❦" }
        }
    }
}

pub fn index(state: &SharedState) -> Markup {
    let catalog = &state.catalog;
    let editorial = &catalog.editorial;
    base(
        state,
        "Fluoddity — a field guide",
        html! {
            main .sheet {
                (page_header(state, "Frontispiece"))

                blockquote .origin-note {
                    p { "“" (editorial.origin.text) "”" }
                    footer {
                        "— the wish that opened this guide, "
                        a href=(editorial.origin.url) { "@" (editorial.origin.handle) }
                    }
                }

                section .contents {
                    h2 .room-label { "Contents · The Rooms" }
                    p .room-sublabel {
                        "arranged by vibe, not by date — a provisional taxonomy, pending the artist's own"
                    }
                    @for (i, room) in editorial.rooms.iter().enumerate() {
                        a .contents-row href=(format!("/room/{}", room.slug)) {
                            div .contents-thumbs {
                                @for s in catalog.room_specimens(room).take(3) {
                                    img src=(state.video_sources(s).2) alt="" loading="lazy";
                                }
                            }
                            div .contents-text {
                                h3 { span .roman { "Plate " (roman(i + 1)) } " — " (room.title) }
                                p { (room.description) }
                                p .count { (room.rkeys.len()) " specimens" }
                            }
                        }
                    }
                }

                section .lineages {
                    h2 .room-label { "The Lineages" }
                    p .room-sublabel {
                        "a specimen is often a family — forms mutate across days, and the guide keeps the descendants together"
                    }
                    ul .lineage-list {
                        @for family in &editorial.families {
                            @if let Some(first) = family.rkeys.first().and_then(|k| catalog.archive.get(k)) {
                                li {
                                    a href=(format!("/specimen/{}", first.rkey)) {
                                        (family.title)
                                        span .count { " · " (family.rkeys.len()) " forms" }
                                    }
                                }
                            }
                        }
                    }
                }

                section .archive-teaser {
                    h2 .room-label { "The Archive" }
                    p .room-sublabel {
                        "the complete expedition record — every specimen has a permanent page here, "
                        "whether or not the survey has hung it in a room"
                    }
                    p .archive-link {
                        a href="/archive" { "browse all " (catalog.archive.len()) " specimens →" }
                    }
                }

                footer .colophon-link {
                    a href="/colophon" { "colophon — why this guide exists" }
                }
            }
        },
    )
}

pub fn room(state: &SharedState, room: &Room) -> Markup {
    let plate_index = state
        .catalog
        .editorial
        .rooms
        .iter()
        .position(|r| r.slug == room.slug)
        .map(|i| i + 1)
        .unwrap_or(0);
    base(
        state,
        &format!("{} — Fluoddity", room.title),
        html! {
            main .sheet {
                (page_header(state, &format!("Plate {}", roman(plate_index))))

                section {
                    h2 .room-label { (room.title) }
                    p .room-sublabel { (room.description) " — as noted by the artist" }

                    div .plate-grid {
                        @for (i, s) in state.catalog.room_specimens(room).enumerate() {
                            figure .specimen {
                                (specimen_video(state, s))
                                figcaption {
                                    p .fig-no { "Fig. " (i + 1) }
                                    p .fig-name {
                                        a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                                    }
                                    p .fig-date { "collected " (pretty_date(&s.date)) }
                                    @if !state.catalog.notes_of(&s.rkey).is_empty()
                                        || !state.catalog.families_of(&s.rkey).is_empty() {
                                        p .fig-more {
                                            a href=(format!("/specimen/{}", s.rkey)) { "field notes →" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                nav .room-nav {
                    a href="/" { "← back to contents" }
                }
            }
        },
    )
}

pub fn archive(state: &SharedState) -> Markup {
    let specimens = state.catalog.archive.all();
    // Group chronologically by month for ledger headings.
    let mut months: Vec<(String, Vec<&Specimen>)> = Vec::new();
    for s in specimens {
        let month = s.date.chars().take(7).collect::<String>();
        match months.last_mut() {
            Some((m, list)) if *m == month => list.push(s),
            _ => months.push((month, vec![s])),
        }
    }
    base(
        state,
        "The Archive — Fluoddity",
        html! {
            main .sheet {
                (page_header(state, "The Archive"))

                section {
                    h2 .room-label { "The Complete Expedition Record" }
                    p .room-sublabel {
                        (specimens.len())
                        " specimens, chronological. Every one has a permanent page — "
                        "the rooms are curation; this is the record."
                    }

                    @for (month, list) in &months {
                        h3 .archive-month { (pretty_month(month)) }
                        div .archive-grid {
                            @for s in list {
                                a .archive-item href=(format!("/specimen/{}", s.rkey)) {
                                    img src=(state.video_sources(s).2) alt=(s.label()) loading="lazy";
                                    span .archive-label { (s.label()) }
                                    span .archive-date { (pretty_date(&s.date)) }
                                }
                            }
                        }
                    }
                }

                nav .room-nav {
                    a href="/" { "← back to contents" }
                }
            }
        },
    )
}

fn family_strip(state: &SharedState, family: &Family, current_rkey: &str) -> Markup {
    html! {
        section .family {
            h3 .family-title { "Lineage · " (family.title) }
            div .family-strip {
                @for (i, rkey) in family.rkeys.iter().enumerate() {
                    @if let Some(member) = state.catalog.archive.get(rkey) {
                        @if i > 0 { span .family-arrow aria-hidden="true" { "⟿" } }
                        a .family-member
                            .current[member.rkey == current_rkey]
                            href=(format!("/specimen/{}", member.rkey)) {
                            img src=(state.video_sources(member).2) alt=(member.label()) loading="lazy";
                            span { (pretty_date(&member.date)) }
                        }
                    }
                }
            }
        }
    }
}

pub fn specimen(state: &SharedState, room: Option<&Room>, s: &Specimen) -> Markup {
    let families = state.catalog.families_of(&s.rkey);
    let notes = state.catalog.notes_of(&s.rkey);
    let plate = room.map(|r| r.title.as_str()).unwrap_or("The Archive");
    base(
        state,
        &format!("{} — Fluoddity", s.label()),
        html! {
            main .sheet .specimen-sheet {
                (page_header(state, plate))

                figure .specimen .specimen-solo {
                    (specimen_video(state, s))
                    figcaption {
                        p .fig-name-big { (s.label()) }
                        p .fig-date { "collected " (pretty_date(&s.date)) }
                    }
                }

                @if !s.label_is_full_caption() && !s.caption.trim().is_empty() {
                    section .field-note {
                        h3 .family-title { "The artist's note" }
                        blockquote .caption-verbatim { (s.caption) }
                    }
                }

                div .wall-label {
                    @if room.is_some() {
                        p .provenance { "selected by the field survey, vol. i" }
                    } @else {
                        p .provenance { "from the expedition record — not yet hung in a room" }
                    }
                    p .source-link {
                        a href=(s.url) { "the original sighting, on Bluesky →" }
                    }
                }

                @if !notes.is_empty() {
                    aside .margin-notes {
                        h3 .family-title { "Noted in the margins" }
                        @for note in notes {
                            p .margin-note {
                                "“" (note.text) "”"
                                span .margin-handle { " — @" (note.handle) }
                            }
                        }
                    }
                }

                @for family in &families {
                    (family_strip(state, family, &s.rkey))
                }

                nav .room-nav {
                    @if let Some(room) = room {
                        a href=(format!("/room/{}", room.slug)) { "← back to " (room.title) }
                    } @else {
                        a href="/archive" { "← back to the archive" }
                    }
                }
            }
        },
    )
}

pub fn colophon(state: &SharedState) -> Markup {
    let editorial = &state.catalog.editorial;
    base(
        state,
        "Colophon — Fluoddity",
        html! {
            main .sheet .colophon {
                (page_header(state, "Colophon"))

                section .prose {
                    h2 .room-label { "Why this guide exists" }
                    p {
                        (editorial.artist.name) " grows things inside a GPU. Millions of particles, "
                        "each with a tiny evolvable brain, leave trails and read the trails of their "
                        "neighbors — and out of that, jellyfish. Party hats. Ouroboros tentacles that "
                        "dissolve into bowler-hat medusae. He calls the system " em { "Fluoddity" } ", "
                        "and he posts his findings to Bluesky like a naturalist filing dispatches from "
                        "an expedition no one else can visit."
                    }
                    p {
                        "Timelines scroll away. Field guides do not. This site is part archive, part "
                        "curation: every specimen in the expedition record has a permanent, linkable "
                        "page — the loop itself, the artist's note preserved verbatim, the date of "
                        "collection — and the rooms hang the survey's selections where the community's "
                        "reactions become notes in the margins."
                    }
                    blockquote .origin-note {
                        p { "“" (editorial.origin.text) "”" }
                        footer {
                            "— "
                            a href=(editorial.origin.url) { "@" (editorial.origin.handle) }
                            ", whose wish this is"
                        }
                    }
                    p {
                        "The rooms are a provisional taxonomy assembled by the field survey; the artist "
                        "has his own, and this guide expects to be corrected. Every caption is his, "
                        "word for word. Curation is meant to be visible — wall labels credit whoever "
                        "selected a piece — and over time the survey hopes to hand rooms to guest "
                        "curators from the sim cluster."
                    }
                    p .source-link {
                        a href=(format!("https://bsky.app/profile/{}", editorial.artist.handle)) {
                            "Follow the expedition: @" (editorial.artist.handle) " →"
                        }
                    }
                }

                nav .room-nav {
                    a href="/" { "← back to contents" }
                }
            }
        },
    )
}

pub fn not_found() -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Not found — Fluoddity" }
                link rel="stylesheet" href="/static/style.css";
            }
            body {
                main .sheet .colophon {
                    h1 { "Specimen not found" }
                    p { "This page has wandered off the plate. " a href="/" { "Back to the guide." } }
                }
            }
        }
    }
}

use maud::{DOCTYPE, Markup, html};

use crate::{
    Ctx, HungRoom,
    auth::Curator,
    catalog::{Specimen, pretty_date, pretty_month, roman, tag_display},
    threads::ThreadRoom,
};

const FONTS: &str = "https://fonts.googleapis.com/css2?family=IM+Fell+English:ital@0;1&family=EB+Garamond:ital,wght@0,400;0,500;1,400;1,500&family=Caveat:wght@400;600&display=swap";

fn base(title: &str, body: Markup) -> Markup {
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
                // Freshly ingested specimens stream from the Bluesky CDN via
                // HLS even in local media mode, so hls.js is always on hand.
                script src="https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js" defer {}
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
fn specimen_video(ctx: &Ctx, s: &Specimen) -> Markup {
    let (src, hls, poster) = ctx.video_sources(s);
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

fn page_header(ctx: &Ctx, plate: &str) -> Markup {
    let artist = &ctx.catalog.editorial.artist;
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

pub fn index(ctx: &Ctx, rooms: &[HungRoom]) -> Markup {
    let catalog = &ctx.catalog;
    let editorial = &catalog.editorial;
    base(
        "Fluoddity — a field guide",
        html! {
            main .sheet {
                (page_header(ctx, "Frontispiece"))

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
                        "every room is one of the artist's own Bluesky threads, rendered live — "
                        "when a thread grows, its room grows with it"
                    }
                    @let artist_rooms: Vec<&HungRoom> = rooms.iter().filter(|h| h.is_by(&editorial.artist.did)).collect();
                    @if artist_rooms.is_empty() {
                        p .room-sublabel { "no rooms hung yet — the archive below holds everything meanwhile" }
                    }
                    @for (i, hung) in artist_rooms.iter().enumerate() {
                        a .contents-row href=(format!("/room/{}/{}", hung.row.author_handle, hung.row.rkey)) {
                            div .contents-thumbs {
                                @for s in hung.room.entries.iter().filter_map(|e| catalog.archive.get(&e.specimen_rkey)).take(3) {
                                    img src=(ctx.video_sources(s).2) alt="" loading="lazy";
                                }
                            }
                            div .contents-text {
                                h3 { span .roman { "Plate " (roman(i + 1)) } " — " (hung.room.title) }
                                p .count { (hung.room.entries.len()) " specimens" }
                            }
                        }
                    }
                }

                @let lineages = catalog.lineage_tags();
                @if !lineages.is_empty() {
                    section .lineages {
                        h2 .room-label { "The Lineages" }
                        p .room-sublabel {
                            "a specimen is often a family — forms mutate across days, and the guide keeps the descendants together"
                        }
                        ul .lineage-list {
                            @for (tag, count) in &lineages {
                                li {
                                    a href=(format!("/tag/{tag}")) {
                                        (tag_display(tag))
                                        span .count { " · " (count) " forms" }
                                    }
                                }
                            }
                        }
                    }
                }

                @let guest_rooms: Vec<&HungRoom> = rooms.iter().filter(|h| !h.is_by(&editorial.artist.did)).collect();
                @if !guest_rooms.is_empty() {
                    section .guest-rooms {
                        h2 .room-label { "Guest Rooms" }
                        p .room-sublabel {
                            "rooms curated by others — post a thread quoting the artist's work "
                            "and it can hang here"
                        }
                        ul .lineage-list {
                            @for hung in &guest_rooms {
                                li {
                                    a href=(format!("/room/{}/{}", hung.row.author_handle, hung.row.rkey)) {
                                        (hung.room.title)
                                    }
                                    span .count { " · hung by @" (hung.row.author_handle) }
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
                    div .latest-strip {
                        @for s in catalog.archive.all().iter().rev().take(6) {
                            a .latest-item href=(format!("/specimen/{}", s.rkey)) title=(s.label()) {
                                img src=(ctx.video_sources(s).2) alt=(s.label()) loading="lazy";
                            }
                        }
                    }
                    p .latest-caption { "the six most recent sightings" }
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

pub fn archive(ctx: &Ctx) -> Markup {
    let specimens = ctx.catalog.archive.all();
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
        "The Archive — Fluoddity",
        html! {
            main .sheet {
                (page_header(ctx, "The Archive"))

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
                                    img src=(ctx.video_sources(s).2) alt=(s.label()) loading="lazy";
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

fn lineage_strip(ctx: &Ctx, tag: &str, current_rkey: &str) -> Markup {
    let members = ctx.catalog.tagged(tag);
    html! {
        section .family {
            h3 .family-title {
                "Lineage · " a href=(format!("/tag/{tag}")) { (tag_display(tag)) }
            }
            div .family-strip {
                @for (i, member) in members.iter().enumerate() {
                    @if i > 0 { span .family-arrow aria-hidden="true" { "⟿" } }
                    a .family-member
                        .current[member.rkey == current_rkey]
                        href=(format!("/specimen/{}", member.rkey)) {
                        img src=(ctx.video_sources(member).2) alt=(member.label()) loading="lazy";
                        span { (pretty_date(&member.date)) }
                    }
                }
            }
        }
    }
}

pub fn specimen(
    ctx: &Ctx,
    hung_in: &[&HungRoom],
    s: &Specimen,
    curator: Option<&Curator>,
) -> Markup {
    let tags = ctx.catalog.tags_of(&s.rkey);
    let notes = ctx.catalog.notes_of(&s.rkey);
    let artist_did = ctx.catalog.editorial.artist.did.clone();
    let plate = hung_in
        .first()
        .map(|h| h.room.title.as_str())
        .unwrap_or("The Archive");
    base(
        &format!("{} — Fluoddity", s.label()),
        html! {
            main .sheet .specimen-sheet {
                (page_header(ctx, plate))

                figure .specimen .specimen-solo {
                    (specimen_video(ctx, s))
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
                    @if hung_in.is_empty() {
                        p .provenance { "from the expedition record — not yet hung in a room" }
                    } @else {
                        @for h in hung_in {
                            p .provenance {
                                "hung in "
                                a href=(format!("/room/{}/{}", h.row.author_handle, h.row.rkey)) { (h.room.title) }
                                @if h.is_by(&artist_did) { " by the artist" }
                                @else { " by @" (h.row.author_handle) }
                            }
                        }
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

                @for tag in tags.iter().filter(|t| t.kind == "lineage") {
                    (lineage_strip(ctx, &tag.tag, &s.rkey))
                }

                div .tag-row {
                    @for tag in tags {
                        span .tag-chip {
                            a href=(format!("/tag/{}", tag.tag)) {
                                @if tag.kind == "lineage" { "⟿ " }
                                (tag_display(&tag.tag))
                            }
                            @if curator.is_some() {
                                form method="post" action="/admin/tags/remove" .inline-form {
                                    input type="hidden" name="rkey" value=(s.rkey);
                                    input type="hidden" name="tag" value=(tag.tag);
                                    button type="submit" .link-button title="remove tag" { "✕" }
                                }
                            }
                        }
                    }
                    @if curator.is_some() {
                        form method="post" action="/admin/tags/add" .inline-form .tag-add {
                            input type="hidden" name="rkey" value=(s.rkey);
                            input type="text" name="tag" placeholder="add tag" required;
                            select name="kind" {
                                option value="tag" { "tag" }
                                option value="lineage" { "lineage" }
                            }
                            button type="submit" { "tag it" }
                        }
                    }
                }

                nav .room-nav {
                    @if let Some(h) = hung_in.first() {
                        a href=(format!("/room/{}/{}", h.row.author_handle, h.row.rkey)) {
                            "← back to " (h.room.title)
                        }
                    } @else {
                        a href="/archive" { "← back to the archive" }
                    }
                }
            }
        },
    )
}

pub fn thread_room(ctx: &Ctx, room: &ThreadRoom, plate: Option<usize>) -> Markup {
    base(
        &format!("{} — Fluoddity", room.title),
        html! {
            main .sheet {
                @let plate_label = match plate {
                    Some(n) => format!("Plate {}", roman(n)),
                    None => "Guest Room".to_string(),
                };
                (page_header(ctx, &plate_label))

                section {
                    h2 .room-label { (room.title) }
                    p .room-sublabel {
                        "a room curated by "
                        a href=(format!("https://bsky.app/profile/{}", room.author_handle)) {
                            (room.author_display)
                        }
                        " — it renders live from "
                        a href=(room.thread_url()) { "their thread" }
                        "; when the thread grows, so does the room"
                    }

                    @if !room.intro.trim().is_empty() {
                        blockquote .origin-note {
                            p { "“" (room.intro) "”" }
                            footer { "— @" (room.author_handle) ", at the door" }
                        }
                    }

                    div .plate-grid {
                        @for (i, entry) in room
                            .entries
                            .iter()
                            .filter_map(|e| ctx.catalog.archive.get(&e.specimen_rkey).map(|s| (e, s)))
                            .enumerate()
                        {
                            @let (entry, s) = entry;
                            figure .specimen {
                                (specimen_video(ctx, s))
                                figcaption {
                                    p .fig-no { "Fig. " (i + 1) }
                                    p .fig-name {
                                        a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                                    }
                                    p .fig-date { "collected " (pretty_date(&s.date)) }
                                    @if entry.note.trim() != s.caption.trim() && !entry.note.trim().is_empty() {
                                        p .curator-note {
                                            "“" (entry.note) "”"
                                            span .margin-handle { " — @" (room.author_handle) }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    @if room.entries.iter().all(|e| ctx.catalog.archive.get(&e.specimen_rkey).is_none()) {
                        p .room-sublabel {
                            "This thread doesn't reference any of the artist's specimens yet. "
                            "Quote-post or link his work in the thread and it will hang here."
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

pub fn tag_page(ctx: &Ctx, tag: &str, kind: &str) -> Markup {
    let members = ctx.catalog.tagged(tag);
    let heading = if kind == "lineage" {
        format!("Lineage · {}", tag_display(tag))
    } else {
        format!("Tagged · {}", tag_display(tag))
    };
    base(
        &format!("{} — Fluoddity", tag_display(tag)),
        html! {
            main .sheet {
                (page_header(ctx, &heading))

                section {
                    h2 .room-label { (tag_display(tag)) }
                    p .room-sublabel {
                        @if kind == "lineage" {
                            (members.len()) " forms, oldest first — watch it evolve"
                        } @else {
                            (members.len()) " specimens carry this tag"
                        }
                    }

                    div .plate-grid {
                        @for (i, s) in members.iter().enumerate() {
                            figure .specimen {
                                (specimen_video(ctx, s))
                                figcaption {
                                    p .fig-no { "Fig. " (i + 1) }
                                    p .fig-name {
                                        a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                                    }
                                    p .fig-date { "collected " (pretty_date(&s.date)) }
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

pub fn colophon(ctx: &Ctx) -> Markup {
    let editorial = &ctx.catalog.editorial;
    base(
        "Colophon — Fluoddity",
        html! {
            main .sheet .colophon {
                (page_header(ctx, "Colophon"))

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

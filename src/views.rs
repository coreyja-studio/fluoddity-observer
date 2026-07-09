use maud::{DOCTYPE, Markup, html};

use crate::{
    Ctx, HungRoom,
    auth::Curator,
    catalog::{MediaKind, Specimen, pretty_date, pretty_month, roman, tag_display},
    threads::ThreadRoom,
};

const FONTS: &str = "https://fonts.googleapis.com/css2?family=IM+Fell+English:ital@0;1&family=EB+Garamond:ital,wght@0,400;0,500;1,400;1,500&family=Caveat:wght@400;600&display=swap";

/// Per-page metadata for the head: title, description, and the OpenGraph
/// card that unfurls when a page is shared (on Bluesky, most importantly).
pub struct PageMeta {
    pub title: String,
    pub description: String,
    /// Absolute image URL — CDN thumbnails work in every media mode.
    pub image: Option<String>,
    /// Site-relative path, joined with PCG_PUBLIC_URL for og:url.
    pub path: String,
}

const DEFAULT_DESCRIPTION: &str = "A field guide to Fluoddity — a universe that only exists inside one GPU. As observed by Oops! All Paperclips.";

impl PageMeta {
    fn new(title: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: DEFAULT_DESCRIPTION.to_string(),
            image: None,
            path: path.into(),
        }
    }
}

/// Absolute thumbnail for a specimen, for OG cards. Always a CDN URL —
/// cards must resolve from anywhere, whatever the media mode.
fn og_thumb(ctx: &Ctx, s: &Specimen) -> String {
    let did = &ctx.catalog.editorial.artist.did;
    match s.kind {
        MediaKind::Video => format!("https://video.bsky.app/watch/{did}/{}/thumbnail.jpg", s.cid),
        MediaKind::Image => format!(
            "https://cdn.bsky.app/img/feed_fullsize/plain/{did}/{}@jpeg",
            s.cid
        ),
    }
}

fn base(meta: PageMeta, body: Markup) -> Markup {
    let og_url = format!("{}{}", crate::bot::public_url(), meta.path);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (meta.title) }
                meta name="description" content=(meta.description);
                meta property="og:site_name" content="Fluoddity — a field guide";
                meta property="og:type" content="website";
                meta property="og:title" content=(meta.title);
                meta property="og:description" content=(meta.description);
                meta property="og:url" content=(og_url);
                @if let Some(image) = &meta.image {
                    meta property="og:image" content=(image);
                    meta name="twitter:card" content="summary_large_image";
                    meta name="twitter:image" content=(image);
                }
                meta name="twitter:title" content=(meta.title);
                meta name="twitter:description" content=(meta.description);
                link rel="alternate" type="application/atom+xml" title="Fluoddity — dispatches from the expedition" href="/feed.xml";
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
                    video muted loop playsinline controls {}
                    img alt="";
                    p .behold-hint { "tap outside the specimen to return to the notebook" }
                }
                script src="/static/gallery.js" defer {}
            }
        }
    }
}

/// A specimen's media: a looping video, or the still image(s) of an image
/// post. Clicking either opens the full-bleed behold view.
fn specimen_media(ctx: &Ctx, s: &Specimen) -> Markup {
    specimen_media_tiered(ctx, s, false)
}

/// `full` swaps the grid-friendly source for the vault's archival copy —
/// the solo specimen page earns the full-rate file up front; a grid of a
/// dozen autoplaying loops stays on the Bluesky CDN and only behold
/// (via data-full) trades up. Solo videos also get native controls
/// (pause/scrub/fullscreen); grid loops stay bare and open behold on tap.
fn specimen_media_tiered(ctx: &Ctx, s: &Specimen, full: bool) -> Markup {
    match s.kind {
        MediaKind::Video => {
            let (src, hls, poster) = ctx.video_sources(s);
            let full_src = ctx.full_video_src(s);
            let (src, hls) = match (full, full_src.clone()) {
                (true, Some(full_src)) => (full_src, None),
                _ => (src, hls),
            };
            html! {
                video .specimen-video
                    src=(src)
                    poster=(poster)
                    data-hls=[hls]
                    data-full=[full_src]
                    muted loop playsinline autoplay
                    controls[full]
                    preload="metadata"
                    aria-label=(s.label()) {}
            }
        }
        MediaKind::Image => html! {
            div .specimen-stills .multi[s.images.len() > 1] {
                @for img in &s.images {
                    img .specimen-image
                        src=(ctx.image_src(img))
                        alt=(if img.alt.is_empty() { s.label() } else { img.alt.clone() })
                        loading="lazy";
                }
            }
        },
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
    let mut meta = PageMeta::new("Fluoddity — a field guide", "/");
    meta.image = catalog.archive.all().last().map(|s| og_thumb(ctx, s));
    base(
        meta,
        html! {
            main .sheet {
                (page_header(ctx, "Frontispiece"))

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
                                    img src=(ctx.thumb(s)) alt="" loading="lazy";
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
                                img src=(ctx.thumb(s)) alt=(s.label()) loading="lazy";
                            }
                        }
                    }
                    p .latest-caption { "the six most recent sightings" }
                    p .archive-link {
                        a href="/archive" { "browse all " (catalog.archive.len()) " specimens →" }
                        " · "
                        a href="/search" { "consult the index" }
                    }
                    p .ambient-link {
                        a href="/ambient" { "or enter ambient mode — lights off, let the collection play ✦" }
                    }
                    p .feed-link {
                        a href="/feed.xml" { "dispatches by feed (Atom)" }
                        " — new sightings, wherever you read"
                    }
                }

                footer .colophon-link {
                    a href="/colophon" { "colophon — why this guide exists, and whose wish began it" }
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
    let mut meta = PageMeta::new("The Archive — Fluoddity", "/archive");
    meta.description = format!(
        "The complete expedition record — {} specimens, chronological, every one with a permanent page.",
        specimens.len()
    );
    meta.image = specimens.last().map(|s| og_thumb(ctx, s));
    base(
        meta,
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
                    (index_search_form(""))

                    @let flat = ctx.catalog.tagged("2d").len();
                    @let deep = ctx.catalog.tagged("3d").len();
                    @if flat > 0 && deep > 0 {
                        p .dimension-filter {
                            "the record in two dimensions: "
                            a href="/tag/2d" { "flatland (" (flat) ")" }
                            " · "
                            a href="/tag/3d" { "the depths (" (deep) ")" }
                        }
                    }

                    @for (month, list) in &months {
                        h3 .archive-month { (pretty_month(month)) }
                        div .archive-grid {
                            @for s in list {
                                a .archive-item href=(format!("/specimen/{}", s.rkey)) {
                                    img src=(ctx.thumb(s)) alt=(s.label()) loading="lazy";
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
                        img src=(ctx.thumb(member)) alt=(member.label()) loading="lazy";
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
    let mut meta = PageMeta::new(
        format!("{} — Fluoddity", s.label()),
        format!("/specimen/{}", s.rkey),
    );
    meta.description = format!(
        "“{}” — collected {}. From the Fluoddity expedition record.",
        crate::catalog::ellipsize(&s.caption.replace('\n', " "), 180),
        pretty_date(&s.date),
    );
    meta.image = Some(og_thumb(ctx, s));
    base(
        meta,
        html! {
            main .sheet .specimen-sheet {
                (page_header(ctx, plate))

                figure .specimen .specimen-solo {
                    (specimen_media_tiered(ctx, s, true))
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
                        span .tag-chip title=(tag.provenance()) {
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
                    @if curator.is_none() {
                        span .tag-invite {
                            a href=(s.url) { "know this form? reply with a #hashtag" }
                            " — the survey collects suggestions daily"
                        }
                    }
                    @if curator.is_some() {
                        form method="post" action="/admin/tags/add" .inline-form .tag-add {
                            input type="hidden" name="rkey" value=(s.rkey);
                            input type="text" name="tag" placeholder="add tags (comma-separated)" required;
                            select name="kind" {
                                option value="tag" { "tag" }
                                option value="lineage" { "lineage" }
                            }
                            button type="submit" { "tag it" }
                        }
                        form method="post" action="/admin/specimens/remove" .inline-form {
                            input type="hidden" name="rkey" value=(s.rkey);
                            button type="submit" .link-button title="remove from gallery" { "remove from gallery" }
                        }
                    }
                }

                @let (prev, next) = ctx.catalog.archive.neighbors(&s.rkey);
                nav .room-nav .page-turn {
                    @if let Some(p) = prev {
                        a .turn-prev href=(format!("/specimen/{}", p.rkey)) title=(p.label()) {
                            "← previous sighting"
                        }
                    } @else { span {} }
                    @if let Some(h) = hung_in.first() {
                        a href=(format!("/room/{}/{}", h.row.author_handle, h.row.rkey)) {
                            "back to " (h.room.title)
                        }
                    } @else {
                        a href="/archive" { "the archive" }
                    }
                    @if let Some(n) = next {
                        a .turn-next href=(format!("/specimen/{}", n.rkey)) title=(n.label()) {
                            "next sighting →"
                        }
                    } @else { span {} }
                }
            }
        },
    )
}

pub fn thread_room(ctx: &Ctx, room: &ThreadRoom, plate: Option<usize>) -> Markup {
    let mut meta = PageMeta::new(
        format!("{} — Fluoddity", room.title),
        format!("/room/{}/{}", room.author_handle, room.rkey),
    );
    meta.description = format!(
        "A room curated by {} — {} specimens, rendered live from their Bluesky thread.",
        room.author_display,
        room.entries.len(),
    );
    meta.image = room
        .entries
        .iter()
        .find_map(|e| ctx.catalog.archive.get(&e.specimen_rkey))
        .map(|s| og_thumb(ctx, s));
    base(
        meta,
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

                    div .thread-flow {
                        @for (i, entry) in room
                            .entries
                            .iter()
                            .filter_map(|e| ctx.catalog.archive.get(&e.specimen_rkey).map(|s| (e, s)))
                            .enumerate()
                        {
                            @let (entry, s) = entry;
                            figure .specimen .thread-entry {
                                (specimen_media(ctx, s))
                                figcaption {
                                    p .fig-no { "Fig. " (i + 1) }
                                    @if !entry.note.trim().is_empty() {
                                        p .thread-note { (entry.note) }
                                    } @else {
                                        p .fig-name {
                                            a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                                        }
                                    }
                                    p .fig-date {
                                        "collected " (pretty_date(&s.date))
                                        " · "
                                        a href=(format!("/specimen/{}", s.rkey)) { "field notes →" }
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
                    a href=(format!("/ambient?room={}/{}", room.author_handle, room.rkey)) {
                        "ambient this room ✦"
                    }
                    " · "
                    a href="/" { "← back to contents" }
                }
            }
        },
    )
}

/// The index consultation form, shared by the pages that offer it.
fn index_search_form(query: &str) -> Markup {
    html! {
        form .index-search action="/search" method="get" {
            input type="search" name="q" value=(query)
                placeholder="consult the index — jellyfish, koosh, living metal…"
                aria-label="search the archive";
            button type="submit" { "consult" }
        }
    }
}

pub fn search(ctx: &Ctx, query: &str) -> Markup {
    let hits = ctx.catalog.search(query);
    let mut meta = PageMeta::new("The Index — Fluoddity", "/search");
    meta.description =
        "Consult the index — search every caption, tag, and note in the expedition record."
            .to_string();
    base(
        meta,
        html! {
            main .sheet {
                (page_header(ctx, "The Index"))

                section {
                    h2 .room-label { "Consult the Index" }
                    p .room-sublabel {
                        "every caption is searchable — the artist's words are half the record"
                    }
                    (index_search_form(query))

                    @if !query.trim().is_empty() {
                        @if hits.is_empty() {
                            p .room-sublabel .index-no-hits {
                                "no specimen answers to “" (query) "” — "
                                "the survey's vocabulary is the artist's own; try his words"
                            }
                        } @else {
                            p .index-count {
                                (hits.len())
                                @if hits.len() == 1 { " specimen answers" } @else { " specimens answer" }
                                " to “" (query) "”"
                            }
                            div .archive-grid {
                                @for s in &hits {
                                    a .archive-item href=(format!("/specimen/{}", s.rkey)) {
                                        img src=(ctx.thumb(s)) alt=(s.label()) loading="lazy";
                                        span .archive-label { (s.label()) }
                                        span .archive-date { (pretty_date(&s.date)) }
                                    }
                                }
                            }
                        }
                    }
                }

                nav .room-nav {
                    a href="/archive" { "← the full record" }
                    " · "
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
    let mut meta = PageMeta::new(
        format!("{} — Fluoddity", tag_display(tag)),
        format!("/tag/{tag}"),
    );
    meta.description = if kind == "lineage" {
        format!(
            "Lineage · {} — {} forms, oldest first. Watch it evolve.",
            tag_display(tag),
            members.len()
        )
    } else {
        format!("{} specimens tagged “{}”.", members.len(), tag_display(tag))
    };
    meta.image = members.first().map(|s| og_thumb(ctx, s));
    base(
        meta,
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

                    @if kind == "lineage" {
                        // The evolution chart: one descent line, oldest at
                        // the top, the time between sightings written on
                        // the stem the way a field notebook would.
                        div .lineage-chart {
                            @for (i, s) in members.iter().enumerate() {
                                @if i > 0 {
                                    p .lineage-gap {
                                        (crate::catalog::days_between(&members[i - 1].date, &s.date))
                                    }
                                }
                                figure .specimen .lineage-node {
                                    (specimen_media(ctx, s))
                                    figcaption {
                                        p .fig-no { "Form " (i + 1) }
                                        p .fig-name {
                                            a href=(format!("/specimen/{}", s.rkey)) { (s.label()) }
                                        }
                                        p .fig-date { "collected " (pretty_date(&s.date)) }
                                    }
                                }
                            }
                        }
                    } @else {
                        div .plate-grid {
                            @for (i, s) in members.iter().enumerate() {
                                figure .specimen {
                                    (specimen_media(ctx, s))
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
                }

                nav .room-nav {
                    a href=(format!("/ambient?tag={tag}")) { "ambient — let it play ✦" }
                    " · "
                    a href="/" { "← back to contents" }
                }
            }
        },
    )
}

/// One entry in the ambient playlist, serialized for the client.
#[derive(serde::Serialize)]
pub struct AmbientEntry {
    /// "video" or "image".
    pub kind: &'static str,
    pub src: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hls: Option<String>,
    pub poster: String,
    pub label: String,
}

pub fn ambient_entry(ctx: &Ctx, s: &Specimen) -> AmbientEntry {
    match s.kind {
        MediaKind::Video => {
            let (src, hls, poster) = ctx.video_sources(s);
            // The exhibition shows one loop at a time, full-bleed — serve
            // the vault's archival copy when we hold one.
            let (src, hls) = match ctx.full_video_src(s) {
                Some(full) => (full, None),
                None => (src, hls),
            };
            AmbientEntry {
                kind: "video",
                src,
                hls,
                poster,
                label: s.label(),
            }
        }
        MediaKind::Image => AmbientEntry {
            kind: "image",
            src: s
                .images
                .first()
                .map(|img| ctx.image_src(img))
                .unwrap_or_default(),
            hls: None,
            poster: String::new(),
            label: s.label(),
        },
    }
}

/// The archive as a slow, endless exhibition: full-bleed dark, crossfading
/// loops, a label that breathes. Esc leaves; space skips.
pub fn ambient(title: &str, entries: &[AmbientEntry]) -> Markup {
    let playlist = serde_json::to_string(entries).unwrap_or_else(|_| "[]".to_string());
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · ambient — Fluoddity" }
                link rel="stylesheet" href=(FONTS);
                link rel="stylesheet" href="/static/style.css";
                script src="https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js" defer {}
            }
            body .ambient-body {
                div #ambient-stage {
                    div .layer {
                        video muted playsinline preload="auto" {}
                        img alt="";
                    }
                    div .layer {
                        video muted playsinline preload="auto" {}
                        img alt="";
                    }
                }
                p #ambient-label {}
                button #ambient-exit title="leave the exhibition (Esc)" { "✕" }
                script #ambient-data type="application/json" { (maud::PreEscaped(playlist)) }
                script src="/static/ambient.js" defer {}
            }
        }
    }
}

pub fn colophon(ctx: &Ctx) -> Markup {
    let editorial = &ctx.catalog.editorial;
    base(
        PageMeta::new("Colophon — Fluoddity", "/colophon"),
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

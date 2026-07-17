# Architecture

One Rust binary (Axum + Maud + SQLx/Postgres), server-rendered, no client
framework. This document is the map; the README explains the concept and
CLAUDE.md lists the operational gotchas.

## The shape of the data

**Postgres is the source of truth.** `catalog.json` and `metadata.jsonl` are
the flat-file era: `cargo run -- import` seeds the database from them
(idempotently) and everything since — live ingest, tags, rooms, suggestions,
margin notes — accumulates in Postgres. Editing `catalog.json` does nothing
to a running instance.

Core tables (see `migrations/` for the full history):

- **specimens / specimen_images** — the expedition record: one row per post
  in the artist's archive, video or still, with caption preserved verbatim.
  `rkey` (the Bluesky record key) is the durable public identifier —
  `/specimen/{rkey}`.
- **thread_rooms** — registered rooms: pointers to Bluesky threads
  (author + rkey), numbered for the homepage. Artist threads are plates;
  others are guest rooms.
- **tags / tag suggestions** — the taxonomy. Artist hashtags apply
  automatically; community hashtags file as suggestions for the curator's
  desk, with attribution kept for the wall label.
- **margin_notes** — quote-posts of specimens, refreshed daily, rendered in
  the page margins.
- **bot state + weekly wrap-up** — replied-mention bookkeeping and the weekly
  posting ledger.
- **media vault / og posters** — cold-stored original blobs and generated
  OpenGraph cards.
- **cja jobs** — the durable job queue (see below).

## Request path

Routes live in `main.rs`; rendering in `views.rs` (Maud). Public pages:

- `/` — the frontispiece: registered rooms, numbered like engravings
- `/archive` — the complete record, chronological, filterable 2D/3D
- `/specimen/{rkey}` — one specimen: loop, verbatim caption, margin notes,
  lineage, page-turn to previous/next sighting
- `/room/{author}/{rkey}` — ANY Bluesky thread rendered as a room
  (permissionless by design; the homepage registry is the curated part),
  five-minute cache
- `/search`, `/tag/{tag}`, `/ambient`, `/feed.xml`, `/colophon`

`/admin` is the curator's desk: Bluesky OAuth for identity only (`auth.rs` —
prove the DID, check the roster, mint our own cookie, discard the atproto
tokens). Curators register rooms, tag specimens, and work the suggestion box.

## Media

`PCG_MEDIA_MODE` picks the baseline: `local` serves files from
`PCG_MEDIA_DIR`; `cdn` (production) hotlinks the Bluesky CDNs — HLS video
via `video.bsky.app` playlists (hls.js outside Safari), stills from
`cdn.bsky.app`.

### The vault

Bluesky's CDN recompresses aggressively, so the museum keeps its own copies
(`storage.rs`): a Bunny Storage zone holding three key families —

- **`pds/{archive filename}`** — full-rate originals pulled from the
  artist's PDS. `cargo run -- pull-media` syncs any specimen the vault is
  missing and records the key on the specimen row.
- **`masters/{rkey}.{ext}`** — source renders uploaded by the artist
  (or a curator on his behalf) at `/admin/vault`. That page is the
  *desiderata list*: every video specimen the vault holds no source
  render for, each with an upload slot. One key per specimen; re-upload
  replaces. (The storage prefix and `master_key` column keep the old
  name — renaming stored keys isn't worth the migration.)
  Uploads are curator-gated, capped at 2 GiB, and whitelisted to
  mp4/mov/webm — Bunny types objects by file extension, so only extensions
  browsers can play get in.
- **`og/{rkey}.jpg`** — generated 1200×630 OpenGraph posters (mid-clip
  ffmpeg frame), produced by the `gen-posters` subcommand.

A Bunny pull zone in front serves the vault at `PCG_MEDIA_BASE_URL`.
**Serving preference per specimen: source render → PDS original → Bluesky CDN.**
The archive grid keeps the CDN's grid-friendly encodes; the specimen page
and behold mode swap in the vault's archival copy when one is held, and OG
cards prefer the own-domain vault poster.

Config: `PCG_BUNNY_STORAGE_ZONE` + `PCG_BUNNY_STORAGE_KEY` (the storage
zone's read-write password — not the account API key), optional
`PCG_BUNNY_STORAGE_ENDPOINT` for non-default regions. Unset disables the
vault entirely and everything falls back to the Bluesky CDNs.

The takedown story is unchanged in spirit: vault contents are the museum's
own preservation copies of work the artist published, held with his
blessing, and deleted on his word like everything else.

## Background work

`cron.rs` registers tickers on the cja cron worker; each tick enqueues a
durable job (`jobs.rs`) with retries and a dead-letter queue, so a slow poll
can never wedge the schedule:

| job | cadence | what it does |
|---|---|---|
| `IngestPoll` | `PCG_POLL_SECS` (300s) | poll the artist's feed for new specimens |
| `RefreshMarginNotes` | daily | re-pull quote-posts so the margins stay current |
| `HarvestSuggestions` | daily | sweep community hashtags into the suggestion box |
| `ProcessMentions` | `PCG_BOT_POLL_SECS` (60s) | answer @-mentions with room links / tag acks (`bot.rs`) |
| `WeeklyWrapup` | hourly check | post the week's most-liked specimens (≤3; quiet weeks post nothing) |

The bot polls `listNotifications` rather than tailing Jetstream — Jetstream
cannot filter by mention. Bot crons only register when `PCG_BOT_HANDLE` /
`PCG_BOT_PASSWORD` are set.

## Trust boundaries

- **Thread rendering is zero-trust** (`threads.rs`): a room shows other
  people's *words*, but only specimens by the artist's DID ever render as
  media. A thread cannot inject foreign images into the gallery.
- **Artist hashtags are law** — tags in the artist's captions, or in replies
  to his own posts, apply with no approval step. Everyone else's suggestions
  wait for a curator.
- **OAuth never acts on anyone's PDS** — identity proof only, tokens
  discarded on arrival.

## Deploy

Fly.io app `paperclips-gallery` (ewr) + Fly Postgres; pushes to `main`
deploy production via GitHub Actions. CI: fmt, clippy (`-D warnings`),
SQLx prepare-check and tests against a Postgres service, cargo-deny,
conventional-commit titles.

# Fluoddity — a field guide

> A naturalist's field guide to a universe that only exists inside one GPU.

A gallery for the GPU-raytraced particle simulations of
[Oops! All Paperclips](https://bsky.app/profile/all-paperclips.bsky.social)
(the system is his — he calls it *Fluoddity*). Posts scroll away; field guides
don't. Each specimen gets a durable page: the loop, the artist's caption
verbatim, the collection date, real community reactions as margin notes, and
its mutation lineage.

Born from [a wish by @norvid-studies](https://bsky.app/profile/norvid-studies.bsky.social/post/3mpli4fvzns22).

## Design

- **Notebook register** — warm engraved-plate pages (Haeckel's *Art Forms in
  Nature* is the north star): rooms are plates, specimens are figures.
- **Behold register** — click any specimen and the notebook falls away;
  full-bleed loop glowing in the dark. `Esc` or tap to return.
- **Rooms by vibe, not date** — a provisional taxonomy assembled from the
  artist's own vocabulary, pending his real one.
- **Lineages** — caption-backed mutation families (the Cortex Line, the
  Ouroboros Tentacle…) rendered as strips of descendants.
- **Captions verbatim, always.** The only editorial voice is the community's:
  margin notes are real Bluesky quote-posts, attributed.

## Running

```bash
createdb paperclips_gallery
sqlx migrate run          # or just start the app; migrations run on boot
cargo run -- import       # seed from metadata.jsonl + catalog.json
cargo run                 # serve
```

Subcommands: `serve` (default), `import`, `ingest-once` (one manual poll for
new posts; the server also polls every `PCG_POLL_SECS`, default 300).

Environment:

| var | default | meaning |
|---|---|---|
| `PCG_PORT` | `4601` | listen port |
| `PCG_CATALOG` | `catalog.json` | curated catalog path |
| `PCG_MEDIA_DIR` | `/home/coreyja.linux/paperclips-media/oops` | local media archive |
| `PCG_MEDIA_MODE` | `local` | `local` (mp4s from `PCG_MEDIA_DIR`) or `cdn` (Bluesky video CDN, HLS via hls.js — no media hosting needed) |
| `DATABASE_URL` | (required) | Postgres connection string (see `.mise.toml`) |
| `PCG_POLL_SECS` | `300` | ingest poll interval; `0` disables |
| `PCG_ADMIN_DIDS` | — | comma-separated `did[=handle]` curator roster seed |
| `PCG_OAUTH_CALLBACK_URL` | `http://127.0.0.1:{port}/admin/oauth/callback` | loopback OAuth redirect |

In `local` mode, generate poster frames once (requires `ffmpeg`):

```bash
scripts/generate_posters.py [media-dir]
```

## Rooms ARE threads

Every room is a **Bluesky thread** — there is no other kind. The artist's
own registered threads are the museum's first-class plates on the front
page; anyone else's registered threads hang under Guest Rooms. Threads
render live at `/room/{handle}/{rkey}` (5-minute cache): posts are the
walk-through, post text the wall labels, the root post the introduction.
Edit the thread and the room follows.

Only the artist's specimens render (quote-posts of his work, links to his
posts, or — for his own threads — his video posts directly), so a thread
can't inject foreign content. Roster curators register threads from the
desk; viewing any thread as a room is permissionless.

## Tags & lineages

Specimens carry optional tags; tags of kind **lineage** get the
evolution-strip treatment (a chronological family of mutating forms) on
specimen pages and at `/tag/{tag}`. Tag sources:

- **curator** — roster curators add/remove tags inline on specimen pages
  (controls appear when signed in)
- **post** — `#hashtags` in the artist's captions become tags automatically
  (at import and on live ingest), so oops can tag from inside Bluesky
- **community** — reserved for a community-notes-style flow later

## The curator's desk (admin)

`/admin` is the curation portal. Identity is **Bluesky OAuth** — you sign in
with your handle, approve on your own PDS, and the site checks your DID
against the curator roster (`curators` table). We use OAuth for identity
only: the atproto tokens are discarded the moment the DID is verified.

- Roster: the artist DID (from `gallery_meta`) is always on it; seed others
  with `PCG_ADMIN_DIDS="did:plc:xyz=handle,did:plc:abc"`.
- Capabilities: register/remove thread rooms. Room contents are managed by
  editing the threads themselves, on Bluesky.
- The OAuth client currently runs in atproto *loopback* mode, so the browser
  must reach the site via `127.0.0.1` (e.g. `ssh -L 4601:localhost:4601 <vm>`
  then http://127.0.0.1:4601/admin). The hosted confidential-client metadata
  (public client_id URL + JWKS) lands together with hosting.

## Curation

`catalog.json` is the whole editorial layer: rooms, specimens, lineage
families, and margin notes. Specimens reference posts by `rkey`/`cid` from the
archive's `metadata.jsonl`. To recurate, edit the catalog and restart — there
is no database.

The curation ladder (from the project brief): artist → invited curators →
community queue. v1 is the founding survey; wall labels carry provenance so
the ladder stays visible.

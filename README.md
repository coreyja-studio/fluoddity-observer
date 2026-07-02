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
cargo run
```

Environment:

| var | default | meaning |
|---|---|---|
| `PCG_PORT` | `4601` | listen port |
| `PCG_CATALOG` | `catalog.json` | curated catalog path |
| `PCG_MEDIA_DIR` | `/home/coreyja.linux/paperclips-media/oops` | local media archive |
| `PCG_MEDIA_MODE` | `local` | `local` (mp4s from `PCG_MEDIA_DIR`) or `cdn` (Bluesky video CDN, HLS via hls.js — no media hosting needed) |

In `local` mode, generate poster frames once (requires `ffmpeg`):

```bash
scripts/generate_posters.py [media-dir]
```

## Curation

`catalog.json` is the whole editorial layer: rooms, specimens, lineage
families, and margin notes. Specimens reference posts by `rkey`/`cid` from the
archive's `metadata.jsonl`. To recurate, edit the catalog and restart — there
is no database.

The curation ladder (from the project brief): artist → invited curators →
community queue. v1 is the founding survey; wall labels carry provenance so
the ladder stays visible.

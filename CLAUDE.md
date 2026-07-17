# CLAUDE.md

Fluoddity Observer (fluoddity.observer) — a permanent field guide to the art
of Oops! All Paperclips (@all-paperclips.bsky.social). Read the README first:
it explains the concept, the participation mechanics, and the promises made
to the artist. `docs/architecture.md` maps the system.

## Commands

```bash
createdb paperclips_gallery      # once; .mise.toml supplies DATABASE_URL
cargo run -- import              # seed from metadata.jsonl + catalog.json (idempotent)
cargo run                        # serve on :4601 (migrations run on boot)
cargo test                       # NEEDS a live DATABASE_URL — see gotchas
cargo fmt && cargo clippy --all-targets -- -D warnings
cargo sqlx prepare -- --all-targets   # after any query change; commit .sqlx/
```

Other subcommands: `ingest-once`, `harvest-once`, `pull-media`,
`refresh-notes`, `classify-dimensions`, `bot-once`, `bot-weekly`,
`gen-oauth-key`.

## Gotchas that will actually bite you

- **`#[sqlx::test]` needs a live Postgres** even with `SQLX_OFFLINE=true`.
  The offline `.sqlx/` cache covers compilation only; tests create their own
  databases via `DATABASE_URL`.
- **`cargo sqlx prepare` must be run with `-- --all-targets`**, or it silently
  drops query metadata used only by tests. If prepare output looks wrong,
  `cargo clean -p cja` first — stale dep artifacts confuse it.
- **Migration numbering**: check the highest version applied on PROD
  (`_sqlx_migrations`), not just the files here — two same-day PRs have
  collided before. Duplicate versions break the live DB after merge, not CI.
- **Pushes to `main` deploy straight to production Fly** (`paperclips-gallery`
  app). There is no staging. PR titles must be conventional commits.
- Env vars load through **mise** (`.mise.toml`); there is no `.env`.

## Invariants — do not break these

- **The artist's captions render verbatim.** No paraphrase, truncation only
  where the UI already ellipsizes, never "cleanup."
- **Only the artist's media ever renders.** Reference extraction from threads
  is zero-trust (`threads.rs`): posts by anyone else contribute words, never
  images or video.
- **Any thread renders as a room by URL** (`/room/{handle}/{rkey}`) — this is
  intentional and was once wrongly "fixed." The homepage registry is curated;
  rendering is permissionless.
- **Community suggestions pass a curator** before touching the guide.

## Architecture in one breath

Single Axum binary, Maud server-rendered, no client framework. Postgres is
the source of truth (`catalog.json` + `metadata.jsonl` are the seed-era
import, not live config). Media streams from Bluesky CDNs in hosted mode
(`PCG_MEDIA_MODE=cdn`), with an optional Bunny Storage **vault** of
preservation copies served in preference order source render → PDS original →
Bluesky CDN (see `docs/architecture.md`). Background work runs on cja
cron + durable jobs (`cron.rs` / `jobs.rs`): feed ingest, margin-note
refresh, suggestion harvest, bot mentions, weekly wrap-up. Admin OAuth is
identity-only — prove a DID, check the roster, discard tokens (`auth.rs`).

## License

Code is Apache-2.0. The artwork, captions, and all media belong to the
artist and are not covered by the code license.

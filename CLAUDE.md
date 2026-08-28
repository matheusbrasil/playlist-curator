# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Running the app
```bash
npm run tauri dev        # Full desktop app (starts Vite + Tauri)
npm run dev              # Vite dev server only (frontend, no Rust)
npm run tauri build      # Production bundle
```

### Type checking
```bash
npm run typecheck        # tsc --noEmit (no build output)
npm run build            # typecheck + Vite build
```

### Rust tests
All business logic lives in the `core/` crate (`pc-core`), which has no Tauri dependency and is fully testable standalone:

```bash
cargo test -p pc-core                            # all tests
cargo test -p pc-core <test_name>                # single test by name
cargo test -p pc-core taxonomy::normalize        # single module
cargo test -p pc-core -- --nocapture             # with println output
```

`src-tauri` is excluded from the Cargo workspace (it requires webkit2gtk), so `cargo test` from the root only covers `core`.

---

## Architecture

This is a **Tauri desktop app**: React/TypeScript frontend + Rust backend.

### Rust crates

| Crate | Path | Role |
|---|---|---|
| `pc-core` | `core/` | All business logic — no Tauri dependency |
| `playlist-curator-lib` | `src-tauri/` | Thin IPC shell; excluded from workspace |

**Rule:** `src-tauri/src/commands.rs` contains no business logic. Its comment states: *"If you find yourself writing a `for` loop or a conditional, the logic belongs in `pc_core` instead."* All logic lives in `core/`.

`core/` submodules: `enrich/` (MusicBrainz, Last.fm, Discogs, Wikidata pipeline), `spotify/` (PKCE auth, client, import, publish), `suggest/` (filter, NL parser, LLM fallback, scoring), `taxonomy/` (genre tree, aliases, normalization), `store/` (SQLite via rusqlite + r2d2), `llm/` (Anthropic + Ollama backends).

### IPC layer

`src/lib/ipc.ts` is the **only** file that calls `invoke`/`listen`. Everything else imports from there. A private `call<T>(command, args?)` wrapper normalises all Tauri rejections into `IpcError` (with a `.kind` string discriminant).

Error boundary: Rust's `CommandError` serialises to `{ kind: string, message: string }`. On the TS side `toCoreError` normalises any rejection to the same shape. UI should branch on `kind` strings (e.g. `"not_authenticated"`, `"quota_exceeded"`), not on message text.

The only progress event is `"enrich://progress"` emitted by `enrich_playlist`. Subscribe via `listenEnrichProgress` from `ipc.ts`.

### Frontend patterns

- **Routing:** hash-based, no external router library. `useHashRoute` in `src/lib/router.ts` reads/writes `window.location.hash`. Falls back to `"playlists"` for unknown routes.
- **State:** no Redux or Zustand. Use `useAsync<T>` for data loading (supports `reload()` and optimistic `set()`) and `useAction` for mutations (tracks `running`, `error`, `message`). Both are in `src/lib/useAsync.ts`.
- **Settings** and `selectedPlaylistId` are loaded once in `App.tsx` and passed as props; routes do not fetch them independently.

### Key docs

- `docs/IPC_CONTRACT.md` — full list of commands, argument shapes, and return types
- `docs/DATA_SOURCES.md` — external API notes (MusicBrainz, Last.fm, Discogs, Wikidata)

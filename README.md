# Playlist Curator

A Tauri desktop app that reorganises a Spotify playlist into derived child playlists, sliced by **genre**, **geographic origin**, and **era (decade)**.

Spotify removed audio features, recommendations, and related-artists from their API for new apps in November 2024. Playlist Curator works around this by using Spotify only as an identity source (reading ISRC codes from tracks) and as a write target (creating new playlists). All musical knowledge comes from third-party APIs — MusicBrainz, Last.fm, Discogs, and Wikidata.

---

## Features

- **Sync playlists** — import your Spotify playlists into a local cache
- **Enrich tracks** — query MusicBrainz, Last.fm, Discogs, and Wikidata for genre, origin country, and era data; resumable if interrupted
- **Analyse** — view ISRC coverage, MusicBrainz match rate, and genre/country/decade distributions
- **Suggest derived playlists** — auto-suggestions from local data; free-text natural-language filter ("jazz from Brazil in the 1970s"); explicit dropdowns for genre/country/decade; scored by size, coherence, and specificity
- **Review low-confidence tracks** — manually set genre, country, or year overrides per track or artist
- **Dry-run mode** — on by default; nothing is written to Spotify until you turn it off
- **Local-first** — enriched data is stored in a local SQLite database; re-analysis works offline

---

## Required Services and Credentials

All credentials are configured in the in-app **Settings** screen. There is no `.env` file.

### Spotify Web API (required)

The app reads your playlists and creates derived playlists on your Spotify account.

1. Go to [developer.spotify.com](https://developer.spotify.com) and create a new app.
2. In the app settings, add this exact redirect URI:
   ```
   http://127.0.0.1:14523/callback
   ```
   > Use `127.0.0.1`, not `localhost` — Spotify rejects `localhost` for HTTP redirect URIs.
3. Copy the **Client ID** (not the Client Secret — the app uses PKCE and does not need it).
4. Paste the Client ID into **Settings → Spotify Client ID** inside the app.

> **Note:** Your Spotify account must be a **Premium** account. In Development Mode, up to 25 users can be added to the allowlist.

Port `14523` on `127.0.0.1` must be free during the OAuth login flow.

---

### MusicBrainz (no key, but email required)

MusicBrainz is free and open (CC0), but their API policy requires a contact email in the `User-Agent` header.

- Enter your email in **Settings → MusicBrainz Contact Email**.
- The app enforces a 1 request/second rate limit. Exceeding this risks IP blocking by MusicBrainz.

---

### Last.fm API key (optional)

Used for artist and track tags (folksonomic genre signals).

1. Create a free API account at [last.fm/api/account/create](https://www.last.fm/api/account/create).
2. Copy your **API key** into **Settings → Last.fm API Key**.

Without this, Last.fm enrichment is skipped and the app still works.

---

### Discogs personal access token (optional)

Used for editorial release genres and styles.

1. Log in to Discogs → account settings → **Developer → Generate Token**.
2. Paste the token into **Settings → Discogs Token**.

Without this, Discogs enrichment is skipped.

---

### LLM provider (optional)

Used to resolve unknown genre tags and parse natural-language playlist filter queries. Two options:

**Anthropic API** (cloud, requires API key)

1. Get an API key at [console.anthropic.com](https://console.anthropic.com).
2. In **Settings → LLM**, choose **Anthropic**, enter your API key and select a model (default: `claude-opus-5`).
3. Note: tag strings and query text are sent to Anthropic's servers.

**Ollama** (local, no data leaves your machine)

1. Install [Ollama](https://ollama.com) and pull a model, e.g.:
   ```bash
   ollama pull qwen3:8b
   ```
2. In **Settings → LLM**, choose **Ollama** and set the URL (`http://127.0.0.1:11434`) and model name.

If no LLM is configured, the NL parser falls back to its deterministic rule-based mode and unknown tags are left unresolved.

---

### Credentials summary

| Setting | What it is | Required |
|---|---|---|
| Spotify Client ID | Your Spotify developer app Client ID | Yes |
| MusicBrainz Contact Email | Your email (sent in User-Agent) | Yes (MB policy) |
| Last.fm API Key | Last.fm API key | Optional |
| Discogs Token | Discogs personal access token | Optional |
| Anthropic API Key | Anthropic API key (if using cloud LLM) | Optional |

---

## Prerequisites

| Tool | Minimum version |
|---|---|
| Node.js | 24.x |
| Rust | 1.80 (for the Tauri shell) |
| npm | Comes with Node |

No system SQLite or OpenSSL required — both are bundled.

---

## Platform Setup

### Linux (Ubuntu 22.04 or later)

Ubuntu 20.04 is **not supported** — `webkit2gtk-4.1` is not available on it.

Install system dependencies:

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  build-essential \
  curl \
  wget
```

Install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Install Node.js 24 (using [nvm](https://github.com/nvm-sh/nvm)):

```bash
nvm install 24
nvm use 24
```

> On headless servers without a D-Bus secret service, the app falls back to a file-backed credential store with `0600` permissions.

---

### macOS

Install Xcode command-line tools:

```bash
xcode-select --install
```

Install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Install Node.js 24 (using [nvm](https://github.com/nvm-sh/nvm) or [Homebrew](https://brew.sh)):

```bash
brew install node@24
```

---

### Windows

1. Install [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (or Visual Studio with the "Desktop development with C++" workload).
2. Install Rust from [rustup.rs](https://rustup.rs).
3. Install Node.js 24 from [nodejs.org](https://nodejs.org).
4. Install [WebView2](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) — usually already present on Windows 10/11.

---

## Installation

Clone the repo and install JS dependencies:

```bash
git clone <repo-url>
cd playlist-curator
npm install
```

---

## Running the App

### Development (full desktop app)

```bash
npm run tauri dev
```

Starts the Vite dev server on port `1420` and launches the Tauri desktop window. Use this for day-to-day development.

### Frontend only (no Rust)

```bash
npm run dev
```

Useful for UI-only work in a browser. The Tauri IPC calls will not work.

### Production build

```bash
npm run tauri build
```

Produces platform installers (`.deb` / `.AppImage` on Linux, `.dmg` on macOS, `.exe` / `.msi` on Windows) inside `src-tauri/target/release/bundle/`.

---

## Other Commands

```bash
npm run typecheck   # TypeScript check only (no output files)
npm run build       # Type-check + Vite production build into dist/
npm run preview     # Preview the last Vite build in a browser
```

### Rust tests (core library)

All business logic lives in the `pc-core` crate (`core/`), which has no Tauri dependency:

```bash
cargo test -p pc-core                          # all tests
cargo test -p pc-core <test_name>              # single test by name
cargo test -p pc-core taxonomy::normalize      # single module
cargo test -p pc-core -- --nocapture           # with println! output
```

> `src-tauri` is excluded from the Cargo workspace (it requires `webkit2gtk`), so `cargo test` from the root only covers `core/`.

---

## Data Storage

The app stores all data locally in a SQLite database (`curator.db`) and a settings file (`settings.json`).

Default data directory:

| Platform | Path |
|---|---|
| Windows | `%APPDATA%\playlist-curator` |
| macOS | `~/Library/Application Support/playlist-curator` |
| Linux | `$XDG_DATA_HOME/playlist-curator` or `~/.local/share/playlist-curator` |

Override with the `PLAYLIST_CURATOR_DATA_DIR` environment variable.

Spotify OAuth tokens are stored in the OS credential vault (Keychain on macOS, Secret Service on Linux, Credential Manager on Windows).

---

## Documentation

- [`docs/IPC_CONTRACT.md`](docs/IPC_CONTRACT.md) — full list of Tauri commands, argument shapes, and return types
- [`docs/DATA_SOURCES.md`](docs/DATA_SOURCES.md) — per-API rate limits, attribution requirements, and cache TTLs
- [`CLAUDE.md`](CLAUDE.md) — guidance for AI-assisted development in this repo

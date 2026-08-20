# Data Sources & Attributions

Playlist Curator enriches track metadata by querying several external APIs. This
document records each source, its terms of use, and how the app respects them.

---

## MusicBrainz

**Used for:** ISRC → recording lookup, Spotify URL → artist resolution, genre
and tag vocabularies, artist origin (country, area, begin_area), first-release
date for era derivation.

**API endpoint:** `https://musicbrainz.org/ws/2/`

**Rate limit:** 1 request per second (hard limit, not negotiable). The app
enforces this with a per-host governor rate limiter. Exceeding it results in IP
blocking.

**User-Agent requirement:** MusicBrainz requires every client to send an
identifying `User-Agent` header in the format
`AppName/Version ( contact_url )`. The app sends:

```
PlaylistCurator/<version> ( https://github.com/local/playlist-curator )
```

**Terms:** MusicBrainz data is released under the
[Creative Commons CC0 Public Domain Dedication](https://creativecommons.org/publicdomain/zero/1.0/).
Commercial and personal use are both permitted.

**Attribution:** Data from [MusicBrainz](https://musicbrainz.org), used under
CC0.

---

## Last.fm

**Used for:** Artist top tags and track top tags (folksonomic genre signals).

**API endpoint:** `https://ws.audioscrobbler.com/2.0/`

**Rate limit:** The app stays at ≤4 requests/second, below Last.fm's documented
ceiling of ~5/s for non-commercial keys.

**Terms:** The Last.fm API is free for **non-commercial use only**. A
personal-use desktop app cataloguing your own playlists qualifies. Attribution
is required.

**Required attribution:**

> Data from [Last.fm](https://www.last.fm), used under the
> [Last.fm API Terms of Service](https://www.last.fm/api/tos). Non-commercial
> personal use only.

**API key:** The user supplies their own API key from
[last.fm/api/account/create](https://www.last.fm/api/account/create). The key
is stored in the local settings file and never transmitted to any party other
than Last.fm.

---

## Discogs

**Used for:** Release genres and styles (editorial metadata), artist genre
aggregation.

**API endpoint:** `https://api.discogs.com/`

**Rate limit:** The app stays at ≤1 request/second (Discogs authenticates at
60 requests/minute; staying below ensures no throttling).

**Terms:** Discogs data is used for personal, non-commercial research into the
user's own music collection. The Discogs API requires a personal access token
for all requests, including read-only.

**Required attribution:**

> Release and artist metadata from [Discogs](https://www.discogs.com). Personal
> access token required. See
> [Discogs API Terms](https://support.discogs.com/hc/en-us/articles/360009334593-API-Terms-of-Use).

**Token:** The user supplies a personal access token from their Discogs account
settings. Stored locally in the settings file.

---

## Wikidata

**Used for:** Country of origin fallback when MusicBrainz `country` and
`begin_area` are absent. Queries `P495` (country of origin) and `P740`
(location of formation) via SPARQL.

**API endpoint:** `https://query.wikidata.org/sparql`

**Rate limit:** No documented hard limit; the app stays at ≤2 requests/second
as a courtesy to the free public endpoint.

**Terms:** Wikidata content is in the public domain under
[CC0](https://creativecommons.org/publicdomain/zero/1.0/).

**Attribution:** Data from [Wikidata](https://www.wikidata.org), used under CC0.

---

## Spotify Web API

**Used for:** Reading the user's playlists and their tracks (including ISRC in
`external_ids`). Creating derived playlists. **No** audio analysis, genre data,
or recommendation data is requested from Spotify.

**API endpoint:** `https://api.spotify.com/v1/`

**Authentication:** OAuth 2.0 PKCE (public client, no client secret). The
redirect URI is `http://127.0.0.1:14523/callback` (IPv4 loopback literal, as
required by Spotify — `localhost` is rejected).

**Development Mode:** The app runs in Spotify Development Mode, which requires:
- Spotify Premium on the account that owns the app
- Up to 25 Client IDs per developer account
- Users must be added to the app's allowlist (max 25 in Development Mode)

**Terms:** [Spotify Developer Terms of Service](https://developer.spotify.com/terms).
Personal, non-commercial use for playlist management of the user's own account.

---

## Caching policy

All API responses are cached in a local SQLite database with the following
default TTLs:

| Source      | Default TTL |
|-------------|-------------|
| MusicBrainz | 90 days     |
| Last.fm     | 30 days     |
| Discogs     | 30 days     |
| Wikidata    | 90 days     |

Cached data is never shared with third parties. The cache serves two purposes:
(1) respecting each API's rate limits on subsequent runs; (2) allowing offline
re-analysis of a playlist once it has been enriched.

The user can clear the cache from the Settings screen at any time.

---

## Privacy

- OAuth tokens for Spotify are stored in the OS credential vault (`keyring`) or
  a `0600` permissions file on disk. They never reach the webview.
- Last.fm API key and Discogs token are stored in a local JSON settings file.
- No usage data, playlist names, or track metadata are sent to any server
  other than the APIs listed above, and only as part of normal API requests.
- The optional LLM (Ollama or Anthropic) receives tag strings and playlist
  filter descriptions. If Ollama is selected, no data leaves the machine.
  If Anthropic is selected, tag and query data is sent to Anthropic's API
  subject to [Anthropic's Terms of Service](https://www.anthropic.com/terms).

import { useState } from "react";
import { DryRunBanner } from "../components/DryRunBanner";
import { ErrorNotice } from "../components/ErrorNotice";
import { SuggestionCard } from "../components/SuggestionCard";
import { TrackTable } from "../components/TrackTable";
import {
  type AnalysedTrack,
  type CreateResult,
  type Settings,
  type SuggestionCard as Card,
  createPlaylist,
  suggestFromQuery,
  suggestPlaylists,
} from "../lib/ipc";
import type { RouteName } from "../lib/router";
import { useAction, useAsync } from "../lib/useAsync";

type Props = {
  playlistId: string | null;
  settings: Settings | null;
  navigate: (route: RouteName) => void;
};

function toAnalysedTrack(st: Card["tracks"][number]): AnalysedTrack {
  return {
    spotifyId: st.spotifyId,
    name: st.name,
    artists: st.artists.map((name) => ({ spotifyId: "", name })),
    isrc: null,
    genres: st.reason.genre ? [{ slug: st.reason.genre, score: st.reason.genreScore }] : [],
    origin: st.reason.countryCode
      ? { countryCode: st.reason.countryCode, countryLabel: null, city: null, source: st.reason.eraSource ?? "unknown", confidence: 1 }
      : null,
    era: st.reason.year
      ? { year: st.reason.year, decade: st.reason.year ? Math.floor(st.reason.year / 10) * 10 : null, source: st.reason.eraSource ?? "unknown" }
      : null,
    needsReview: st.reason.needsReview,
  };
}

export function Suggestions({ playlistId, settings, navigate }: Props) {
  const dryRun = settings?.dryRun ?? true;

  const suggestions = useAsync(
    () => (playlistId ? suggestPlaylists(playlistId) : Promise.resolve([])),
    [playlistId],
  );

  const [query, setQuery] = useState("");
  const queryAction = useAction();
  const [queryResult, setQueryResult] = useState<Card | null>(null);

  const createAction = useAction();
  const [createResult, setCreateResult] = useState<CreateResult | null>(null);
  const [openCardId, setOpenCardId] = useState<string | null>(null);
  const [creatingCardId, setCreatingCardId] = useState<string | null>(null);

  if (!playlistId) {
    return (
      <div className="screen">
        <h2>Suggestions</h2>
        <p className="muted">Select a playlist on the Playlists tab first.</p>
        <button type="button" onClick={() => navigate("playlists")}>
          Go to Playlists
        </button>
      </div>
    );
  }

  async function runCreate(card: Card) {
    setCreatingCardId(card.id);
    setCreateResult(null);
    await createAction.run(
      () => createPlaylist({ card, public: false, dryRunOverride: null }),
      (result) => {
        setCreateResult(result);
        return result.dryRun
          ? `Dry run: would create "${result.name}" with ${result.trackCount} tracks.`
          : `Created "${result.name}" with ${result.trackCount} tracks on Spotify.`;
      },
    );
    setCreatingCardId(null);
  }

  async function runQuery() {
    if (!query.trim() || !playlistId) return;
    setQueryResult(null);
    await queryAction.run(
      () => suggestFromQuery(playlistId, query),
      (card) => {
        setQueryResult(card);
        return `Found ${card.trackCount} tracks for "${query}".`;
      },
    );
  }

  const allCards: Card[] = [
    ...(suggestions.state.status === "success" ? suggestions.state.data : []),
    ...(queryResult ? [queryResult] : []),
  ];

  return (
    <div className="screen">
      <h2>Suggestions</h2>

      {dryRun ? (
        <DryRunBanner
          dryRun
          onOpenSettings={() => navigate("settings")}
        />
      ) : null}

      {createResult ? (
        <div className={`notice ${createResult.dryRun ? "notice-info" : "notice-ok"}`} role="status">
          <p className="notice-title">
            {createResult.dryRun ? "Dry run result" : "Playlist created"}
          </p>
          <p>
            {createResult.dryRun
              ? `Would create "${createResult.name}" with ${createResult.trackCount} tracks.`
              : `Created "${createResult.name}" — open in Spotify to see it.`}
          </p>
          {createResult.skipped.length > 0 ? (
            <p className="muted">
              {createResult.skipped.length} tracks skipped (local files or no URI).
            </p>
          ) : null}
        </div>
      ) : null}
      {createAction.error ? (
        <ErrorNotice
          error={createAction.error}
          onRetry={createAction.clear}
          onGoConnect={() => navigate("connect")}
        />
      ) : null}

      <section className="panel">
        <h3>Free-text query</h3>
        <p className="muted">
          Describe the playlist you want — genre, country, decade, or a combination.
          {" "}Uses the LLM if configured, otherwise parses common patterns.
        </p>
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            void runQuery();
          }}
        >
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder='e.g. "Brazilian soul from the 1970s"'
            disabled={queryAction.running}
            style={{ flex: 1 }}
          />
          <button
            type="submit"
            className="primary"
            disabled={queryAction.running || !query.trim()}
          >
            {queryAction.running ? "Searching…" : "Search"}
          </button>
        </form>
        {queryAction.error ? (
          <ErrorNotice error={queryAction.error} onRetry={queryAction.clear} />
        ) : null}
        {queryAction.message ? <p className="ok">{queryAction.message}</p> : null}
      </section>

      <section className="panel">
        <h3>Automatic suggestions</h3>

        {suggestions.state.status === "loading" ? (
          <p aria-live="polite">Computing suggestions…</p>
        ) : null}
        {suggestions.state.status === "error" ? (
          <ErrorNotice
            error={suggestions.state.error}
            onRetry={suggestions.reload}
          />
        ) : null}
        {suggestions.state.status === "success" && suggestions.state.data.length === 0 ? (
          <p className="muted">
            No suggestions yet. Enrich the playlist on the Analysis tab first.
          </p>
        ) : null}

        <div className="card-grid">
          {allCards.map((card) => (
            <div key={card.id}>
              <SuggestionCard
                card={card}
                dryRun={dryRun}
                selected={openCardId === card.id}
                onOpen={(c) => setOpenCardId(openCardId === c.id ? null : c.id)}
                onCreate={(c) => void runCreate(c)}
                creating={creatingCardId === card.id}
              />
              {openCardId === card.id ? (
                <section className="panel panel-inset">
                  <h4>Tracks in this suggestion</h4>
                  <TrackTable
                    tracks={card.tracks.map(toAnalysedTrack)}
                    caption={`${card.trackCount} tracks`}
                  />
                </section>
              ) : null}
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import { ErrorNotice } from "../components/ErrorNotice";
import { FacetChart } from "../components/FacetChart";
import { ProgressBar } from "../components/ProgressBar";
import { TrackTable } from "../components/TrackTable";
import {
  type AnalysedTrack,
  type AnalysisSummary,
  type EnrichProgress,
  type EnrichStats,
  type Settings,
  analysisSummary,
  analysisTracks,
  enrichPlaylist,
  listenEnrichProgress,
} from "../lib/ipc";
import { percent } from "../lib/format";
import type { RouteName } from "../lib/router";
import { useAction, useAsync } from "../lib/useAsync";

type Props = {
  playlistId: string | null;
  settings: Settings | null;
  navigate: (route: RouteName) => void;
};

type ProgressState = {
  done: number;
  total: number;
  label: string;
};

export function Analysis({ playlistId, navigate }: Props) {
  const summary = useAsync(
    () => (playlistId ? analysisSummary(playlistId) : Promise.resolve(null)),
    [playlistId],
  );
  const tracks = useAsync(
    () => (playlistId ? analysisTracks(playlistId) : Promise.resolve(null)),
    [playlistId],
  );

  const enrich = useAction();
  const [progress, setProgress] = useState<ProgressState | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);

  async function runEnrich() {
    if (!playlistId) return;

    setProgress({ done: 0, total: 0, label: "Starting…" });

    const unlisten = await listenEnrichProgress((ev: EnrichProgress) => {
      if (ev.type === "started") {
        setProgress({ done: 0, total: ev.total, label: "Starting…" });
      } else if (ev.type === "track") {
        setProgress({ done: ev.done, total: ev.total, label: `Track: ${ev.name}` });
      } else if (ev.type === "artist") {
        setProgress({ done: ev.done, total: ev.total, label: `Artist: ${ev.name}` });
      } else if (ev.type === "finished") {
        setProgress(null);
      }
    });
    unlistenRef.current = unlisten;

    await enrich.run(
      () => enrichPlaylist(playlistId),
      (stats: EnrichStats) => {
        setProgress(null);
        unlisten();
        summary.reload();
        tracks.reload();
        return `Enriched ${stats.tracksMatched} / ${stats.tracksTotal} tracks. Cache hits: ${stats.cacheHits}.`;
      },
    );

    if (enrich.error) {
      setProgress(null);
    }
  }

  useEffect(() => {
    return () => {
      unlistenRef.current?.();
    };
  }, []);

  if (!playlistId) {
    return (
      <div className="screen">
        <h2>Analysis</h2>
        <p className="muted">Select a playlist on the Playlists tab first.</p>
        <button type="button" onClick={() => navigate("playlists")}>
          Go to Playlists
        </button>
      </div>
    );
  }

  const sum: AnalysisSummary | null =
    summary.state.status === "success" ? summary.state.data : null;
  const trackList: AnalysedTrack[] | null =
    tracks.state.status === "success" ? tracks.state.data : null;

  return (
    <div className="screen">
      <h2>Analysis</h2>

      <section className="panel">
        <h3>Enrich</h3>
        <p className="muted">
          Fetches genre, origin and era data from MusicBrainz, Last.fm and Discogs. Resumable — safe
          to interrupt and re-run.
        </p>
        <div className="row">
          <button
            type="button"
            className="primary"
            disabled={enrich.running}
            onClick={() => void runEnrich()}
          >
            {enrich.running ? "Enriching…" : "Enrich playlist"}
          </button>
        </div>

        {progress ? (
          <ProgressBar
            label={progress.label}
            value={progress.done}
            max={progress.total}
            indeterminate={progress.total === 0}
          />
        ) : null}

        {enrich.error ? (
          <ErrorNotice
            error={enrich.error}
            onRetry={enrich.clear}
            onGoConnect={() => navigate("connect")}
            onGoSettings={() => navigate("settings")}
          />
        ) : null}
        {enrich.message ? <p className="ok">{enrich.message}</p> : null}
      </section>

      <section className="panel">
        <h3>Summary</h3>
        {summary.state.status === "loading" ? <p aria-live="polite">Loading summary…</p> : null}
        {summary.state.status === "error" ? (
          <ErrorNotice error={summary.state.error} onRetry={summary.reload} />
        ) : null}
        {sum ? (
          <>
            <dl className="kv">
              <div>
                <dt>Tracks</dt>
                <dd>{sum.trackCount}</dd>
              </div>
              <div>
                <dt>ISRC coverage</dt>
                <dd
                  title="Higher is better — ISRC enables deterministic MusicBrainz matching"
                >
                  {percent(sum.isrcCoverage, 1)}
                </dd>
              </div>
              <div>
                <dt>MusicBrainz coverage</dt>
                <dd>{percent(sum.mbCoverage, 1)}</dd>
              </div>
              {sum.needsReviewCount > 0 ? (
                <div>
                  <dt>Needs review</dt>
                  <dd>
                    {sum.needsReviewCount} track{sum.needsReviewCount !== 1 ? "s" : ""} with low
                    confidence
                  </dd>
                </div>
              ) : null}
            </dl>

            <div className="charts">
              <FacetChart
                title="Genres"
                items={sum.genreDistribution.map((g) => ({
                  key: g.slug,
                  label: g.label,
                  count: g.count,
                }))}
                emptyHint="No genre data yet — run Enrich first."
              />
              <FacetChart
                title="Origins"
                items={sum.countryDistribution.map((c) => ({
                  key: c.code,
                  label: c.label || c.code,
                  count: c.count,
                }))}
                emptyHint="No origin data yet — run Enrich first."
              />
              <FacetChart
                title="Decades"
                items={sum.decadeDistribution.map((d) => ({
                  key: String(d.decade),
                  label: `${d.decade}s`,
                  count: d.count,
                }))}
                emptyHint="No era data yet — run Enrich first."
              />
            </div>
          </>
        ) : null}
      </section>

      <section className="panel">
        <h3>Tracks</h3>
        {tracks.state.status === "loading" ? <p aria-live="polite">Loading tracks…</p> : null}
        {tracks.state.status === "error" ? (
          <ErrorNotice error={tracks.state.error} onRetry={tracks.reload} />
        ) : null}
        {trackList ? (
          <TrackTable
            tracks={trackList}
            caption={`${trackList.length} tracks in the playlist`}
          />
        ) : null}
      </section>
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import { ErrorNotice } from "../components/ErrorNotice";
import { FacetChart } from "../components/FacetChart";
import { ProgressBar } from "../components/ProgressBar";
import { TrackTable } from "../components/TrackTable";
import {
  type AnalysedTrack,
  type AnalysisSummary,
  type DeriveStats,
  type EnrichProgress,
  type EnrichStats,
  type Settings,
  analysisSummary,
  analysisTracks,
  derivePlaylist,
  enrichCounts,
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
  onEnrichStart?: () => void;
  onEnrichEnd?: () => void;
};

type ProgressState = {
  done: number;
  total: number;
  label: string;
};

export function Analysis({ playlistId, navigate, onEnrichStart, onEnrichEnd }: Props) {
  const summary = useAsync(
    () => (playlistId ? analysisSummary(playlistId) : Promise.resolve(null)),
    [playlistId],
  );
  const tracks = useAsync(
    () => (playlistId ? analysisTracks(playlistId) : Promise.resolve(null)),
    [playlistId],
  );

  const enrich = useAction();
  const derive = useAction();
  const [progress, setProgress] = useState<ProgressState | null>(null);
  const [batchSize, setBatchSize] = useState<number | null | "unresolved">(null);
  const [enrichCountsData, setEnrichCountsData] = useState<{
    total: number;
    unresolved: number;
  } | null>(null);
  const [summaryOpen, setSummaryOpen] = useState(true);
  const unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (playlistId) {
      enrichCounts(playlistId).then(setEnrichCountsData).catch(() => {});
    }
  }, [playlistId]);

  async function runEnrich(limit: number | null, onlyUnresolved?: boolean) {
    if (!playlistId) return;

    onEnrichStart?.();
    setProgress({ done: 0, total: 0, label: "Starting…" });

    const unlisten = await listenEnrichProgress((ev: EnrichProgress) => {
      setProgress({ done: ev.current, total: ev.total, label: `Track: ${ev.trackName}` });
    });
    unlistenRef.current = unlisten;

    await enrich.run(
      async () => {
        const stats = await enrichPlaylist(playlistId, limit ?? undefined, onlyUnresolved);
        // Derive genres/origins/eras from the raw signals that were just collected.
        // This is a fast, local-only pass — no network needed.
        await derivePlaylist(playlistId);
        return stats;
      },
      (stats: EnrichStats) => {
        setProgress(null);
        unlisten();
        onEnrichEnd?.();
        summary.reload();
        tracks.reload();
        enrichCounts(playlistId).then(setEnrichCountsData).catch(() => {});
        const parts: string[] = [];
        if (stats.recordingsResolved > 0)
          parts.push(`${stats.recordingsResolved} MB recording${stats.recordingsResolved !== 1 ? "s" : ""}`);
        if (stats.artistsResolved > 0)
          parts.push(`${stats.artistsResolved} artist${stats.artistsResolved !== 1 ? "s" : ""}`);
        if (stats.tagSignalsInserted > 0)
          parts.push(`${stats.tagSignalsInserted} tag signal${stats.tagSignalsInserted !== 1 ? "s" : ""}`);
        const summary_str =
          parts.length > 0
            ? parts.join(", ")
            : "no new data (all already resolved or sources unavailable)";
        return `Enriched: ${summary_str}. Network calls: ${stats.networkCalls}, cache hits: ${stats.cacheHits}.`;
      },
    );

    if (enrich.error) {
      setProgress(null);
      onEnrichEnd?.();
    }
  }

  useEffect(() => {
    return () => {
      unlistenRef.current?.();
      onEnrichEnd?.();
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

  const unresolvedLabel = enrichCountsData
    ? ` (${enrichCountsData.unresolved})`
    : "";

  return (
    <div className="screen--split">
      <div className="page-toolbar">
        {/* Row 1: Enrich button + batch selector */}
        <div className="row" style={{ alignItems: "center", gap: 8 }}>
          <label htmlFor="batchSize" style={{ whiteSpace: "nowrap" }}>
            Batch:
          </label>
          <select
            id="batchSize"
            value={batchSize === "unresolved" ? "unresolved" : (batchSize ?? "all")}
            disabled={enrich.running}
            onChange={(e) => {
              if (e.target.value === "all") setBatchSize(null);
              else if (e.target.value === "unresolved") setBatchSize("unresolved");
              else setBatchSize(Number(e.target.value));
            }}
            style={{ minWidth: 200 }}
          >
            <option value="all">All unresolved</option>
            <option value="unresolved">Unresolved only{unresolvedLabel}</option>
            <option value="5">Next 5 tracks</option>
            <option value="10">Next 10 tracks</option>
            <option value="20">Next 20 tracks</option>
            <option value="50">Next 50 tracks</option>
          </select>
          <button
            type="button"
            className="primary"
            disabled={enrich.running}
            onClick={() => {
              if (batchSize === "unresolved") void runEnrich(null, true);
              else void runEnrich(batchSize);
            }}
          >
            {enrich.running
              ? "Enriching…"
              : batchSize === "unresolved"
              ? "Enrich unresolved"
              : batchSize
              ? `Enrich next ${batchSize}`
              : "Enrich all"}
          </button>
          {enrich.message ? <span className="ok small">{enrich.message}</span> : null}
        </div>

        {/* Row 2: Progress bar when running */}
        {progress ? (
          <ProgressBar
            label={progress.label}
            value={progress.done}
            max={progress.total}
            indeterminate={progress.total === 0}
          />
        ) : null}

        {/* Row 3: Derive button + status */}
        <div className="row">
          <button
            type="button"
            disabled={derive.running || enrich.running}
            onClick={() =>
              void derive.run(
                () => derivePlaylist(playlistId),
                (result: DeriveStats) => {
                  summary.reload();
                  tracks.reload();
                  return `Derived ${result.tracksWithGenre} genre${result.tracksWithGenre !== 1 ? "s" : ""}, ${result.originsResolved} origin${result.originsResolved !== 1 ? "s" : ""}, ${result.erasResolved} era${result.erasResolved !== 1 ? "s" : ""}.`;
                },
              )
            }
          >
            {derive.running ? "Deriving…" : "Derive genres & origins"}
          </button>
          {derive.message ? <span className="ok small">{derive.message}</span> : null}
        </div>

        {enrich.error ? (
          <ErrorNotice
            error={enrich.error}
            onRetry={enrich.clear}
            onGoConnect={() => navigate("settings")}
            onGoSettings={() => navigate("settings")}
          />
        ) : null}
        {derive.error ? <ErrorNotice error={derive.error} onRetry={derive.clear} /> : null}
      </div>

      <div className="page-body">
        <h2>Analysis</h2>

        {/* Summary section — collapsible */}
        <section className="panel">
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
            }}
          >
            <h3>Summary</h3>
            <button
              type="button"
              onClick={() => setSummaryOpen((v) => !v)}
              style={{ background: "none", border: "none", cursor: "pointer", fontSize: 12, color: "var(--text-dim)" }}
            >
              {summaryOpen ? "▼" : "▶"}
            </button>
          </div>

          {summaryOpen ? (
            <>
              {summary.state.status === "loading" ? (
                <p aria-live="polite">Loading summary…</p>
              ) : null}
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
                          {sum.needsReviewCount} track{sum.needsReviewCount !== 1 ? "s" : ""} with
                          low confidence
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
            </>
          ) : null}
        </section>

        {/* Track list — always visible */}
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
    </div>
  );
}

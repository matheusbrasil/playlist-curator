import { useState } from "react";
import { ErrorNotice } from "../components/ErrorNotice";
import { listReviews, resolveReview, retryEnrichTrack, type ReviewItem, type Settings } from "../lib/ipc";
import type { RouteName } from "../lib/router";
import { useAsync } from "../lib/useAsync";

interface Props {
  navigate: (route: RouteName) => void;
  settings: Settings | null;
}

const REASON_LABELS: Record<string, string> = {
  low_confidence_match: "Low-confidence match",
  no_mb_match: "No MusicBrainz match",
  search_error: "Search error",
};

function reasonLabel(reason: string): string {
  return REASON_LABELS[reason] ?? reason;
}

function formatDetail(reason: string, detail: string | null, reviewThreshold?: number): string {
  if (reason === "search_error") return "MusicBrainz query failed";
  if (reason === "no_mb_match") return "No recording found";
  if (!detail) return "—";
  const match = detail.match(/^best score\s+([\d.]+)$/i);
  if (match) {
    const threshold = reviewThreshold != null ? reviewThreshold.toFixed(2) : "0.50";
    return `Match confidence: ${match[1]} (threshold: ${threshold})`;
  }
  return detail;
}

const RETRYABLE_REASONS = new Set(["search_error", "no_mb_match"]);

function actionHint(reason: string): string | null {
  if (reason === "low_confidence_match")
    return "Verify genre and origin look correct on the Analysis tab. If they look right, dismiss to accept.";
  if (reason === "no_mb_match")
    return "Genre relies on artist-level tags (Last.fm/Discogs) only — may be coarser. Use Retry to check if MusicBrainz has updated, or set manual overrides in Analysis.";
  if (reason === "search_error")
    return "Use Retry to re-attempt the MusicBrainz query. Dismiss to ignore if not critical.";
  return null;
}

function TrackCell({ item }: { item: ReviewItem }) {
  const name = item.trackName ?? null;
  const artists = item.artistNames.length > 0 ? item.artistNames.join(", ") : null;
  return (
    <td>
      {name ? (
        <span style={{ display: "block" }}>{name}</span>
      ) : null}
      {artists ? (
        <span className="muted" style={{ display: "block", fontSize: 12 }}>
          {artists}
        </span>
      ) : null}
      <span
        className="muted"
        style={{ display: "block", fontFamily: "monospace", fontSize: 11 }}
      >
        {item.entityId}
      </span>
    </td>
  );
}

export function Reviews({ navigate, settings }: Props) {
  const reviews = useAsync(listReviews, []);
  const [resolving, setResolving] = useState<Set<string>>(new Set());
  const [retrying, setRetrying] = useState<Set<string>>(new Set());
  const [retryErrors, setRetryErrors] = useState<Map<string, string>>(new Map());
  const [retryAllProgress, setRetryAllProgress] = useState<{ done: number; total: number } | null>(null);
  const [retryAllSummary, setRetryAllSummary] = useState<string | null>(null);

  const items: ReviewItem[] =
    reviews.state.status === "success" ? (reviews.state.data ?? []) : [];

  const retryAllRunning = retryAllProgress !== null;

  async function handleResolve(item: ReviewItem) {
    const key = `${item.entityType}:${item.entityId}:${item.reason}`;
    setResolving((prev) => new Set(prev).add(key));
    try {
      await resolveReview({
        entityType: item.entityType,
        entityId: item.entityId,
        reason: item.reason,
      });
      reviews.set(
        items.filter(
          (i) =>
            !(
              i.entityType === item.entityType &&
              i.entityId === item.entityId &&
              i.reason === item.reason
            ),
        ),
      );
    } finally {
      setResolving((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  }

  async function handleRetry(item: ReviewItem) {
    const key = `${item.entityType}:${item.entityId}:${item.reason}`;
    setRetrying((prev) => new Set(prev).add(key));
    setRetryErrors((prev) => {
      const next = new Map(prev);
      next.delete(key);
      return next;
    });
    try {
      await retryEnrichTrack(item.entityId, item.reason);
      reviews.reload();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setRetryErrors((prev) => new Map(prev).set(key, message));
    } finally {
      setRetrying((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  }

  async function handleRetryAll() {
    // Deduplicate by entityId+reason — same track can't appear twice in the same group.
    const seen = new Set<string>();
    const queue = items.filter((i) => {
      if (!RETRYABLE_REASONS.has(i.reason)) return false;
      const key = `${i.entityId}:${i.reason}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });

    if (queue.length === 0) return;

    setRetryAllSummary(null);
    setRetryAllProgress({ done: 0, total: queue.length });
    setRetryErrors(new Map());

    let failed = 0;
    // Sequential to respect the MusicBrainz rate limiter inside each enrich_track call.
    for (const item of queue) {
      const key = `${item.entityType}:${item.entityId}:${item.reason}`;
      setRetrying((prev) => new Set(prev).add(key));
      try {
        await retryEnrichTrack(item.entityId, item.reason);
      } catch (err) {
        failed++;
        const message = err instanceof Error ? err.message : String(err);
        setRetryErrors((prev) => new Map(prev).set(key, message));
      } finally {
        setRetrying((prev) => {
          const next = new Set(prev);
          next.delete(key);
          return next;
        });
        setRetryAllProgress((prev) => prev ? { ...prev, done: prev.done + 1 } : null);
      }
    }

    setRetryAllProgress(null);
    reviews.reload();

    const succeeded = queue.length - failed;
    if (failed === 0) {
      setRetryAllSummary(`Retried ${queue.length} track${queue.length !== 1 ? "s" : ""} — list refreshed.`);
    } else {
      setRetryAllSummary(
        `Retried ${queue.length} track${queue.length !== 1 ? "s" : ""}: ${succeeded} ok, ${failed} failed (errors shown inline).`,
      );
    }
  }

  const retryableCount = items.filter((i) => RETRYABLE_REASONS.has(i.reason)).length;

  const grouped = new Map<string, ReviewItem[]>();
  for (const item of items) {
    const group = grouped.get(item.reason) ?? [];
    group.push(item);
    grouped.set(item.reason, group);
  }

  function reasonExplanation(reason: string): React.ReactNode {
    if (reason === "low_confidence_match") {
      return (
        <p className="muted" style={{ marginTop: 4 }}>
          MusicBrainz found a recording for this track but the match score was below the
          acceptance threshold. Genre and origin data may still be correct, but they're less
          reliable. If the genre or origin looks wrong, use the{" "}
          <button type="button" className="link" onClick={() => navigate("analysis")}>
            Analysis
          </button>{" "}
          tab to set a manual override.
        </p>
      );
    }
    if (reason === "no_mb_match") {
      return (
        <p className="muted" style={{ marginTop: 4 }}>
          No MusicBrainz recording was found. If Last.fm and Discogs API keys are configured in
          Settings, artist-level tags may still be present — check the genre distribution on the{" "}
          <button type="button" className="link" onClick={() => navigate("analysis")}>
            Analysis
          </button>{" "}
          tab. Otherwise, set manual overrides or resolve to dismiss.
        </p>
      );
    }
    if (reason === "search_error") {
      return (
        <p className="muted" style={{ marginTop: 4 }}>
          An error occurred while querying MusicBrainz. Use <strong>Retry</strong> to re-attempt
          the query — transient network errors usually resolve on the next attempt.
        </p>
      );
    }
    return null;
  }

  return (
    <div className="screen">
      <h2>Reviews</h2>
      <p className="muted">
        Tracks flagged during MusicBrainz enrichment that may have inaccurate metadata. Use{" "}
        <strong>Retry</strong> to re-attempt enrichment for a single track, or{" "}
        <strong>Dismiss</strong> to acknowledge and remove the flag.
      </p>

      {reviews.state.status === "loading" ? (
        <p aria-live="polite">Loading reviews…</p>
      ) : null}
      {reviews.state.status === "error" ? (
        <ErrorNotice error={reviews.state.error} onRetry={reviews.reload} />
      ) : null}

      {reviews.state.status === "success" && items.length === 0 ? (
        <p className="ok">No open review items — all tracks look good.</p>
      ) : null}

      {retryableCount > 0 ? (
        <div className="row" style={{ alignItems: "center", gap: 12, marginBottom: 16 }}>
          <button
            type="button"
            className="primary"
            disabled={retryAllRunning || retrying.size > 0}
            onClick={() => void handleRetryAll()}
          >
            {retryAllRunning
              ? `Retrying ${retryAllProgress!.done} / ${retryAllProgress!.total}…`
              : `Retry all (${retryableCount})`}
          </button>
          {retryAllSummary ? (
            <span className="ok small" aria-live="polite">{retryAllSummary}</span>
          ) : null}
        </div>
      ) : null}

      {[...grouped.entries()].map(([reason, group]) => (
        <section key={reason} className="panel">
          <h3>{reasonLabel(reason)}</h3>
          {reasonExplanation(reason)}
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>Track</th>
                  <th>Detail</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {group.map((item) => {
                  const key = `${item.entityType}:${item.entityId}:${item.reason}`;
                  const hint = actionHint(item.reason);
                  const canRetry = RETRYABLE_REASONS.has(item.reason);
                  const isBusy = resolving.has(key) || retrying.has(key) || retryAllRunning;
                  const retryError = retryErrors.get(key);
                  return (
                    <tr key={key}>
                      <TrackCell item={item} />
                      <td style={{ fontSize: 12 }}>
                        <span className="muted">
                          {formatDetail(item.reason, item.detail, settings?.reviewThreshold)}
                        </span>
                        {hint ? (
                          <span
                            className="muted"
                            style={{ display: "block", fontStyle: "italic", marginTop: 2 }}
                          >
                            {hint}
                          </span>
                        ) : null}
                        {retryError ? (
                          <span
                            style={{ display: "block", color: "var(--error, #c0392b)", marginTop: 4, fontSize: 11 }}
                          >
                            Retry failed: {retryError}
                          </span>
                        ) : null}
                      </td>
                      <td>
                        <div style={{ display: "flex", gap: 4, flexDirection: "column" }}>
                          {canRetry ? (
                            <button
                              type="button"
                              className="primary"
                              disabled={isBusy}
                              title={`Re-run MusicBrainz enrichment for '${item.trackName ?? item.entityId}'`}
                              onClick={() => void handleRetry(item)}
                            >
                              {retrying.has(key) ? "Retrying…" : "Retry"}
                            </button>
                          ) : null}
                          <button
                            type="button"
                            disabled={isBusy}
                            title={`Dismiss the '${reasonLabel(item.reason)}' flag for '${item.trackName ?? item.entityId}'`}
                            onClick={() => void handleResolve(item)}
                          >
                            {resolving.has(key) ? "Dismissing…" : "Dismiss"}
                          </button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </section>
      ))}
    </div>
  );
}

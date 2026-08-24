import { useEffect, useState } from "react";
import { ErrorNotice } from "../components/ErrorNotice";
import {
  type Settings,
  SOURCE_WEIGHT_KEYS,
  clearCache,
  exportDatabase,
} from "../lib/ipc";
import { type Async, useAction } from "../lib/useAsync";

type Props = {
  settings: Async<Settings>;
  onSaveSettings: (next: Settings) => Promise<void>;
};

const WEIGHT_LABELS: Record<string, string> = {
  musicbrainzGenre: "MusicBrainz genre (curated, voted)",
  discogs: "Discogs (editorial)",
  musicbrainzTag: "MusicBrainz tag (free-form)",
  lastfmArtist: "Last.fm artist tags",
  lastfmTrack: "Last.fm track tags",
  spotifyArtist: "Spotify artist genres (last resort)",
};

export function Advanced({ settings, onSaveSettings }: Props) {
  const loaded = settings.state.status === "success" ? settings.state.data : null;
  const [draft, setDraft] = useState<Settings | null>(null);
  useEffect(() => setDraft(loaded), [loaded]);

  const save = useAction();
  const cacheAction = useAction();
  const exportAction = useAction();

  function patchCache(next: Partial<Settings["cache"]>) {
    setDraft((current) =>
      current ? { ...current, cache: { ...current.cache, ...next } } : current,
    );
  }

  function patchWeight(key: string, value: number) {
    setDraft((current) =>
      current
        ? { ...current, weights: { ...current.weights, [key]: value } }
        : current,
    );
  }

  if (settings.state.status === "loading") {
    return (
      <div className="screen">
        <h2>Advanced</h2>
        <p aria-live="polite">Loading settings…</p>
      </div>
    );
  }

  if (settings.state.status === "error") {
    return (
      <div className="screen">
        <h2>Advanced</h2>
        <ErrorNotice error={settings.state.error} onRetry={settings.reload} />
      </div>
    );
  }

  if (!draft) {
    return null;
  }

  return (
    <div className="screen">
      <h2>Advanced</h2>

      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save.run(
            async () => {
              await onSaveSettings(draft);
            },
            () => "Settings saved.",
          );
        }}
      >
        <section className="panel">
          <h3>Cache TTL</h3>
          <p className="muted">
            Cached API responses are not re-fetched until they expire. Longer TTLs mean fewer
            network requests but older data.
          </p>
          <div className="field">
            <label htmlFor="mbTtl">MusicBrainz (days)</label>
            <input
              id="mbTtl"
              type="number"
              min={1}
              max={365}
              value={draft.cache.musicbrainzTtlDays}
              onChange={(e) => patchCache({ musicbrainzTtlDays: Number(e.target.value) })}
            />
          </div>
          <div className="field">
            <label htmlFor="lfmTtl">Last.fm (days)</label>
            <input
              id="lfmTtl"
              type="number"
              min={1}
              max={365}
              value={draft.cache.lastfmTtlDays}
              onChange={(e) => patchCache({ lastfmTtlDays: Number(e.target.value) })}
            />
          </div>
          <div className="field">
            <label htmlFor="dgTtl">Discogs (days)</label>
            <input
              id="dgTtl"
              type="number"
              min={1}
              max={365}
              value={draft.cache.discogsTtlDays}
              onChange={(e) => patchCache({ discogsTtlDays: Number(e.target.value) })}
            />
          </div>
          <div className="field">
            <label htmlFor="wdTtl">Wikidata (days)</label>
            <input
              id="wdTtl"
              type="number"
              min={1}
              max={365}
              value={draft.cache.wikidataTtlDays}
              onChange={(e) => patchCache({ wikidataTtlDays: Number(e.target.value) })}
            />
          </div>
        </section>

        <section className="panel">
          <h3>Source weights</h3>
          <p className="muted">
            How much each data source's tags contribute to the genre score (0 = ignore, 1 = full
            weight). Changing weights takes effect on the next "Derive" pass — no network needed.
          </p>
          {SOURCE_WEIGHT_KEYS.map((key) => (
            <div className="field" key={key}>
              <label htmlFor={`w-${key}`}>
                {WEIGHT_LABELS[key] ?? key} —{" "}
                <span className="muted">{draft.weights[key].toFixed(2)}</span>
              </label>
              <input
                id={`w-${key}`}
                type="range"
                min={0}
                max={1}
                step={0.05}
                value={draft.weights[key]}
                onChange={(e) => patchWeight(key, Number(e.target.value))}
              />
            </div>
          ))}
        </section>

        <div className="row">
          <button type="submit" className="primary" disabled={save.running}>
            {save.running ? "Saving…" : "Save settings"}
          </button>
          <button type="button" onClick={() => setDraft(loaded)} disabled={save.running}>
            Revert
          </button>
        </div>
        {save.error ? <ErrorNotice error={save.error} onRetry={save.clear} /> : null}
        {save.message ? <p className="ok">{save.message}</p> : null}
      </form>

      <section className="panel">
        <h3>Cache management</h3>
        <p className="muted">
          Clearing the API response cache forces a full re-fetch on the next enrichment run.
          Genre aliases and derived data are preserved.
        </p>
        <div className="row">
          <button
            type="button"
            disabled={cacheAction.running}
            onClick={() =>
              void cacheAction.run(clearCache, (result) =>
                `${result.rowsDeleted} cached responses cleared.`,
              )
            }
          >
            {cacheAction.running ? "Clearing…" : "Clear API cache"}
          </button>
        </div>
        {cacheAction.error ? (
          <ErrorNotice error={cacheAction.error} onRetry={cacheAction.clear} />
        ) : null}
        {cacheAction.message ? <p className="ok">{cacheAction.message}</p> : null}
      </section>

      <section className="panel">
        <h3>Export database</h3>
        <p className="muted">
          Copies the SQLite database to a path you choose. Useful for backups or transferring
          your analysis to another machine.
        </p>
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            const input = (
              e.currentTarget.elements.namedItem("destPath") as HTMLInputElement
            ).value;
            if (!input.trim()) return;
            void exportAction.run(
              () => exportDatabase(input.trim()),
              () => `Database exported to ${input.trim()}.`,
            );
          }}
        >
          <input
            id="destPath"
            name="destPath"
            type="text"
            placeholder="/home/user/playlist-curator-backup.db"
            disabled={exportAction.running}
            style={{ flex: 1 }}
          />
          <button type="submit" className="primary" disabled={exportAction.running}>
            {exportAction.running ? "Exporting…" : "Export"}
          </button>
        </form>
        {exportAction.error ? (
          <ErrorNotice error={exportAction.error} onRetry={exportAction.clear} />
        ) : null}
        {exportAction.message ? <p className="ok">{exportAction.message}</p> : null}
      </section>
    </div>
  );
}

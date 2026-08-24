import { useEffect, useState } from "react";
import { ErrorNotice } from "../components/ErrorNotice";
import {
  type LlmProvider,
  type Settings,
  llmStatus,
} from "../lib/ipc";
import { type Async, useAction, useAsync } from "../lib/useAsync";

type Props = {
  settings: Async<Settings>;
  onSaveSettings: (next: Settings) => Promise<void>;
};

const PROVIDERS: { value: LlmProvider; label: string }[] = [
  { value: "disabled", label: "Disabled (deterministic parser only)" },
  { value: "ollama", label: "Ollama (local)" },
  { value: "anthropic", label: "Anthropic API" },
];

export function SettingsScreen({ settings, onSaveSettings }: Props) {
  const loaded = settings.state.status === "success" ? settings.state.data : null;
  const [draft, setDraft] = useState<Settings | null>(null);
  useEffect(() => setDraft(loaded), [loaded]);

  const llm = useAsync(llmStatus, []);
  const save = useAction();

  function patch(next: Partial<Settings>) {
    setDraft((current) => (current ? { ...current, ...next } : current));
  }

  function patchLlm(next: Partial<Settings["llm"]>) {
    setDraft((current) =>
      current ? { ...current, llm: { ...current.llm, ...next } } : current,
    );
  }

  if (settings.state.status === "loading") {
    return (
      <div className="screen">
        <h2>Settings</h2>
        <p aria-live="polite">Loading settings…</p>
      </div>
    );
  }

  if (settings.state.status === "error") {
    return (
      <div className="screen">
        <h2>Settings</h2>
        <ErrorNotice error={settings.state.error} onRetry={settings.reload} />
      </div>
    );
  }

  if (!draft) {
    return null;
  }

  return (
    <div className="screen">
      <h2>Settings</h2>

      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save.run(
            async () => {
              await onSaveSettings(draft);
              llm.reload();
            },
            () => "Settings saved.",
          );
        }}
      >
        <section className="panel">
          <h3>Credentials</h3>

          <div className="field">
            <label htmlFor="clientId">Spotify Client ID</label>
            <input
              id="clientId"
              type="text"
              autoComplete="off"
              spellCheck={false}
              value={draft.spotifyClientId ?? ""}
              onChange={(e) => patch({ spotifyClientId: e.target.value || null })}
            />
            <p className="hint">
              A public client — there is no secret, PKCE is mandatory. Create the app at
              developer.spotify.com and register the loopback redirect URI{" "}
              <code>http://127.0.0.1:14523/callback</code>.
            </p>
          </div>

          <div className="field">
            <label htmlFor="lastfm">Last.fm API key</label>
            <input
              id="lastfm"
              type="password"
              autoComplete="off"
              value={draft.lastfmApiKey ?? ""}
              onChange={(e) => patch({ lastfmApiKey: e.target.value || null })}
            />
            <p className="hint">Optional. Without it, Last.fm tags are skipped.</p>
          </div>

          <div className="field">
            <label htmlFor="discogs">Discogs personal access token</label>
            <input
              id="discogs"
              type="password"
              autoComplete="off"
              value={draft.discogsToken ?? ""}
              onChange={(e) => patch({ discogsToken: e.target.value || null })}
            />
            <p className="hint">Optional. Discogs requires a token even for read-only access.</p>
          </div>

          <div className="field">
            <label htmlFor="mbContact">MusicBrainz contact email</label>
            <input
              id="mbContact"
              type="email"
              autoComplete="email"
              value={draft.mbContactEmail ?? ""}
              onChange={(e) => patch({ mbContactEmail: e.target.value || null })}
            />
            <p className="hint">
              Required by MusicBrainz policy. Sent in the User-Agent header so they can reach you
              if the app exceeds rate limits.
            </p>
          </div>
        </section>

        <section className="panel">
          <h3>LLM configuration</h3>

          <fieldset className="field">
            <legend>Genre-resolution LLM</legend>
            <label htmlFor="provider">Provider</label>
            <select
              id="provider"
              value={draft.llm.provider}
              onChange={(e) => patchLlm({ provider: e.target.value as LlmProvider })}
            >
              {PROVIDERS.map((p) => (
                <option key={p.value} value={p.value}>
                  {p.label}
                </option>
              ))}
            </select>
            <p className="hint">
              The LLM only names leftover unknown tags and parses free-text queries. Every screen
              works with it disabled.
            </p>

            {draft.llm.provider === "ollama" ? (
              <>
                <label htmlFor="ollamaUrl">Ollama URL</label>
                <input
                  id="ollamaUrl"
                  type="text"
                  value={draft.llm.ollamaUrl}
                  onChange={(e) => patchLlm({ ollamaUrl: e.target.value })}
                />
                <label htmlFor="ollamaModel">Ollama model</label>
                <input
                  id="ollamaModel"
                  type="text"
                  value={draft.llm.ollamaModel}
                  onChange={(e) => patchLlm({ ollamaModel: e.target.value })}
                />
              </>
            ) : null}

            {draft.llm.provider === "anthropic" ? (
              <>
                <label htmlFor="anthropicModel">Anthropic model</label>
                <input
                  id="anthropicModel"
                  type="text"
                  value={draft.llm.anthropicModel}
                  onChange={(e) => patchLlm({ anthropicModel: e.target.value })}
                />
                <label htmlFor="anthropicKey">Anthropic API key</label>
                <input
                  id="anthropicKey"
                  type="password"
                  autoComplete="off"
                  value={draft.llm.anthropicApiKey ?? ""}
                  onChange={(e) => patchLlm({ anthropicApiKey: e.target.value || null })}
                />
              </>
            ) : null}
          </fieldset>

          <div style={{ marginTop: 8 }}>
            {llm.state.status === "loading" ? (
              <p className="muted">LLM status: Checking…</p>
            ) : null}
            {llm.state.status === "error" ? (
              <ErrorNotice error={llm.state.error} onRetry={llm.reload} />
            ) : null}
            {llm.state.status === "success" ? (
              <p>
                <span
                  className={llm.state.data.available ? "dot dot-ok" : "dot dot-off"}
                  aria-hidden
                />{" "}
                {llm.state.data.provider}:{" "}
                {llm.state.data.available ? "available" : "unavailable"} —{" "}
                {llm.state.data.detail}
              </p>
            ) : null}
          </div>
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
    </div>
  );
}

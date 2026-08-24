import { useEffect, useState } from "react";
import { ErrorNotice } from "../components/ErrorNotice";
import {
  type ConnectionStatus,
  type LlmProvider,
  type Settings,
  llmStatus,
  spotifyLogin,
  spotifyLogout,
} from "../lib/ipc";
import type { RouteName } from "../lib/router";
import { useAction, useAsync, type Async } from "../lib/useAsync";

type Props = {
  status: Async<ConnectionStatus>;
  settings: Async<Settings>;
  onSaveSettings: (next: Settings) => Promise<void>;
  navigate: (route: RouteName) => void;
};

const PROVIDERS: { value: LlmProvider; label: string }[] = [
  { value: "disabled", label: "Disabled (deterministic parser only)" },
  { value: "ollama", label: "Ollama (local)" },
  { value: "anthropic", label: "Anthropic API" },
];

export function Connect({ status, settings, onSaveSettings, navigate }: Props) {
  const loaded = settings.state.status === "success" ? settings.state.data : null;
  const [draft, setDraft] = useState<Settings | null>(null);
  useEffect(() => setDraft(loaded), [loaded]);

  const llm = useAsync(llmStatus, []);
  const save = useAction();
  const login = useAction();
  const logout = useAction();

  const conn = status.state.status === "success" ? status.state.data : null;

  function patch(next: Partial<Settings>) {
    setDraft((current) => (current ? { ...current, ...next } : current));
  }

  function patchLlm(next: Partial<Settings["llm"]>) {
    setDraft((current) =>
      current ? { ...current, llm: { ...current.llm, ...next } } : current,
    );
  }

  return (
    <div className="screen">
      <h2>Connect</h2>

      <section className="panel">
        <h3>Spotify</h3>
        {status.state.status === "loading" ? <p aria-live="polite">Checking connection…</p> : null}
        {status.state.status === "error" ? (
          <ErrorNotice
            error={status.state.error}
            onRetry={status.reload}
            onGoSettings={() => navigate("settings")}
          />
        ) : null}

        {conn ? (
          <>
            <dl className="kv">
              <div>
                <dt>Status</dt>
                <dd>{conn.connected ? "Connected" : "Not connected"}</dd>
              </div>
              <div>
                <dt>Client ID</dt>
                <dd>{conn.clientIdConfigured ? "Configured" : "Missing"}</dd>
              </div>
              <div>
                <dt>Account</dt>
                <dd>
                  {conn.user
                    ? `${conn.user.displayName ?? conn.user.id}${
                        conn.user.product ? ` (${conn.user.product})` : ""
                      }`
                    : "—"}
                </dd>
              </div>
              <div>
                <dt>Token store</dt>
                <dd>
                  {conn.tokenStore === "keyring"
                    ? "OS credential vault (keyring)"
                    : "Encrypted-at-rest file (0600) in the data directory"}
                </dd>
              </div>
            </dl>

            {conn.premiumWarning ? (
              <div className="notice notice-warn" role="status">
                <p className="notice-title">Premium required for Development Mode</p>
                <p>
                  Spotify apps that are not quota-extended run in Development Mode, and Development
                  Mode requires the <strong>app owner</strong> to hold a Premium subscription.
                  Playlist reads and writes will fail on a free account even though login succeeds.
                </p>
              </div>
            ) : null}

            <p className="muted">
              Logging in opens your system browser. The app runs the PKCE flow against a loopback
              listener on <code>127.0.0.1:14523</code>, so the redirect URI registered in your
              Spotify app must be exactly <code>http://127.0.0.1:14523/callback</code> —{" "}
              <code>localhost</code> is rejected by Spotify. No token ever reaches this window; the
              Rust side stores it in the vault above.
            </p>

            <div className="row">
              <button
                type="button"
                className="primary"
                disabled={login.running || !conn.clientIdConfigured}
                onClick={() =>
                  void login.run(spotifyLogin, (user) => {
                    status.reload();
                    return `Connected as ${user?.displayName ?? user?.id ?? "unknown"}.`;
                  })
                }
              >
                {login.running ? "Waiting for your approval in the browser…" : "Log in to Spotify"}
              </button>
              <button
                type="button"
                disabled={logout.running || !conn.connected}
                onClick={() =>
                  void logout.run(spotifyLogout, () => {
                    status.reload();
                    return "Tokens cleared.";
                  })
                }
              >
                Log out
              </button>
            </div>
            {!conn.clientIdConfigured ? (
              <p className="muted">Set a Client ID below before logging in.</p>
            ) : null}
            {login.error ? <ErrorNotice error={login.error} onRetry={login.clear} /> : null}
            {logout.error ? <ErrorNotice error={logout.error} onRetry={logout.clear} /> : null}
            {login.message ? <p className="ok">{login.message}</p> : null}
            {logout.message ? <p className="ok">{logout.message}</p> : null}
          </>
        ) : null}
      </section>

      <section className="panel">
        <h3>Credentials</h3>
        {settings.state.status === "loading" ? <p aria-live="polite">Loading settings…</p> : null}
        {settings.state.status === "error" ? (
          <ErrorNotice error={settings.state.error} onRetry={settings.reload} />
        ) : null}

        {draft ? (
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void save.run(
                async () => {
                  await onSaveSettings(draft);
                },
                () => {
                  status.reload();
                  llm.reload();
                  return "Saved.";
                },
              );
            }}
          >
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
                developer.spotify.com and register the loopback redirect URI.
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
                if the app exceeds rate limits. Use a personal email, not a work address.
              </p>
            </div>

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

            <div className="row">
              <button type="submit" className="primary" disabled={save.running}>
                {save.running ? "Saving…" : "Save credentials"}
              </button>
              <button type="button" onClick={() => setDraft(loaded)} disabled={save.running}>
                Revert
              </button>
            </div>
            {save.error ? <ErrorNotice error={save.error} onRetry={save.clear} /> : null}
            {save.message ? <p className="ok">{save.message}</p> : null}
          </form>
        ) : null}
      </section>

      <section className="panel">
        <h3>LLM status</h3>
        {llm.state.status === "loading" ? <p aria-live="polite">Checking…</p> : null}
        {llm.state.status === "error" ? (
          <ErrorNotice error={llm.state.error} onRetry={llm.reload} />
        ) : null}
        {llm.state.status === "success" ? (
          <p>
            <span className={llm.state.data.available ? "dot dot-ok" : "dot dot-off"} aria-hidden />
            {llm.state.data.provider}: {llm.state.data.available ? "available" : "unavailable"} —{" "}
            {llm.state.data.detail}
          </p>
        ) : null}
      </section>
    </div>
  );
}

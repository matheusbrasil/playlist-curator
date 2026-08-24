import { useEffect, useRef, useState } from "react";
import { ErrorNotice } from "./ErrorNotice";
import {
  type ConnectionStatus,
  connectionStatus,
  spotifyLogin,
  spotifyLogout,
} from "../lib/ipc";
import { useAction } from "../lib/useAsync";

export function SpotifyStatusButton() {
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  const login = useAction();
  const logout = useAction();
  const popoverRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);

  async function loadStatus() {
    try {
      const result = await connectionStatus();
      setStatus(result);
    } catch {
      // ignore — status widget failing shouldn't crash the app
    }
  }

  useEffect(() => {
    void loadStatus();
  }, []);

  useEffect(() => {
    if (!open) return;
    function handleMousedown(e: MouseEvent) {
      if (
        popoverRef.current &&
        !popoverRef.current.contains(e.target as Node) &&
        buttonRef.current &&
        !buttonRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handleMousedown);
    return () => document.removeEventListener("mousedown", handleMousedown);
  }, [open]);

  const connected = status?.connected ?? false;
  const clientIdConfigured = status?.clientIdConfigured ?? false;

  return (
    <div style={{ position: "relative", marginLeft: "auto" }}>
      <button
        ref={buttonRef}
        type="button"
        className="header-status"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        aria-haspopup="dialog"
        style={{
          cursor: "pointer",
          background: "none",
          border: "none",
          padding: "0 20px",
          display: "flex",
          alignItems: "center",
          gap: 7,
          color: "var(--text-dim)",
          fontSize: 12,
          whiteSpace: "nowrap",
        }}
      >
        <span className={connected ? "dot dot-ok" : "dot dot-off"} aria-hidden="true" />
        Spotify
      </button>

      {open ? (
        <div
          ref={popoverRef}
          role="dialog"
          aria-label="Spotify connection"
          style={{
            position: "absolute",
            right: 0,
            top: "100%",
            zIndex: 100,
            background: "var(--surface2)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius-lg)",
            padding: 16,
            minWidth: 300,
            boxShadow: "var(--shadow)",
          }}
        >
          <p style={{ fontWeight: 600, marginBottom: 8 }}>
            {connected ? "Connected" : "Not connected"}
          </p>

          {status?.user ? (
            <p className="muted" style={{ marginBottom: 8 }}>
              {status.user.displayName ?? status.user.id}
              {status.user.product ? ` (${status.user.product})` : ""}
            </p>
          ) : null}

          {status ? (
            <p className="muted" style={{ fontSize: 12, marginBottom: 8 }}>
              Token store:{" "}
              {status.tokenStore === "keyring"
                ? "OS credential vault (keyring)"
                : "Encrypted file in data directory"}
            </p>
          ) : null}

          {status?.premiumWarning ? (
            <div className="notice notice-warn" role="status" style={{ marginBottom: 8 }}>
              <p className="notice-title">Premium required for Development Mode</p>
              <p>
                Spotify Development Mode requires the app owner to hold Premium. Playlist reads and
                writes will fail on a free account even though login succeeds.
              </p>
            </div>
          ) : null}

          <p className="muted" style={{ fontSize: 11, marginBottom: 12 }}>
            Opens system browser. Redirect URI must be{" "}
            <code>http://127.0.0.1:14523/callback</code>
          </p>

          {!connected ? (
            <button
              type="button"
              className="primary"
              disabled={login.running || !clientIdConfigured}
              onClick={() =>
                void login.run(spotifyLogin, (user) => {
                  void loadStatus();
                  return `Connected as ${user?.displayName ?? user?.id ?? "unknown"}.`;
                })
              }
            >
              {login.running ? "Waiting for browser…" : "Log in"}
            </button>
          ) : (
            <button
              type="button"
              disabled={logout.running}
              onClick={() =>
                void logout.run(spotifyLogout, () => {
                  void loadStatus();
                  return "Tokens cleared.";
                })
              }
            >
              {logout.running ? "Logging out…" : "Log out"}
            </button>
          )}

          {!clientIdConfigured ? (
            <p className="muted" style={{ marginTop: 8, fontSize: 12 }}>
              Set a Client ID in Settings before logging in.
            </p>
          ) : null}

          {login.message ? (
            <p className="ok" style={{ marginTop: 8 }}>
              {login.message}
            </p>
          ) : null}
          {logout.message ? (
            <p className="ok" style={{ marginTop: 8 }}>
              {logout.message}
            </p>
          ) : null}
          {login.error ? (
            <ErrorNotice error={login.error} onRetry={login.clear} />
          ) : null}
          {logout.error ? (
            <ErrorNotice error={logout.error} onRetry={logout.clear} />
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

import type { CoreError } from "../lib/ipc";
import { isRetryable } from "../lib/ipc";

type Props = {
  error: CoreError;
  onRetry?: (() => void) | undefined;
  onGoConnect?: (() => void) | undefined;
  onGoSettings?: (() => void) | undefined;
};

/** Advice per error `kind`, so the UI never dead-ends on a failure. */
function advice(kind: string): string {
  switch (kind) {
    case "not_authenticated":
      return "You are not connected to Spotify. Connect first.";
    case "quota_exceeded":
      return "Spotify's quota for this developer account is spent. Retrying will not help — wait for it to reset.";
    case "spotify_api":
      return "Spotify rejected the request. Retrying often works.";
    case "config":
      return "Something is not configured yet. Open Settings.";
    case "credential":
      return "The OS credential vault is unavailable, so tokens fall back to a 0600 file in the data directory.";
    case "invalid_filter":
      return "That filter cannot be executed as written.";
    case "upstream":
      return "A metadata source failed. Enrichment continues without it; run it again later to fill the gap.";
    default:
      return "Unexpected failure.";
  }
}

export function ErrorNotice({ error, onRetry, onGoConnect, onGoSettings }: Props) {
  const showConnect = error.kind === "not_authenticated" && onGoConnect;
  const showSettings = error.kind === "config" && onGoSettings;
  const showRetry = onRetry && isRetryable(error);

  return (
    <div className="notice notice-error" role="alert">
      <p className="notice-title">
        <span className="kind-tag">{error.kind}</span> {advice(error.kind)}
      </p>
      <p className="notice-message">{error.message}</p>
      <div className="row">
        {showRetry ? (
          <button type="button" onClick={onRetry}>
            Retry
          </button>
        ) : null}
        {showConnect ? (
          <button type="button" onClick={onGoConnect}>
            Go to Connect
          </button>
        ) : null}
        {showSettings ? (
          <button type="button" onClick={onGoSettings}>
            Open Settings
          </button>
        ) : null}
      </div>
    </div>
  );
}

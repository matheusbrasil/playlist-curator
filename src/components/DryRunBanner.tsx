type Props = {
  dryRun: boolean;
  onOpenSettings?: (() => void) | undefined;
};

/** The user must never be surprised by a write to their real Spotify account. */
export function DryRunBanner({ dryRun, onOpenSettings }: Props) {
  return (
    <div className={dryRun ? "banner banner-dry" : "banner banner-live"} role="status">
      <strong>{dryRun ? "DRY RUN" : "LIVE WRITES"}</strong>
      <span>
        {dryRun
          ? "Nothing is written to Spotify. Created playlists are simulated and reported only."
          : "Creating a playlist writes it to your real Spotify account."}
      </span>
      {onOpenSettings ? (
        <button type="button" className="link" onClick={onOpenSettings}>
          Change in Settings
        </button>
      ) : null}
    </div>
  );
}

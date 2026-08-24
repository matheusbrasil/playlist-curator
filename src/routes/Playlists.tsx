import { useState } from "react";
import { ErrorNotice } from "../components/ErrorNotice";
import {
  type ImportStats,
  importPlaylist,
  listPlaylists,
  syncPlaylists,
} from "../lib/ipc";
import { dateTime, percent } from "../lib/format";
import type { RouteName } from "../lib/router";
import { useAction, useAsync } from "../lib/useAsync";

type Props = {
  selectedPlaylistId: string | null;
  onSelect: (playlistId: string) => void;
  navigate: (route: RouteName) => void;
};

export function Playlists({ selectedPlaylistId, onSelect, navigate }: Props) {
  const playlists = useAsync(listPlaylists, []);
  const sync = useAction();
  const importAction = useAction();
  const [importing, setImporting] = useState<string | null>(null);
  const [stats, setStats] = useState<Record<string, ImportStats>>({});
  const [search, setSearch] = useState("");

  const rows = playlists.state.status === "success" ? playlists.state.data : [];

  const filtered = search
    ? rows.filter(
        (p) =>
          p.name.toLowerCase().includes(search.toLowerCase()) ||
          (p.owner ?? "").toLowerCase().includes(search.toLowerCase()),
      )
    : rows;

  async function runImport(playlistId: string) {
    setImporting(playlistId);
    const result = await importAction.run(() => importPlaylist(playlistId));
    setImporting(null);
    if (result) {
      setStats((current) => ({ ...current, [playlistId]: result }));
      onSelect(playlistId);
      playlists.reload();
    }
  }

  return (
    <div className="screen--split">
      <div className="page-toolbar">
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <button
            type="button"
            className="primary"
            disabled={sync.running}
            onClick={() =>
              void sync.run(syncPlaylists, (fetched) => {
                if (fetched) playlists.set(fetched);
                return `${fetched?.length ?? 0} playlists from Spotify.`;
              })
            }
          >
            {sync.running ? "Syncing from Spotify…" : "Sync from Spotify"}
          </button>
          <button
            type="button"
            onClick={playlists.reload}
            disabled={playlists.state.status === "loading"}
          >
            Reload cached list
          </button>
          <input
            type="search"
            placeholder="Search playlists..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            style={{ flex: 1 }}
          />
          {sync.message ? <span className="ok">{sync.message}</span> : null}
        </div>
      </div>

      <div className="page-body">
        <h2>Playlists</h2>

        {sync.error ? (
          <ErrorNotice
            error={sync.error}
            onRetry={() => void sync.run(syncPlaylists, (f) => (f ? (playlists.set(f), "") : ""))}
            onGoConnect={() => navigate("settings")}
            onGoSettings={() => navigate("settings")}
          />
        ) : null}
        {importAction.error ? (
          <ErrorNotice
            error={importAction.error}
            onGoConnect={() => navigate("settings")}
            onRetry={importAction.clear}
          />
        ) : null}

        {playlists.state.status === "loading" ? <p aria-live="polite">Loading playlists…</p> : null}
        {playlists.state.status === "error" ? (
          <ErrorNotice
            error={playlists.state.error}
            onRetry={playlists.reload}
            onGoConnect={() => navigate("settings")}
          />
        ) : null}

        {playlists.state.status === "success" && rows.length === 0 ? (
          <p className="muted">
            Nothing cached yet. Sync from Spotify to fetch your playlists — the list is stored
            locally, so it loads instantly next time.
          </p>
        ) : null}

        {filtered.length > 0 ? (
          <div className="table-scroll">
            <table className="data-table">
              <caption>
                {search
                  ? `${filtered.length} of ${rows.length} playlists`
                  : `${rows.length} playlists in the local cache`}
              </caption>
              <thead>
                <tr>
                  <th scope="col">Playlist</th>
                  <th scope="col" className="col-hide-md">Owner</th>
                  <th scope="col">Tracks</th>
                  <th scope="col">State</th>
                  <th scope="col" className="col-hide-md">Last import</th>
                  <th scope="col">Actions</th>
                </tr>
              </thead>
              <tbody>
                {filtered.map((playlist) => {
                  const stat = stats[playlist.spotifyId];
                  const analysed = playlist.syncedAt !== null;
                  const isSelected = playlist.spotifyId === selectedPlaylistId;
                  return (
                    <tr key={playlist.spotifyId} className={isSelected ? "row-selected" : undefined}>
                      <th scope="row">
                        {playlist.name}
                        {isSelected ? <span className="badge">selected</span> : null}
                      </th>
                      <td className="col-hide-md">{playlist.owner ?? "—"}</td>
                      <td>{playlist.trackCount ?? "—"}</td>
                      <td>
                        <span className={analysed ? "badge badge-high" : "badge badge-low"}>
                          {analysed ? "analysed" : "never analysed"}
                        </span>
                        {stat ? (
                          <span
                            className="badge"
                            title={`${stat.withIsrc} of ${stat.tracksImported} imported tracks carry an ISRC. Low coverage weakens MusicBrainz matching.`}
                          >
                            ISRC{" "}
                            {percent(
                              stat.tracksImported > 0 ? stat.withIsrc / stat.tracksImported : 0,
                            )}
                          </span>
                        ) : null}
                      </td>
                      <td className="col-hide-md">{dateTime(playlist.syncedAt)}</td>
                      <td>
                        <div className="table-actions">
                          <button
                            type="button"
                            onClick={() => void runImport(playlist.spotifyId)}
                            disabled={importing !== null}
                          >
                            {importing === playlist.spotifyId ? "Importing…" : "Import"}
                          </button>
                          <button type="button" onClick={() => onSelect(playlist.spotifyId)}>
                            Select
                          </button>
                          <button
                            type="button"
                            onClick={() => {
                              onSelect(playlist.spotifyId);
                              navigate("analysis");
                            }}
                          >
                            Analyse
                          </button>
                          {stat ? (
                            <p className="muted">
                              {stat.tracksImported} tracks, {stat.artistsImported} artists from{" "}
                              {stat.itemsSeen} items. Skipped: {stat.skippedLocal} local,{" "}
                              {stat.skippedEpisodes} episodes, {stat.skippedUnresolvable} unresolvable.
                            </p>
                          ) : null}
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        ) : null}

        {rows.length > 0 && filtered.length === 0 ? (
          <p className="muted">No playlists match "{search}".</p>
        ) : null}
      </div>
    </div>
  );
}

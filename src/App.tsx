import { useCallback, useState } from "react";
import { DryRunBanner } from "./components/DryRunBanner";
import { connectionStatus, getSettings, saveSettings, type Settings } from "./lib/ipc";
import { ROUTES, ROUTE_TITLES, useHashRoute, type RouteName } from "./lib/router";
import { useAsync } from "./lib/useAsync";
import { Analysis } from "./routes/Analysis";
import { Connect } from "./routes/Connect";
import { Playlists } from "./routes/Playlists";
import { SettingsScreen } from "./routes/Settings";
import { Suggestions } from "./routes/Suggestions";

const SELECTED_KEY = "pc.selectedPlaylistId";

export function App() {
  const [route, navigate] = useHashRoute();
  const settings = useAsync(getSettings, []);
  const status = useAsync(connectionStatus, []);

  const [selectedPlaylistId, setSelectedPlaylistId] = useState<string | null>(() =>
    window.localStorage.getItem(SELECTED_KEY),
  );

  const selectPlaylist = useCallback((id: string) => {
    window.localStorage.setItem(SELECTED_KEY, id);
    setSelectedPlaylistId(id);
  }, []);

  const persistSettings = useCallback(
    async (next: Settings) => {
      await saveSettings(next);
      settings.set(next);
    },
    [settings],
  );

  const loaded = settings.state.status === "success" ? settings.state.data : null;
  const connected = status.state.status === "success" ? status.state.data.connected : false;

  return (
    <div className="app">
      <header className="app-header">
        <h1>Playlist Curator</h1>
        <nav aria-label="Screens">
          <ul>
            {ROUTES.map((name: RouteName) => (
              <li key={name}>
                <button
                  type="button"
                  className={route === name ? "tab tab-active" : "tab"}
                  aria-current={route === name ? "page" : undefined}
                  onClick={() => navigate(name)}
                >
                  {ROUTE_TITLES[name]}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <p className="header-status">
          <span className={connected ? "dot dot-ok" : "dot dot-off"} aria-hidden="true" />
          {connected ? "Spotify connected" : "Not connected"}
        </p>
      </header>

      <DryRunBanner
        dryRun={loaded ? loaded.dryRun : true}
        onOpenSettings={route === "settings" ? undefined : () => navigate("settings")}
      />

      <main className="app-main">
        {route === "connect" ? (
          <Connect
            status={status}
            settings={settings}
            onSaveSettings={persistSettings}
            navigate={navigate}
          />
        ) : null}
        {route === "playlists" ? (
          <Playlists
            selectedPlaylistId={selectedPlaylistId}
            onSelect={selectPlaylist}
            navigate={navigate}
          />
        ) : null}
        {route === "analysis" ? (
          <Analysis playlistId={selectedPlaylistId} settings={loaded} navigate={navigate} />
        ) : null}
        {route === "suggestions" ? (
          <Suggestions playlistId={selectedPlaylistId} settings={loaded} navigate={navigate} />
        ) : null}
        {route === "settings" ? (
          <SettingsScreen settings={settings} onSaveSettings={persistSettings} />
        ) : null}
      </main>
    </div>
  );
}

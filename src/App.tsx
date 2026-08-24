import { useCallback, useState } from "react";
import { SpotifyStatusButton } from "./components/SpotifyStatusButton";
import { getSettings, saveSettings, type Settings } from "./lib/ipc";
import { ROUTES, ROUTE_TITLES, useHashRoute, type RouteName } from "./lib/router";
import { useAsync } from "./lib/useAsync";
import { Advanced } from "./routes/Advanced";
import { Analysis } from "./routes/Analysis";
import { Playlists } from "./routes/Playlists";
import { SettingsScreen } from "./routes/Settings";
import { Suggestions } from "./routes/Suggestions";

const SELECTED_KEY = "pc.selectedPlaylistId";

export function App() {
  const [route, navigate] = useHashRoute();
  const settings = useAsync(getSettings, []);

  const [selectedPlaylistId, setSelectedPlaylistId] = useState<string | null>(() =>
    window.localStorage.getItem(SELECTED_KEY),
  );
  const [enrichRunning, setEnrichRunning] = useState(false);

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
                  disabled={enrichRunning && name !== route}
                  title={enrichRunning && name !== route ? "Enrichment in progress — finish or wait" : undefined}
                  onClick={() => navigate(name)}
                >
                  {ROUTE_TITLES[name]}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <SpotifyStatusButton />
      </header>

      <main className="app-main">
        {route === "playlists" ? (
          <Playlists
            selectedPlaylistId={selectedPlaylistId}
            onSelect={selectPlaylist}
            navigate={navigate}
          />
        ) : null}
        {route === "analysis" ? (
          <Analysis
            playlistId={selectedPlaylistId}
            settings={loaded}
            navigate={navigate}
            onEnrichStart={() => setEnrichRunning(true)}
            onEnrichEnd={() => setEnrichRunning(false)}
          />
        ) : null}
        {route === "suggestions" ? (
          <Suggestions playlistId={selectedPlaylistId} settings={loaded} navigate={navigate} />
        ) : null}
        {route === "settings" ? (
          <SettingsScreen settings={settings} onSaveSettings={persistSettings} />
        ) : null}
        {route === "advanced" ? (
          <Advanced settings={settings} onSaveSettings={persistSettings} />
        ) : null}
      </main>
    </div>
  );
}

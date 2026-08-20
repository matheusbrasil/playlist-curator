import { useCallback, useEffect, useState } from "react";

export const ROUTES = ["connect", "playlists", "analysis", "suggestions", "settings"] as const;

export type RouteName = (typeof ROUTES)[number];

export const ROUTE_TITLES: Record<RouteName, string> = {
  connect: "Connect",
  playlists: "Playlists",
  analysis: "Analysis",
  suggestions: "Suggestions",
  settings: "Settings",
};

function parseHash(hash: string): RouteName {
  const name = hash.replace(/^#\/?/, "");
  return (ROUTES as readonly string[]).includes(name) ? (name as RouteName) : "connect";
}

/** Hash routing keeps deep links working inside the webview with no server. */
export function useHashRoute(): [RouteName, (next: RouteName) => void] {
  const [route, setRoute] = useState<RouteName>(() => parseHash(window.location.hash));

  useEffect(() => {
    const onChange = () => setRoute(parseHash(window.location.hash));
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);

  const navigate = useCallback((next: RouteName) => {
    window.location.hash = `#/${next}`;
  }, []);

  return [route, navigate];
}

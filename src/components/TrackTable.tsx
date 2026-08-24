import { useMemo, useState, type ReactNode } from "react";
import type { AnalysedTrack } from "../lib/ipc";
import { confidenceLevel, score, sourceLabel } from "../lib/format";

type Props = {
  tracks: AnalysedTrack[];
  caption: string;
  renderActions?: ((track: AnalysedTrack) => ReactNode) | undefined;
};

type SortState = { col: string; dir: "asc" | "desc" } | null;

/**
 * A track's confidence is the mean of the signals that actually exist for it:
 * the top genre score and the origin confidence. A track with neither is
 * "unknown" rather than 0, because absence is not disagreement.
 */
function trackConfidence(track: AnalysedTrack): number | null {
  const parts: number[] = [];
  const top = track.genres[0];
  if (top) parts.push(top.score);
  if (track.origin) parts.push(track.origin.confidence);
  if (parts.length === 0) return null;
  return parts.reduce((a, b) => a + b, 0) / parts.length;
}

function ConfidenceBadge({ track }: { track: AnalysedTrack }) {
  const value = trackConfidence(track);
  const level = confidenceLevel(value);
  const text = value === null ? "no data" : `${level} ${score(value)}`;
  return (
    <span
      className={`badge badge-${level}`}
      title="Mean of the top genre score and the origin confidence"
    >
      {text}
    </span>
  );
}

function originText(track: AnalysedTrack): string {
  if (!track.origin) return "—";
  const { city, countryLabel, countryCode } = track.origin;
  const place = countryLabel ?? countryCode ?? "unknown";
  return city ? `${city}, ${place}` : place;
}

type ColHeaderProps = {
  label: string;
  colKey: string;
  sortable: boolean;
  filterable: boolean;
  sort: SortState;
  filters: Record<string, Set<string>>;
  uniqueVals: string[];
  onSort: (col: string) => void;
  onToggleDropdown: (col: string) => void;
  openDropdown: string | null;
  onToggleValue: (col: string, val: string) => void;
  onClearCol: (col: string) => void;
};

function ColHeader({
  label, colKey, sortable, filterable, sort, filters, uniqueVals,
  onSort, onToggleDropdown, openDropdown, onToggleValue, onClearCol,
}: ColHeaderProps) {
  const activeCount = filters[colKey]?.size ?? 0;
  const sortDir = sort?.col === colKey ? sort.dir : null;
  return (
    <th scope="col">
      <div className="col-header-inner">
        {sortable ? (
          <button className="col-sort-btn" onClick={() => onSort(colKey)}>
            {label}
            <span className="sort-icon">{sortDir === "asc" ? "↑" : sortDir === "desc" ? "↓" : "⇅"}</span>
          </button>
        ) : <span>{label}</span>}
        {filterable ? (
          <div className="col-filter-wrap">
            <button
              className={`col-filter-btn${activeCount > 0 ? " active" : ""}`}
              onClick={() => onToggleDropdown(colKey)}
            >
              ▼{activeCount > 0 ? ` ×${activeCount}` : ""}
            </button>
            {openDropdown === colKey ? (
              <div className="col-filter-dropdown">
                {uniqueVals.map(v => (
                  <label key={v} className="col-filter-item">
                    <input
                      type="checkbox"
                      checked={filters[colKey]?.has(v) ?? false}
                      onChange={() => onToggleValue(colKey, v)}
                    />
                    {v}
                  </label>
                ))}
                {activeCount > 0 ? (
                  <button onClick={() => onClearCol(colKey)}>Clear</button>
                ) : null}
              </div>
            ) : null}
          </div>
        ) : null}
      </div>
    </th>
  );
}

export function TrackTable({ tracks, caption, renderActions }: Props) {
  const [sort, setSort] = useState<SortState>(null);
  const [filters, setFilters] = useState<Record<string, Set<string>>>({});
  const [openDropdown, setOpenDropdown] = useState<string | null>(null);

  const uniqueValues = useMemo(() => ({
    artists: [...new Set(tracks.flatMap(t => t.artists.map(a => a.name)))].sort(),
    genres: [...new Set(tracks.flatMap(t => t.genres.slice(0, 3).map(g => g.slug)))].sort(),
    origin: [...new Set(
      tracks.map(t => t.origin?.countryLabel ?? t.origin?.countryCode).filter((x): x is string => Boolean(x))
    )].sort(),
    era: [...new Set(
      tracks.map(t => t.era?.decade ? `${t.era.decade}s` : t.era?.year ? String(t.era.year) : null)
            .filter((x): x is string => Boolean(x))
    )].sort(),
  }), [tracks]);

  const displayed = useMemo(() => {
    let result = tracks.filter(t => {
      const af = filters["artists"];
      if (af?.size && !t.artists.some(a => af.has(a.name))) return false;
      const gf = filters["genres"];
      if (gf?.size && !t.genres.slice(0, 3).some(g => gf.has(g.slug))) return false;
      const of_ = filters["origin"];
      const ol = t.origin?.countryLabel ?? t.origin?.countryCode ?? null;
      if (of_?.size && (!ol || !of_.has(ol))) return false;
      const ef = filters["era"];
      const el = t.era?.decade ? `${t.era.decade}s` : t.era?.year ? String(t.era.year) : null;
      if (ef?.size && (!el || !ef.has(el))) return false;
      return true;
    });
    if (sort) {
      const { col, dir } = sort;
      const mult = dir === "asc" ? 1 : -1;
      result = [...result].sort((a, b) => {
        let av = "", bv = "";
        if (col === "track") { av = a.name; bv = b.name; }
        else if (col === "artists") { av = a.artists[0]?.name ?? ""; bv = b.artists[0]?.name ?? ""; }
        else if (col === "genres") { av = a.genres[0]?.slug ?? ""; bv = b.genres[0]?.slug ?? ""; }
        else if (col === "origin") { av = a.origin?.countryLabel ?? a.origin?.countryCode ?? ""; bv = b.origin?.countryLabel ?? b.origin?.countryCode ?? ""; }
        else if (col === "era") { av = String(a.era?.decade ?? a.era?.year ?? 0); bv = String(b.era?.decade ?? b.era?.year ?? 0); }
        else if (col === "confidence") { av = String(trackConfidence(a) ?? -1); bv = String(trackConfidence(b) ?? -1); }
        return av.localeCompare(bv, undefined, { numeric: true }) * mult;
      });
    }
    return result;
  }, [tracks, filters, sort]);

  if (tracks.length === 0) {
    return <p className="muted">No tracks to show.</p>;
  }

  const anyFilter = Object.values(filters).some(s => s.size > 0);

  function handleSort(col: string) {
    setSort(prev => {
      if (prev?.col === col) return prev.dir === "asc" ? { col, dir: "desc" } : null;
      return { col, dir: "asc" };
    });
  }

  function handleToggleDropdown(col: string) {
    setOpenDropdown(prev => prev === col ? null : col);
  }

  function handleToggleValue(col: string, val: string) {
    setFilters(prev => {
      const existing = prev[col] ? new Set(prev[col]) : new Set<string>();
      if (existing.has(val)) existing.delete(val);
      else existing.add(val);
      return { ...prev, [col]: existing };
    });
  }

  function handleClearCol(col: string) {
    setFilters(prev => {
      const next = { ...prev };
      delete next[col];
      return next;
    });
  }

  const colProps = {
    sort, filters, openDropdown,
    onSort: handleSort,
    onToggleDropdown: handleToggleDropdown,
    onToggleValue: handleToggleValue,
    onClearCol: handleClearCol,
  };

  return (
    <div className="table-scroll">
      {anyFilter ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, paddingBottom: 8 }}>
          <span className="muted small">{displayed.length} of {tracks.length}</span>
          <button type="button" onClick={() => setFilters({})}>Clear all filters</button>
        </div>
      ) : null}
      <table className="data-table">
        <caption>{anyFilter ? `${displayed.length} of ${tracks.length} — ${caption}` : caption}</caption>
        <thead>
          <tr>
            <ColHeader label="Track" colKey="track" sortable filterable={false} uniqueVals={[]} {...colProps} />
            <ColHeader label="Artists" colKey="artists" sortable filterable uniqueVals={uniqueValues.artists} {...colProps} />
            <ColHeader label="Genres" colKey="genres" sortable filterable uniqueVals={uniqueValues.genres} {...colProps} />
            <ColHeader label="Origin" colKey="origin" sortable filterable uniqueVals={uniqueValues.origin} {...colProps} />
            <ColHeader label="Era" colKey="era" sortable filterable uniqueVals={uniqueValues.era} {...colProps} />
            <ColHeader label="Confidence" colKey="confidence" sortable filterable={false} uniqueVals={[]} {...colProps} />
            {renderActions ? <th scope="col">Actions</th> : null}
          </tr>
        </thead>
        <tbody>
          {displayed.map((track) => (
            <tr key={track.spotifyId} className={track.needsReview ? "row-review" : undefined}>
              <th scope="row">
                {track.name}
                {track.needsReview ? (
                  <span className="badge badge-review" title="Queued for review">
                    review
                  </span>
                ) : null}
                {track.isrc ? null : (
                  <span className="badge badge-low" title="No ISRC: matching is weaker">
                    no ISRC
                  </span>
                )}
              </th>
              <td>{track.artists.map((a) => a.name).join(", ")}</td>
              <td>
                {track.genres.length === 0 ? (
                  <span className="muted">—</span>
                ) : (
                  <ul className="inline-list">
                    {track.genres.slice(0, 3).map((g) => (
                      <li key={g.slug}>
                        {g.slug} <span className="muted">{score(g.score)}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </td>
              <td>
                {originText(track)}
                {track.origin ? (
                  <span className="muted source"> {sourceLabel(track.origin.source)}</span>
                ) : null}
              </td>
              <td>
                {track.era?.year ?? track.era?.decade ?? "—"}
                {track.era ? (
                  <span className="muted source"> {sourceLabel(track.era.source)}</span>
                ) : null}
              </td>
              <td>
                <ConfidenceBadge track={track} />
              </td>
              {renderActions ? <td>{renderActions(track)}</td> : null}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

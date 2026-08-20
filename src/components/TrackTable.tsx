import type { ReactNode } from "react";
import type { AnalysedTrack } from "../lib/ipc";
import { confidenceLevel, score, sourceLabel } from "../lib/format";

type Props = {
  tracks: AnalysedTrack[];
  caption: string;
  renderActions?: ((track: AnalysedTrack) => ReactNode) | undefined;
};

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

export function TrackTable({ tracks, caption, renderActions }: Props) {
  if (tracks.length === 0) {
    return <p className="muted">No tracks to show.</p>;
  }

  return (
    <div className="table-scroll">
      <table className="data-table">
        <caption>{caption}</caption>
        <thead>
          <tr>
            <th scope="col">Track</th>
            <th scope="col">Artists</th>
            <th scope="col">Genres</th>
            <th scope="col">Origin</th>
            <th scope="col">Era</th>
            <th scope="col">Confidence</th>
            {renderActions ? <th scope="col">Actions</th> : null}
          </tr>
        </thead>
        <tbody>
          {tracks.map((track) => (
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

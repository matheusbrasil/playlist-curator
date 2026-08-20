import type { SuggestionCard as Card } from "../lib/ipc";
import { score } from "../lib/format";

type Props = {
  card: Card;
  dryRun: boolean;
  selected: boolean;
  onOpen: (card: Card) => void;
  onCreate: (card: Card) => void;
  creating: boolean;
};

function filterSummary(card: Card): string {
  const { filter } = card;
  const parts: string[] = [];
  if (filter.genres.length > 0) {
    const mode = filter.genreMode === "any_with_children" ? " (incl. sub-genres)" : "";
    parts.push(`${filter.genres.join(" / ")}${mode}`);
  }
  if (filter.countries.length > 0) parts.push(filter.countries.join(", "));
  if (filter.yearRange) parts.push(`${filter.yearRange[0]}–${filter.yearRange[1]}`);
  if (filter.excludeNeedsReview) parts.push("reviewed only");
  return parts.length > 0 ? parts.join(" · ") : "no constraints";
}

export function SuggestionCard({
  card,
  dryRun,
  selected,
  onOpen,
  onCreate,
  creating,
}: Props) {
  const s = card.score;
  return (
    <article className={selected ? "card card-selected" : "card"} aria-label={card.proposedName}>
      <h3>{card.proposedName}</h3>
      <p className="card-desc">{card.description}</p>
      <p className="card-filter">{filterSummary(card)}</p>
      <dl className="card-stats">
        <div>
          <dt>Tracks</dt>
          <dd>{card.trackCount}</dd>
        </div>
        <div>
          <dt>Total</dt>
          <dd>{score(s.total)}</dd>
        </div>
        <div>
          <dt>Coherence</dt>
          <dd>{score(s.coherence)}</dd>
        </div>
        <div>
          <dt>Specificity</dt>
          <dd>{score(s.specificity)}</dd>
        </div>
        <div>
          <dt>Redundancy</dt>
          <dd>{score(s.redundancy)}</dd>
        </div>
        <div>
          <dt>Confidence</dt>
          <dd>{score(s.confidence)}</dd>
        </div>
      </dl>
      <div className="row">
        <button type="button" onClick={() => onOpen(card)} aria-expanded={selected}>
          {selected ? "Hide tracks" : "Show tracks"}
        </button>
        <button
          type="button"
          className={dryRun ? "primary dry" : "primary danger"}
          disabled={creating}
          onClick={() => onCreate(card)}
        >
          {creating
            ? "Working…"
            : dryRun
              ? "Create (dry run)"
              : "Create on Spotify"}
        </button>
      </div>
    </article>
  );
}

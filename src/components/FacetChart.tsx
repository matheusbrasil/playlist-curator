export type FacetItem = { key: string; label: string; count: number };

type Props = {
  title: string;
  items: FacetItem[];
  /** Optional click-through, e.g. "filter suggestions by this genre". */
  onSelect?: ((key: string) => void) | undefined;
  emptyHint?: string | undefined;
  maxRows?: number | undefined;
};

const ROW_H = 22;
const BAR_H = 14;
const LABEL_W = 132;
const COUNT_W = 44;
const WIDTH = 420;

/**
 * Horizontal bars in plain SVG. The graphic carries a full text description for
 * screen readers, and the same numbers are visible as text next to each bar, so
 * nothing is conveyed by length alone.
 */
export function FacetChart({ title, items, onSelect, emptyHint, maxRows = 12 }: Props) {
  const rows = items.slice(0, maxRows);
  const total = items.reduce((sum, i) => sum + i.count, 0);
  const peak = rows.reduce((m, i) => Math.max(m, i.count), 0);
  const barSpace = WIDTH - LABEL_W - COUNT_W - 8;

  if (rows.length === 0) {
    return (
      <section className="chart">
        <h3>{title}</h3>
        <p className="muted">{emptyHint ?? "No data yet."}</p>
      </section>
    );
  }

  const description = rows.map((r) => `${r.label}: ${r.count}`).join(", ");

  return (
    <section className="chart">
      <h3>{title}</h3>
      <svg
        role="img"
        aria-label={`${title}. ${description}.`}
        viewBox={`0 0 ${WIDTH} ${rows.length * ROW_H + 4}`}
        width="100%"
        height={rows.length * ROW_H + 4}
      >
        <title>{title}</title>
        <desc>{description}</desc>
        {rows.map((row, index) => {
          const y = index * ROW_H + 2;
          const width = peak > 0 ? Math.max(2, (row.count / peak) * barSpace) : 2;
          return (
            <g key={row.key}>
              <text x={0} y={y + BAR_H - 2} className="chart-label">
                {row.label}
              </text>
              <rect
                x={LABEL_W}
                y={y}
                width={width}
                height={BAR_H}
                rx={3}
                className="chart-bar"
              />
              <text x={LABEL_W + width + 6} y={y + BAR_H - 2} className="chart-count">
                {row.count}
              </text>
            </g>
          );
        })}
      </svg>
      {onSelect ? (
        <ul className="chart-actions">
          {rows.map((row) => (
            <li key={row.key}>
              <button
                type="button"
                className="chip"
                onClick={() => onSelect(row.key)}
                aria-label={`Filter by ${row.label} (${row.count} tracks)`}
              >
                {row.label}
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      {items.length > rows.length ? (
        <p className="muted">
          Showing the top {rows.length} of {items.length} ({total} tracks counted).
        </p>
      ) : null}
    </section>
  );
}

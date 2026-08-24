export type FacetItem = { key: string; label: string; count: number };

type Props = {
  title: string;
  items: FacetItem[];
  onSelect?: ((key: string) => void) | undefined;
  emptyHint?: string | undefined;
  maxRows?: number | undefined;
};

const ROW_H  = 26;
const BAR_H  = 14;
const LABEL_W = 110;
const COUNT_W = 38;
const WIDTH   = 360;
const BAR_AREA = WIDTH - LABEL_W - COUNT_W - 6;

export function FacetChart({ title, items, onSelect, emptyHint, maxRows = 10 }: Props) {
  const rows  = items.slice(0, maxRows);
  const total = items.reduce((s, i) => s + i.count, 0);
  const peak  = rows.reduce((m, i) => Math.max(m, i.count), 0);

  if (rows.length === 0) {
    return (
      <section className="chart">
        <h3>{title}</h3>
        <p className="muted" style={{ fontSize: 12 }}>{emptyHint ?? "No data yet."}</p>
      </section>
    );
  }

  const description = rows.map((r) => `${r.label}: ${r.count}`).join(", ");
  const svgH = rows.length * ROW_H + 4;

  return (
    <section className="chart">
      <h3>{title}</h3>
      <svg
        role="img"
        aria-label={`${title}. ${description}.`}
        viewBox={`0 0 ${WIDTH} ${svgH}`}
        width="100%"
        height={svgH}
        style={{ overflow: "visible" }}
      >
        <title>{title}</title>
        <desc>{description}</desc>
        {rows.map((row, index) => {
          const y      = index * ROW_H + 2;
          const barW   = peak > 0 ? Math.max(3, (row.count / peak) * BAR_AREA) : 3;
          const pct    = total > 0 ? Math.round((row.count / total) * 100) : 0;
          const labelX = LABEL_W - 6;

          return (
            <g key={row.key} style={onSelect ? { cursor: "pointer" } : undefined}
               onClick={onSelect ? () => onSelect(row.key) : undefined}
               role={onSelect ? "button" : undefined}
               aria-label={onSelect ? `Filter by ${row.label}` : undefined}
               tabIndex={onSelect ? 0 : undefined}
            >
              {/* label */}
              <text
                x={labelX}
                y={y + BAR_H - 2}
                textAnchor="end"
                className="chart-label"
                style={{ fontSize: 11 }}
              >
                {row.label.length > 14 ? `${row.label.slice(0, 13)}…` : row.label}
              </text>

              {/* background track */}
              <rect
                x={LABEL_W}
                y={y + 1}
                width={BAR_AREA}
                height={BAR_H - 2}
                rx={3}
                className="chart-bar-bg"
              />

              {/* filled bar */}
              <rect
                x={LABEL_W}
                y={y + 1}
                width={barW}
                height={BAR_H - 2}
                rx={3}
                className="chart-bar"
              />

              {/* count */}
              <text
                x={LABEL_W + BAR_AREA + 5}
                y={y + BAR_H - 2}
                className="chart-count"
                style={{ fontSize: 11 }}
              >
                {row.count}
              </text>

              {/* percent inside bar (only if bar is wide enough) */}
              {barW > 30 ? (
                <text
                  x={LABEL_W + barW - 5}
                  y={y + BAR_H - 3}
                  textAnchor="end"
                  className="chart-pct"
                  style={{ fontSize: 9.5 }}
                >
                  {pct}%
                </text>
              ) : null}
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
        <p style={{ fontSize: 11, color: "var(--muted-c)", marginTop: 6 }}>
          Top {rows.length} of {items.length} ({total} total)
        </p>
      ) : null}
    </section>
  );
}

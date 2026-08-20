type Props = {
  label: string;
  value: number;
  max: number;
  detail?: string | undefined;
  indeterminate?: boolean | undefined;
};

export function ProgressBar({ label, value, max, detail, indeterminate }: Props) {
  const safeMax = max > 0 ? max : 0;
  const clamped = safeMax > 0 ? Math.min(Math.max(value, 0), safeMax) : 0;
  const ratio = safeMax > 0 ? clamped / safeMax : 0;

  return (
    <div className="progress">
      <div className="progress-head">
        <span className="progress-label">{label}</span>
        <span className="progress-count">
          {safeMax > 0 ? `${clamped} / ${safeMax}` : "…"}
        </span>
      </div>
      <div
        className={indeterminate ? "progress-track indeterminate" : "progress-track"}
        role="progressbar"
        aria-label={label}
        {...(indeterminate || safeMax === 0
          ? {}
          : {
              "aria-valuenow": clamped,
              "aria-valuemin": 0,
              "aria-valuemax": safeMax,
              "aria-valuetext": `${clamped} of ${safeMax}`,
            })}
      >
        <div className="progress-fill" style={{ width: `${(ratio * 100).toFixed(1)}%` }} />
      </div>
      {detail ? (
        <p className="progress-detail" aria-live="polite">
          {detail}
        </p>
      ) : null}
    </div>
  );
}

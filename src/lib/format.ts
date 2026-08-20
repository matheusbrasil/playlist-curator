export function percent(ratio: number, digits = 0): string {
  if (!Number.isFinite(ratio)) return "—";
  return `${(ratio * 100).toFixed(digits)}%`;
}

export function score(value: number): string {
  return Number.isFinite(value) ? value.toFixed(2) : "—";
}

export function dateTime(iso: string | null): string {
  if (!iso) return "—";
  const parsed = new Date(iso);
  if (Number.isNaN(parsed.getTime())) return iso;
  return parsed.toLocaleString();
}

export function sourceLabel(source: string): string {
  return source.replace(/_/g, " ");
}

export type ConfidenceLevel = "high" | "medium" | "low" | "unknown";

export function confidenceLevel(value: number | null): ConfidenceLevel {
  if (value === null || !Number.isFinite(value)) return "unknown";
  if (value >= 0.75) return "high";
  if (value >= 0.4) return "medium";
  return "low";
}

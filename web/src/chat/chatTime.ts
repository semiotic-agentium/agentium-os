/** Provenance timestamps may be ns or ms; normalize to ms for `Date`. */
export function normalizeEpochMs(raw: number | string | undefined): number {
  const numeric = typeof raw === "string" ? Number(raw) : raw;
  if (!Number.isFinite(numeric) || !numeric || numeric <= 0) return 0;
  if (numeric > 10_000_000_000_000) return Math.floor(numeric / 1_000_000);
  return numeric;
}

export function normalizePreview(text: string | undefined): string {
  if (!text) return "Untitled conversation";
  const singleLine = text.replace(/\s+/g, " ").trim();
  if (singleLine.length === 0) return "Untitled conversation";
  return singleLine.length > 80 ? `${singleLine.slice(0, 77)}...` : singleLine;
}

/** Format raw UTF-8 byte counts as KiB/MiB for prompt JSON sizing. */
export function formatKb(bytes: number | undefined | null): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes <= 0) return "—";
  const kb = bytes / 1024;
  if (kb >= 1024) return `${(kb / 1024).toFixed(2)} MiB`;
  return `${kb.toFixed(1)} KiB`;
}

/** Format a token/number count compactly: 1234 → "1.2k", 1_500_000 → "1.5M". */
export function formatCompact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/** Format a duration in milliseconds: 1500 → "1.50s", 42 → "42ms". */
export function formatDuration(ms: number): string {
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`;
  return `${ms}ms`;
}

/** Truncate an ID string for display: "abcdef01-2345-6789..." → "abcdef01...6789". */
export function shortId(id: string | undefined | null): string {
  if (id == null || id === "") return "—";
  if (id.length <= 12) return id;
  return `${id.slice(0, 8)}...${id.slice(-4)}`;
}

/** Normalize a potentially empty/null group value to undefined. */
export function normalizeGroupValue(raw: string | null | undefined): string | undefined {
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

/** Build a display-friendly agent identity string from optional parts. */
export function asDisplayIdentity(
  agentId?: string,
  agentPackage?: string,
  agentVersion?: string,
): string {
  const packageName = agentPackage && agentPackage !== "unknown" ? agentPackage : "";
  const version = agentVersion && agentVersion !== "unknown" ? agentVersion : "";
  if (packageName && version) return `${packageName}/${version}`;
  if (packageName) return packageName;
  if (agentId && agentId !== "unknown") return shortId(agentId);
  return "unknown-agent";
}

/** Extract a dimension value from groupValues array with groupKey pipe-fallback. */
export function groupValueAt(
  values: Array<string | null> | undefined,
  groupKey: string,
  index: number,
): string | undefined {
  const fromValues = normalizeGroupValue(values?.[index]);
  if (fromValues) return fromValues;
  return normalizeGroupValue(groupKey.split("|")[index]);
}

/**
 * Session-unique temporal id used by the web app for client-minted entity ids
 * (chat message ids, event-console context/message ids, etc).
 *
 * Format: `${prefix}-${now}-${counter}` — same shape the Rust runner parses for
 * `ContextId::parse_temporal` (`ctx-<ms>-<n>`). The `now` parameter is exposed
 * for tests; production callers omit it and use `Date.now()`.
 */
let counter = 0;

export function mintTemporalId(prefix: string, now: number = Date.now()): string {
  counter += 1;
  return `${prefix}-${now}-${counter}`;
}

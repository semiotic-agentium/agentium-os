/**
 * Encode a provenance context id for a single URL path segment.
 *
 * Delegated A2A contexts look like
 * `a2a:ctx-…:grafana-investigator/default:a2a-child-…` — colons and slashes
 * must be percent-encoded or `/contexts/{id}/…` routes split on the inner `/`.
 */
export function encodeContextIdForPath(contextId: string): string {
  return encodeURIComponent(contextId);
}

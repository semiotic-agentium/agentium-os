/** True when text looks like a Mermaid sequence diagram from the runner export API. */
export function looksLikeMermaidDiagram(text: string): boolean {
  return /^\s*sequenceDiagram\b/m.test(text.trim());
}

/**
 * Fetch canonical context mermaid from the runner. Returns empty string on 404, invalid body, or error.
 */
export async function fetchContextMermaidDiagram(contextId: string): Promise<string> {
  try {
    const res = await fetch(`/contexts/${contextId}/mermaid`);
    if (!res.ok) {
      return "";
    }
    const text = await res.text();
    return looksLikeMermaidDiagram(text) ? text : "";
  } catch {
    return "";
  }
}

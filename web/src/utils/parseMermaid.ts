const MERMAID_BLOCK_RE = /```mermaid\r?\n([\s\S]*?)```/g;

/**
 * Extracts mermaid diagram source strings from a text containing fenced code blocks.
 * Returns an array of trimmed diagram sources (empty array if none found).
 */
export function parseMermaidBlocks(text: string): string[] {
  const blocks: string[] = [];
  MERMAID_BLOCK_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = MERMAID_BLOCK_RE.exec(text)) !== null) {
    const code = match[1]?.trim();
    if (code) blocks.push(code);
  }
  return blocks;
}

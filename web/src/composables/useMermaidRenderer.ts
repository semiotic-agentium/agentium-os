import { ref, watch, type Ref } from "vue";
import mermaid from "mermaid";

export interface RenderedDiagram {
  svg: string;
  error: string | null;
}

let seq = 0;

/**
 * Renders an array of mermaid diagram sources into SVG strings.
 * Re-renders whenever sources or theme changes.
 */
export function useMermaidRenderer(sources: Ref<string[]>, theme: Ref<string>) {
  const rendered = ref<RenderedDiagram[]>([]);

  async function renderAll() {
    mermaid.initialize({
      startOnLoad: false,
      theme: theme.value === "dark" ? "dark" : "default",
      securityLevel: "loose",
    });
    rendered.value = await Promise.all(sources.value.map(renderOne));
  }

  async function renderOne(src: string): Promise<RenderedDiagram> {
    try {
      const id = `mermaid-${++seq}`;
      const { svg } = await mermaid.render(id, src);
      return { svg, error: null };
    } catch (e) {
      return { svg: "", error: e instanceof Error ? e.message : String(e) };
    }
  }

  watch([sources, theme], renderAll, { immediate: true, deep: true });

  return { rendered };
}

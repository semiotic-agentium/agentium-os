// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { onUnmounted, ref, watch, type Ref } from "vue";
import mermaid from "mermaid";
import { looksLikeMermaidDiagram } from "../utils/mermaidDiagram";

export interface RenderedDiagram {
  svg: string;
  error: string | null;
}

let seq = 0;

const RENDER_DEBOUNCE_MS = 80;

/**
 * Renders an array of mermaid diagram sources into SVG strings.
 * Debounces source updates to avoid re-rendering on every streaming chunk.
 */
export function useMermaidRenderer(sources: Ref<string[]>, theme: Ref<string>) {
  const rendered = ref<RenderedDiagram[]>([]);
  let initTheme: string | null = null;
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;

  async function renderAll() {
    const t = theme.value === "dark" ? "dark" : "default";
    if (initTheme !== t) {
      initTheme = t;
      mermaid.initialize({
        startOnLoad: false,
        theme: t,
        securityLevel: "loose",
      });
    }
    rendered.value = await Promise.all(sources.value.map(renderOne));
  }

  async function renderOne(src: string): Promise<RenderedDiagram> {
    if (!looksLikeMermaidDiagram(src)) {
      return { svg: "", error: null };
    }
    try {
      const id = `mermaid-${++seq}`;
      const { svg } = await mermaid.render(id, src);
      return { svg, error: null };
    } catch (e) {
      return { svg: "", error: e instanceof Error ? e.message : String(e) };
    }
  }

  function scheduleRender() {
    if (debounceTimer !== null) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      void renderAll();
    }, RENDER_DEBOUNCE_MS);
  }

  let sourcesFirst = true;
  watch(
    () => JSON.stringify(sources.value),
    () => {
      if (sourcesFirst) {
        sourcesFirst = false;
        void renderAll();
        return;
      }
      scheduleRender();
    },
    { immediate: true },
  );

  watch(theme, () => {
    void renderAll();
  });

  onUnmounted(() => {
    if (debounceTimer !== null) clearTimeout(debounceTimer);
  });

  return { rendered };
}

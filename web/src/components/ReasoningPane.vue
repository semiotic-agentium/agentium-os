<script setup lang="ts">
import { ref, computed } from "vue";
import { useTheme } from "../composables/useTheme";
import { useMermaidRenderer } from "../composables/useMermaidRenderer";

const props = defineProps<{ diagrams: string[] }>();

const isOpen = ref(typeof window !== "undefined" ? window.innerWidth >= 1500 : true);
const { theme } = useTheme();

const sources = computed(() => props.diagrams);
const { rendered } = useMermaidRenderer(sources, theme);

// Modal state
const expandedIdx = ref<number | null>(null);

function openModal(i: number) {
  expandedIdx.value = i;
}

function closeModal() {
  expandedIdx.value = null;
}

function onOverlayClick(e: MouseEvent) {
  if ((e.target as HTMLElement).classList.contains("diagram-modal-overlay")) {
    closeModal();
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") closeModal();
}

function downloadSvg(svg: string, index: number) {
  const blob = new Blob([svg], { type: "image/svg+xml" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `reasoning-diagram-${index + 1}.svg`;
  a.click();
  URL.revokeObjectURL(url);
}
</script>

<template>
  <aside class="reasoning-pane" :class="{ open: isOpen }">
    <!-- Collapsible content -->
    <div class="reasoning-pane-inner">
      <header class="reasoning-header">
        <!-- Eye / reasoning icon -->
        <svg
          class="reasoning-icon"
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
          <circle cx="12" cy="12" r="3" />
        </svg>
        <span>Reasoning</span>
      </header>

      <div class="reasoning-body">
        <!-- Empty state -->
        <div v-if="rendered.length === 0" class="reasoning-empty">
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <rect x="3" y="3" width="5" height="4" rx="1" />
            <rect x="16" y="3" width="5" height="4" rx="1" />
            <rect x="9" y="17" width="6" height="4" rx="1" />
            <path d="M5.5 7v3a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V7" />
            <path d="M12 11v6" />
          </svg>
          <p>The conversation sequence diagram will appear here after the first reply.</p>
        </div>

        <!-- Diagrams list -->
        <div v-else class="reasoning-diagrams">
          <div
            v-for="(item, i) in rendered"
            :key="i"
            class="diagram-card"
            :class="{ clickable: !item.error }"
            @click="!item.error && openModal(i)"
            :title="item.error ? undefined : 'Click to expand'"
          >
            <div v-if="item.error" class="diagram-error">
              <span class="diagram-error-label">Render error</span>
              <pre>{{ item.error }}</pre>
            </div>
            <template v-else>
              <div class="diagram-svg" v-html="item.svg" />
              <!-- Expand hint -->
              <div class="diagram-expand-hint">
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <polyline points="15 3 21 3 21 9" /><polyline points="9 21 3 21 3 15" />
                  <line x1="21" y1="3" x2="14" y2="10" /><line x1="3" y1="21" x2="10" y2="14" />
                </svg>
              </div>
            </template>
          </div>
        </div>
      </div>
    </div>

    <!-- Toggle strip (always visible) -->
    <button
      class="reasoning-toggle"
      @click="isOpen = !isOpen"
      :title="isOpen ? 'Collapse reasoning pane' : 'Expand reasoning pane'"
      :aria-label="isOpen ? 'Collapse reasoning pane' : 'Expand reasoning pane'"
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2.5"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <polyline :points="isOpen ? '15 18 9 12 15 6' : '9 18 15 12 9 6'" />
      </svg>
    </button>
  </aside>

  <!-- Diagram modal — rendered outside the aside via Teleport -->
  <Teleport to="body">
    <div
      v-if="expandedIdx !== null && rendered[expandedIdx] && !rendered[expandedIdx]!.error"
      class="diagram-modal-overlay"
      @click="onOverlayClick"
      @keydown="onKeydown"
      tabindex="-1"
    >
      <div class="diagram-modal" role="dialog" aria-modal="true" aria-label="Diagram fullscreen view">
        <header class="diagram-modal-header">
          <span class="diagram-modal-title">Reasoning Diagram</span>
          <div class="diagram-modal-actions">
            <button
              class="diagram-modal-btn"
              title="Download SVG"
              @click="downloadSvg(rendered[expandedIdx!]!.svg, expandedIdx!)"
            >
              <!-- Download icon -->
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" y1="15" x2="12" y2="3" />
              </svg>
              Download
            </button>
            <button
              class="diagram-modal-btn diagram-modal-close"
              title="Close"
              @click="closeModal"
            >
              <!-- X icon -->
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
        </header>
        <div
          class="diagram-modal-body"
          v-html="rendered[expandedIdx!]!.svg"
        />
      </div>
    </div>
  </Teleport>
</template>

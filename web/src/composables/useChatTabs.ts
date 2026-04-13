import { ref, computed, type Ref, shallowRef, triggerRef } from "vue";
import { useA2aClient } from "./useA2aClient";

export interface ChatTab {
  id: string;
  title: string;
  /** The full useA2aClient() return value backing this tab. */
  client: ReturnType<typeof useA2aClient>;
}

let tabCounter = 0;

export function useChatTabs() {
  const tabs: Ref<ChatTab[]> = shallowRef([]);
  const activeTabId = ref<string | null>(null);

  function createTab(title?: string): ChatTab {
    const id = `tab-${Date.now()}-${++tabCounter}`;
    const client = useA2aClient();
    const tab: ChatTab = { id, title: title ?? "New Chat", client };
    tabs.value = [...tabs.value, tab];
    activeTabId.value = id;
    // Fetch agents for the new tab's client
    client.fetchAgents();
    return tab;
  }

  function closeTab(id: string) {
    const idx = tabs.value.findIndex((t) => t.id === id);
    if (idx === -1) return;
    // Cancel any active stream before closing
    tabs.value[idx]!.client.cancelStream();
    const next = tabs.value.filter((t) => t.id !== id);
    tabs.value = next;
    if (activeTabId.value === id) {
      // Switch to adjacent tab or null
      activeTabId.value = next[Math.min(idx, next.length - 1)]?.id ?? null;
    }
    // If no tabs left, create a fresh one
    if (next.length === 0) {
      createTab();
    }
  }

  function switchTab(id: string) {
    if (tabs.value.some((t) => t.id === id)) {
      activeTabId.value = id;
    }
  }

  function renameTab(id: string, title: string) {
    const tab = tabs.value.find((t) => t.id === id);
    if (tab) {
      tab.title = title;
      triggerRef(tabs);
    }
  }

  const activeTab = computed(() =>
    tabs.value.find((t) => t.id === activeTabId.value) ?? null,
  );

  const activeClient = computed(() => activeTab.value?.client ?? null);

  // Start with one tab
  createTab();

  return {
    tabs,
    activeTabId,
    activeTab,
    activeClient,
    createTab,
    closeTab,
    switchTab,
    renameTab,
  };
}

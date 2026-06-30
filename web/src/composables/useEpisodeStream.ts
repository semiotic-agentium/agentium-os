// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { onUnmounted, ref, type Ref } from "vue";
import { instanceFetch } from "./instanceApi";
import type { EpisodeSnapshot } from "../types/provenance";

export interface EpisodeStreamState {
  episode: EpisodeSnapshot | null;
  isStreaming: boolean;
  error: string | null;
}

export interface EpisodeStreamController {
  readonly state: Ref<EpisodeStreamState>;
  connect: (taskId: string) => Promise<void>;
  disconnect: () => void;
  readonly currentTaskId: Ref<string | null>;
}

export function useEpisodeStream(): EpisodeStreamController {
  const state = ref<EpisodeStreamState>({
    episode: null,
    isStreaming: false,
    error: null,
  });
  const currentTaskId = ref<string | null>(null);

  let abortController: AbortController | null = null;

  async function connect(taskId: string): Promise<void> {
    if (!taskId?.trim()) {
      state.value.error = "Task ID is required";
      return;
    }

    // No-op if already streaming the same task without an error — avoids expensive
    // SurrealDB re-reads that contend with agent provenance writes.
    if (taskId === currentTaskId.value && state.value.isStreaming && !state.value.error) {
      return;
    }

    // Disconnect existing stream if any
    disconnect();

    state.value.error = null;
    state.value.isStreaming = true;
    currentTaskId.value = taskId;

    const controller = new AbortController();
    abortController = controller;

    try {
      const url = `/tasks/${taskId}/episode/stream`;
      const response = await instanceFetch(url, {
        method: "GET",
        headers: { Accept: "text/event-stream" },
        signal: controller.signal,
      });

      if (!response.ok) {
        if (response.status === 404) {
          state.value.error = "Episode not found";
        } else if (response.status === 501) {
          state.value.error = "Episode service unavailable";
        } else {
          state.value.error = `HTTP ${response.status}`;
        }
        state.value.isStreaming = false;
        return;
      }

      if (!response.body) {
        state.value.error = "No response body";
        state.value.isStreaming = false;
        return;
      }

      await readSSEStream(response.body, taskId);
    } catch (err) {
      // AbortError is expected when disconnect() is called
      if (err instanceof DOMException && err.name === "AbortError") {
        return;
      }
      state.value.error = `Stream error: ${err}`;
      state.value.isStreaming = false;
    }
  }

  async function readSSEStream(body: ReadableStream<Uint8Array>, _taskId: string): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() || ""; // Keep incomplete line in buffer

        for (const raw of lines) {
          const line = raw.replace(/\r$/, "");
          if (line.startsWith("data: ")) {
            const data = line.slice(6);
            try {
              const snapshot: EpisodeSnapshot = JSON.parse(data);
              state.value.episode = snapshot;
            } catch (err) {
              console.warn("Failed to parse episode snapshot:", err, data);
            }
          } else if (line.startsWith("event: ")) {
            const event = line.slice(7).trim();
            if (event === "done") {
              state.value.isStreaming = false;
              return;
            }
          }
          // Ignore comments (: lines) and empty lines
        }
      }
    } finally {
      reader.releaseLock();
      state.value.isStreaming = false;
    }
  }

  function disconnect(): void {
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    state.value.isStreaming = false;
    currentTaskId.value = null;
  }

  // Auto-disconnect on component unmount
  onUnmounted(() => {
    disconnect();
  });

  return {
    state,
    connect,
    disconnect,
    currentTaskId,
  };
}

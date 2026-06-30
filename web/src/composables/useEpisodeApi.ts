// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { instanceFetch } from "./instanceApi";

export type EpisodeTextFetchResult =
  | { ok: true; blob: Blob }
  | { ok: false; status?: number; aborted?: boolean };

function linkAbortSignals(target: AbortController, source?: AbortSignal): void {
  if (!source) return;
  if (source.aborted) {
    target.abort();
    return;
  }
  source.addEventListener("abort", () => target.abort(), { once: true });
}

/** Download persisted episode transcript for a task (operator/public read route). */
export async function fetchEpisodeTextBlob(
  taskId: string,
  options?: { signal?: AbortSignal; timeoutMs?: number },
): Promise<EpisodeTextFetchResult> {
  const controller = new AbortController();
  linkAbortSignals(controller, options?.signal);
  const timeoutMs = options?.timeoutMs ?? 15_000;
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const res = await instanceFetch(`/tasks/${encodeURIComponent(taskId)}/episode/text`, {
      signal: controller.signal,
    });
    if (!res.ok) {
      return { ok: false, status: res.status };
    }
    return { ok: true, blob: await res.blob() };
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") {
      return { ok: false, aborted: true };
    }
    return { ok: false };
  } finally {
    clearTimeout(timer);
  }
}

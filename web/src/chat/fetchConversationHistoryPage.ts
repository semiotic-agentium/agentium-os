// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type { ConversationHistoryPage } from "../types/a2a";
import {
  observationScopeQueryParams,
  type ObservationScope,
} from "../composables/useObservationScope";

const DEFAULT_PAGE_SIZE = 50;

/** Paginated GET merge — same shape as backend `merge_conversation_history_pages`. */
export async function fetchMergedConversationHistoryPage(
  scope: ObservationScope,
  options?: { signal?: AbortSignal; pageSize?: number },
): Promise<ConversationHistoryPage | null> {
  const pageSize = options?.pageSize ?? DEFAULT_PAGE_SIZE;
  const allItems: ConversationHistoryPage["items"] = [];
  let cursor: string | undefined;
  let lastPage: ConversationHistoryPage | null = null;
  let maxEventOrder = 0;

  for (;;) {
    const params = observationScopeQueryParams(scope);
    params.set("limit", String(pageSize));
    params.set("profile", "full");
    if (cursor) params.set("cursor", cursor);

    const res = await fetch(
      `/contexts/${scope.contextId}/conversation-history?${params.toString()}`,
      { signal: options?.signal },
    );
    if (!res.ok) return null;

    const page = (await res.json()) as ConversationHistoryPage;
    if (!Array.isArray(page.items)) return null;

    allItems.push(...page.items);
    maxEventOrder = Math.max(maxEventOrder, page.maxEventOrder);
    lastPage = page;
    if (!page.nextCursor) break;
    cursor = page.nextCursor;
  }

  if (!lastPage) return null;

  return {
    ...lastPage,
    items: allItems,
    maxEventOrder,
    nextCursor: null,
  };
}

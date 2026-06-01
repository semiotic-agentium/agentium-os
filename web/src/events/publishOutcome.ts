// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Interpret `POST /events/publish` subscriber ack details for operator UX. */

import type { EventPublishResponse } from "../types/events";

export function isNoopSubscriberDetail(detail: string): boolean {
  const d = detail.toLowerCase();
  return (
    d.includes("no lifecycle records") ||
    d.includes("no lifecycle units") ||
    d.includes("no readable slack") ||
    d.includes("skipped:not_relevant") ||
    d.includes("skipped:empty_unit")
  );
}

export function publishHadNoEffectiveWork(outcome: EventPublishResponse | null): boolean {
  if (!outcome) return false;
  if (outcome.subscribers_matched === 0) return true;
  const acceptances = outcome.acceptances ?? [];
  if (acceptances.length === 0) return false;
  return acceptances.every((a) => isNoopSubscriberDetail(a.detail));
}

export function formatPublishAcceptanceSummary(outcome: EventPublishResponse): string {
  const acceptances = outcome.acceptances ?? [];
  if (acceptances.length === 0) {
    if (outcome.subscribers_matched === 0) {
      return "No agents subscribe to this event (check source_kind and deploy subscribers).";
    }
    return `${outcome.subscribers_accepted} of ${outcome.subscribers_matched} subscriber(s) accepted`;
  }
  return acceptances
    .map((a) => `${a.agent_package}/${a.agent_instance_id}: ${a.detail}`)
    .join("\n");
}

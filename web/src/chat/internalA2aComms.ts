// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import {
  isInterAgentA2aHeaderSummary,
  summarizeSendDoneReplayPayload,
  summarizeSessionStepHeader,
} from "./interAgentA2a";
import { pushA2aCommsEvent } from "./toolNotificationEvents";
import type { ToolNotificationBlock } from "../types/a2a";

/** Outbound A2A comms for `system/internal_a2a` send_done only (inter-agent, not user replies). */
export function appendInterAgentA2aSendDoneArtifacts(
  block: ToolNotificationBlock,
  toolName: string,
  stepKind: string,
  op: { kind: string },
  sendDoneReplayPayload?: unknown,
): void {
  if (stepKind !== "send_done" || toolName !== "system/internal_a2a") return;
  const header = summarizeSessionStepHeader(op);
  if (header && isInterAgentA2aHeaderSummary(header)) {
    pushA2aCommsEvent(block, header);
  }
  const replay = summarizeSendDoneReplayPayload(sendDoneReplayPayload);
  if (replay) {
    pushA2aCommsEvent(block, replay);
  }
}

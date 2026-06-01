// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  hasOpenToolSession,
  parseUnitProgress,
  worstUnitProgress,
} from "./runProgressParse";
import type { ChatMessage } from "../types/a2a";

describe("parseUnitProgress", () => {
  it("parses ClickUp ack detail", () => {
    expect(
      parseUnitProgress(
        "Processed ClickUp lifecycle ingress: 0/1 unit(s) from 1 record(s)",
      ),
    ).toEqual({ done: 0, total: 1 });
  });

  it("returns null when no unit pattern", () => {
    expect(parseUnitProgress("No lifecycle records")).toBeNull();
  });
});

describe("worstUnitProgress", () => {
  it("picks the most incomplete subscriber", () => {
    const result = worstUnitProgress([
      {
        agent_package: "a",
        agent_instance_id: "default",
        detail: "Processed ClickUp lifecycle ingress: 1/1 unit(s)",
      },
      {
        agent_package: "b",
        agent_instance_id: "default",
        detail: "Processed ClickUp lifecycle ingress: 0/2 unit(s)",
      },
    ]);
    expect(result).toEqual({
      agentLabel: "b/default",
      done: 0,
      total: 2,
    });
  });
});

describe("hasOpenToolSession", () => {
  it("detects running tool with open FSM step", () => {
    const messages: ChatMessage[] = [
      {
        id: "msg-open",
        role: "agent",
        text: "",
        timestamp: new Date(0),
        contentBlocks: [
          {
            type: "tool",
            toolName: "support/clickup",
            status: "Running",
            events: [
              {
                kind: "system_notice",
                subtype: "Session step",
                text: "open",
              },
            ],
          },
        ],
      },
    ];
    expect(hasOpenToolSession(messages)).toBe(true);
  });

  it("returns false when tool session finished", () => {
    const messages: ChatMessage[] = [
      {
        id: "msg-done",
        role: "agent",
        text: "",
        timestamp: new Date(0),
        contentBlocks: [
          {
            type: "tool",
            toolName: "support/clickup",
            status: "Done",
            completion: "DONE",
            events: [
              {
                kind: "system_notice",
                subtype: "Session step",
                text: "finish",
              },
            ],
          },
        ],
      },
    ];
    expect(hasOpenToolSession(messages)).toBe(false);
  });
});

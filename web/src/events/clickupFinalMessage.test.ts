// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  collectStepEvidence,
  extractFinalMessage,
} from "../../../agents/clickup-agent/src/clickupWorkflow.ts";

// Shapes copied from a real ClickUp step-executor run so the guards stay anchored to reality:
// `run.outcome === "completed"` only means the loop terminated cleanly.
const sendTeams = {
  status: "done",
  output: '@1 · support/clickup:Send(list_teams) · 5L · 154B',
  receipt: {
    archive_ref: "@1",
    tool_name: "support/clickup",
    action_identity: "Send(list_teams)",
    line_count: 5,
  },
  result: {
    items: [{ id: "9013491519", name: "Workspace", kind: "team" }],
    message: "Found 1 team(s)",
  },
};

const sendSpaces = {
  status: "done",
  output:
    '@2 · support/clickup:Send(list_spaces team_id="9013491519") · 7L · 165B',
  receipt: {
    archive_ref: "@2",
    tool_name: "support/clickup",
    action_identity: 'Send(list_spaces team_id="9013491519")',
    line_count: 7,
  },
  result: {
    items: [{ id: "90132086465", name: "Team Space", kind: "space" }],
    message: "Found 1 space(s)",
  },
};

const sendTasks = {
  status: "done",
  output:
    '@4 · support/clickup:Send(list_tasks list_id="901325431486") · 210L · 6.3KB',
  receipt: {
    archive_ref: "@4",
    tool_name: "support/clickup",
    action_identity: 'Send(list_tasks list_id="901325431486")',
    line_count: 210,
  },
  result: {
    tasks: [
      { id: "86aff3vkz", name: "Task21", status: "in progress" },
      { id: "86ag53uj5", name: "Task100", status: "to do" },
    ],
  },
};

const partialNavigationFinish = {
  op: "ReadOnlyFinish",
  reply: {
    parts: [
      {
        type: "text",
        text:
          "Workspace hierarchy: 1 team, 1 space. Next would list lists then filter tasks.",
      },
    ],
    citations: [],
  },
};

const groundedFinish = {
  op: "ReadOnlyFinish",
  reply: {
    parts: [{ type: "text", text: "Found 1 in-progress task: Task21." }],
    citations: ["@4"],
  },
};

describe("ClickUp executor evidence", () => {
  it("counts task evidence from an executor envelope", () => {
    const evidence = collectStepEvidence([sendTeams, sendSpaces, sendTasks]);
    expect(evidence.listedTasks).toBe(true);
    expect(evidence.navigated).toBe(true);
    expect(evidence.taskCount).toBe(2);
  });

  it("rejects a navigation-only answer for a task-list plan", () => {
    // The observed failure: list teams → list spaces → stop with prose.
    const outcome = extractFinalMessage(
      [sendTeams, sendSpaces, partialNavigationFinish],
      "task_list",
    );
    expect(outcome.kind).toBe("incomplete");
  });

  it("accepts a navigation-only answer for a navigation plan", () => {
    const outcome = extractFinalMessage(
      [sendTeams, sendSpaces, partialNavigationFinish],
      "navigation",
    );
    expect(outcome.kind).toBe("message");
  });

  it("rejects navigation prose even when tasks were retrieved", () => {
    const outcome = extractFinalMessage(
      [sendTeams, sendSpaces, sendTasks, partialNavigationFinish],
      "task_list",
    );
    expect(outcome.kind).toBe("incomplete");
  });

  it("accepts a grounded answer backed by a task list", () => {
    const outcome = extractFinalMessage(
      [sendTeams, sendSpaces, sendTasks, groundedFinish],
      "task_list",
    );
    expect(outcome.kind).toBe("message");
  });

  it("rejects a write plan without a write result", () => {
    const outcome = extractFinalMessage(
      [sendTasks, groundedFinish],
      "write_confirmation",
    );
    expect(outcome.kind).toBe("incomplete");
  });

  it("rejects a loop that produced no terminal answer", () => {
    const outcome = extractFinalMessage([sendTasks], "task_list");
    expect(outcome.kind).toBe("incomplete");
  });
});

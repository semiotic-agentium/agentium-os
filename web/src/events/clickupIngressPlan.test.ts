// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  PKG_CLICKUP_EXECUTE,
  PKG_CLICKUP_FORMAT,
  validateClickUpPlanForExecution,
} from "../../../agents/clickup-agent/src/clickupExecution.ts";

type PlanInput = Parameters<typeof validateClickUpPlanForExecution>[0];

function plan(steps: PlanInput["plan_steps"]): PlanInput {
  return {
    intent_description: "Test",
    objective: "Test",
    plan_steps: steps,
    citations: null,
  };
}

describe("validateClickUpPlanForExecution", () => {
  it("accepts execute then format", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          agent_instance_id: "default",
          sub_message: "List tasks",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Summarize",
        },
      ]),
    );
    expect(Array.isArray(result)).toBe(true);
  });

  it("rejects zero format steps", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          agent_instance_id: "default",
          sub_message: "Only execute",
        },
      ]),
    );
    expect(result).toBe("plan must include exactly one clickup-format step");
  });

  it("rejects two format steps", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          agent_instance_id: "default",
          sub_message: "Execute",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Format 1",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Format 2",
        },
      ]),
    );
    expect(result).toBe("plan must include exactly one clickup-format step");
  });

  it("rejects discovery agent_package names", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: "clickup-agent",
          agent_instance_id: "default",
          sub_message: "Wrong package",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Format",
        },
      ]),
    );
    expect(typeof result).toBe("string");
    expect(result).toContain("invalid agent_package");
  });
});

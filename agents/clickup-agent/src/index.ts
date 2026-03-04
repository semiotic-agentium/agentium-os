/// <reference path="./baml-runtime.d.ts" />

import type { ChatMessage } from "./a2a";

declare function ChooseClickUpAction(
  args?: Record<string, unknown>
): Promise<unknown>;

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>
): Promise<ToolSessionHandle>;

const MAX_REACT_STEPS = 8;
const MAX_FINGERPRINT_CHARS = 6000;
const MAX_CONSECUTIVE_REPEATS = 2;
const CLICKUP_TOOL_NAME = "support/clickup";

type FinalResponse = { message: string };
type ClickUpTask = {
  id?: string;
  name?: string;
  status?: string;
  url?: string;
};
type ClickUpItem = {
  id: string;
  name: string;
  kind: string;
};
type ClickUpOutput = {
  tasks?: ClickUpTask[];
  items?: ClickUpItem[];
  message?: string;
};
type SupportClickupSessionStep = {
  op?: string;
  input?: Record<string, unknown>;
  reason?: string;
};
type SupportClickupSessionPlan = {
  steps: SupportClickupSessionStep[];
};

function extractText(message: ChatMessage | null | undefined): string {
  if (!message?.parts?.length) return "unknown";
  const first = message.parts[0];
  if (first && typeof (first as { text?: string }).text === "string") {
    return (first as { text: string }).text;
  }
  return "unknown";
}

function isObject(v: unknown): v is Record<string, unknown> {
  return v != null && typeof v === "object";
}

function isFinalResponse(v: unknown): v is FinalResponse {
  if (!isObject(v)) return false;
  if (typeof v.message !== "string") return false;
  return !("tasks" in v || "items" in v || "steps" in v || "action" in v);
}

function isToolOutput(v: unknown): v is ClickUpOutput {
  if (!isObject(v)) return false;
  return Array.isArray(v.tasks) || Array.isArray(v.items);
}

function isSessionPlan(v: unknown): v is SupportClickupSessionPlan {
  if (!isObject(v) || !Array.isArray(v.steps) || v.steps.length === 0) return false;
  return v.steps.every(
    (step) =>
      isObject(step) &&
      typeof step.op === "string" &&
      (step.op !== "Send" || isObject(step.input))
  );
}

function isExplicitlyEmptySessionPlan(v: unknown): boolean {
  return isObject(v) && Array.isArray(v.steps) && v.steps.length === 0;
}

function extractToolOutput(v: unknown): ClickUpOutput | null {
  if (isToolOutput(v)) return v;
  if (isObject(v) && isToolOutput(v.output)) return v.output;
  return null;
}

async function executeClickUpPlan(
  plan: SupportClickupSessionPlan
): Promise<ClickUpOutput | null> {
  let session: ToolSessionHandle | null = null;
  let lastStepOutput: unknown = null;

  for (const step of plan.steps) {
    switch (step.op) {
      case "Open":
        if (!session) session = await openToolSession(CLICKUP_TOOL_NAME);
        break;
      case "Send":
        if (!session) session = await openToolSession(CLICKUP_TOOL_NAME);
        await session.send(step.input || {});
        break;
      case "Next":
        if (!session) session = await openToolSession(CLICKUP_TOOL_NAME);
        lastStepOutput = await session.continue();
        break;
      case "Finish":
        if (session) {
          await session.finish();
          session = null;
        }
        break;
      case "Abort":
        if (session) {
          await session.abort(step.reason);
          session = null;
        }
        return null;
      default:
        return null;
    }
  }

  if (session) {
    lastStepOutput = await session.continue();
    await session.finish();
  }

  return extractToolOutput(lastStepOutput);
}

function truncate(text: string, max: number): string {
  return text.length <= max ? text : text.slice(0, max);
}

function fingerprint(output: ClickUpOutput): string {
  return truncate(
    JSON.stringify({
      message: output.message || "",
      items: (output.items || []).slice(0, 10),
      tasks: (output.tasks || []).slice(0, 20),
    }),
    MAX_FINGERPRINT_CHARS
  );
}

function formatOutput(output: ClickUpOutput): string {
  let response = output.message || "Done.";
  if (output.items && output.items.length > 0) {
    const itemList = output.items
      .map((i) => `• [${i.kind}] ${i.name} (id: ${i.id})`)
      .join("\n");
    response += "\n\n" + itemList;
  }
  if (output.tasks && output.tasks.length > 0) {
    const taskList = output.tasks
      .map((t) => `• ${t.name || "Unnamed task"} [${t.status || "unknown"}]${t.url ? ` — ${t.url}` : ""}`)
      .join("\n");
    response += "\n\n" + taskList;
  }
  return response;
}

async function onChatMessage(
  message: ChatMessage & { __baml_invocation_token?: string }
): Promise<void> {
  const s = session(message);
  await s.run(async () => {
    const text = extractText(message);
    const token = message.__baml_invocation_token;
    let prevFingerprint: string | null = null;
    let consecutiveRepeats = 0;
    let lastToolOutput: ClickUpOutput | null = null;

    try {
      for (let step = 1; step <= MAX_REACT_STEPS; step++) {
        let result: unknown;
        try {
          result = await ChooseClickUpAction({
            user_message: text,
            __baml_invocation_token: token,
          });
        } catch (planErr) {
          // If the planner call fails (e.g. LLM returns empty/invalid
          // response) but we already accumulated output from a prior
          // step, return that output rather than losing the data.
          if (lastToolOutput) return { message: formatOutput(lastToolOutput) };
          throw planErr;
        }

        if (isExplicitlyEmptySessionPlan(result)) {
          if (lastToolOutput) return { message: formatOutput(lastToolOutput) };
          return { message: "ClickUp planner returned an empty session plan." };
        }

        if (isSessionPlan(result)) {
          const executedOutput = await executeClickUpPlan(result);
          if (executedOutput) {
            result = executedOutput;
          } else {
            return {
              message:
                "ClickUp planner returned a raw session plan but no tool output was produced.",
            };
          }
        }

        if (isFinalResponse(result)) {
          return { message: result.message };
        }

        if (isToolOutput(result)) {
          lastToolOutput = result;
          const fp = fingerprint(result);
          if (fp === prevFingerprint) {
            consecutiveRepeats += 1;
          } else {
            consecutiveRepeats = 0;
            prevFingerprint = fp;
          }

          if (consecutiveRepeats >= MAX_CONSECUTIVE_REPEATS) {
            return { message: formatOutput(result) };
          }
          continue;
        }

        if (isObject(result) && typeof result.message === "string") {
          return { message: result.message };
        }

        return { message: "ClickUp planner returned an unexpected response shape." };
      }

      if (lastToolOutput) {
        const msg = `${formatOutput(lastToolOutput)}\n\nStopped after ${MAX_REACT_STEPS} planning steps.`;
        return { message: msg };
      }

      return {
        message: `Unable to complete the request within ${MAX_REACT_STEPS} planning steps.`,
      };
    } catch (e) {
      // Preserve accumulated output even when a later step fails.
      if (lastToolOutput) {
        return { message: formatOutput(lastToolOutput) };
      }
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: `Error: ${errMsg}` };
    }
  });
}

__chat_register({ onChatMessage });

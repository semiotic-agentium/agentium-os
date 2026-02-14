/// <reference path="./baml-runtime.d.ts" />

import type { ChatMessage, ChatStreamChunk, Task } from "./a2a";

declare function ChooseClickUpAction(
  args?: Record<string, unknown>
): Promise<unknown>;

const MAX_REACT_STEPS = 8;
const MAX_FINGERPRINT_CHARS = 6000;
const MAX_CONSECUTIVE_REPEATS = 2;

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

function extractText(message: ChatMessage | null | undefined): string {
  if (!message?.parts?.length) return "unknown";
  const first = message.parts[0];
  if (first && typeof (first as { text?: string }).text === "string") {
    return (first as { text: string }).text;
  }
  return "unknown";
}

function newMessage(text: string): { parts: { text: string }[] } {
  return { parts: [{ text }] };
}

function newTask(message?: { parts: { text: string }[] }): Task {
  return {
    status: { state: "TASK_STATE_WORKING", message },
  };
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
  const text = extractText(message);
  const token = message.__baml_invocation_token;
  let prevFingerprint: string | null = null;
  let consecutiveRepeats = 0;
  let lastToolOutput: ClickUpOutput | null = null;

  try {
    for (let step = 1; step <= MAX_REACT_STEPS; step++) {
      const result: unknown = await ChooseClickUpAction({
        user_message: text,
        __baml_invocation_token: token,
      });

      if (isFinalResponse(result)) {
        const msg = result.message;
        __baml_chat_yield({ message: newMessage(msg), task: newTask(newMessage(msg)) });
        return;
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
          const msg = formatOutput(result);
          __baml_chat_yield({ message: newMessage(msg), task: newTask(newMessage(msg)) });
          return;
        }
        continue;
      }

      if (isObject(result) && typeof result.message === "string") {
        const msg = result.message;
        __baml_chat_yield({ message: newMessage(msg), task: newTask(newMessage(msg)) });
        return;
      }

      __baml_chat_yield({
        message: newMessage("ClickUp planner returned an unexpected response shape."),
        task: newTask(),
      });
      return;
    }

    if (lastToolOutput) {
      const msg = `${formatOutput(lastToolOutput)}\n\nStopped after ${MAX_REACT_STEPS} planning steps.`;
      __baml_chat_yield({ message: newMessage(msg), task: newTask(newMessage(msg)) });
      return;
    }

    __baml_chat_yield({
      message: newMessage(
        `Unable to complete the request within ${MAX_REACT_STEPS} planning steps.`
      ),
      task: newTask(),
    });
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    __baml_chat_yield({ message: newMessage(`Error: ${errMsg}`), task: newTask() });
  }
}

__baml_chat_register({ onChatMessage });

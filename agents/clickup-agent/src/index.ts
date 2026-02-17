/// <reference path="./baml-runtime.d.ts" />

declare function ChooseClickUpAction(
  args?: Record<string, unknown>
): Promise<unknown>;

const MAX_REACT_STEPS = 10;
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
  message: ChatMessage
): Promise<void> {
  const s = session(message);
  await s.run(async () => {
    const text = extractText(message);
    let prevFingerprint: string | null = null;
    let consecutiveRepeats = 0;
    let lastToolOutput: ClickUpOutput | null = null;

    try {
      for (let step = 1; step <= MAX_REACT_STEPS; step++) {
        const result: unknown = await ChooseClickUpAction({
          user_message: text,
        });

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
        return { message: formatOutput(lastToolOutput) };
      }

      return {
        message: `Unable to complete the request within ${MAX_REACT_STEPS} planning steps.`,
      };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      if (lastToolOutput) {
        return { message: formatOutput(lastToolOutput) };
      }
      return { error: `Error: ${errMsg}` };
    }
  });
}

__chat_register({ onChatMessage });

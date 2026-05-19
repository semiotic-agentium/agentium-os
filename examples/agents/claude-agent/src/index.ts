/// <reference path="./baml-runtime.d.ts" />
import type { RunContext, SessionResult, StructuredReply } from "./baml-runtime";

// Stop after Open → Send. The Send step blocks until Claude Code emits a
// terminal_result, and this agent returns that result directly. Do not enter the
// Continue phase: generated Continue still permits Send in its schema for some
// tools, and prompt-only "Finish" guidance has proven insufficient to prevent
// duplicate Claude invocations.
const MAX_FSM_STEPS = 2;

type ClaudeEvent = {
  kind?: string;
  text?: string;
  result?: string;
  is_error?: boolean;
};

function isObject(v: unknown): v is Record<string, unknown> {
  return v != null && typeof v === "object";
}

/** Walk an arbitrarily-nested executor hop result and extract Claude's terminal_result text. */
function extractTerminalText(node: unknown): string | null {
  if (!isObject(node)) return null;
  const events = (node as { events?: unknown }).events;
  if (Array.isArray(events)) {
    for (const ev of events as ClaudeEvent[]) {
      if (ev?.kind === "terminal_result" && typeof ev.result === "string" && ev.result.length > 0) {
        return ev.result;
      }
    }
  }
  for (const key of ["result", "output", "last"]) {
    const child = (node as Record<string, unknown>)[key];
    const found = extractTerminalText(child);
    if (found) return found;
  }
  return null;
}

/** Fallback: collect all assistant_text fragments anywhere under node, in encounter order. */
function collectAssistantText(node: unknown, out: string[]): void {
  if (!isObject(node)) return;
  const events = (node as { events?: unknown }).events;
  if (Array.isArray(events)) {
    for (const ev of events as ClaudeEvent[]) {
      if (ev?.kind === "assistant_text" && typeof ev.text === "string" && ev.text.length > 0) {
        out.push(ev.text);
      }
    }
  }
  for (const key of ["result", "output", "last"]) {
    collectAssistantText((node as Record<string, unknown>)[key], out);
  }
}

function extractClaudeReply(run: unknown): StructuredReply {
  if (isObject(run) && (run as { outcome?: string }).outcome === "agent_correctable") {
    const r = (run as { recovery: { code: string; mistake: string } }).recovery;
    return {
      parts: [{ type: "text", text: `[${r.code}] ${r.mistake}` }],
      citations: [],
    };
  }
  if (isObject(run) && (run as { outcome?: string }).outcome === "fatal") {
    const m = (run as { message: string }).message;
    return { parts: [{ type: "text", text: m }], citations: [] };
  }
  const telemetry = isObject(run) && (run as { outcome?: string }).outcome === "completed" ? run : { last: run, steps: undefined };
  const candidates: unknown[] = [];
  if (isObject(telemetry)) {
    candidates.push((telemetry as { last?: unknown }).last);
    const steps = (telemetry as { steps?: unknown[] }).steps;
    if (Array.isArray(steps)) candidates.push(...steps.slice().reverse());
  } else {
    candidates.push(telemetry);
  }
  for (const c of candidates) {
    const text = extractTerminalText(c);
    if (text) {
      return {
        parts: [{ type: "text", text }],
        citations: [],
      };
    }
  }
  // No terminal_result — likely interrupted/aborted. Fall back to streamed assistant_text.
  const assistantParts: string[] = [];
  for (const c of candidates) {
    collectAssistantText(c, assistantParts);
  }
  if (assistantParts.length > 0) {
    return {
      parts: [{ type: "text", text: assistantParts.join("\n\n") }],
      citations: [],
    };
  }
  return {
    parts: [{ type: "text", text: "Claude Code returned no terminal result." }],
    citations: [],
  };
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const userText = (ctx.text ?? "").trim() || "unknown";
    ctx.emit.message("Working with Claude Code...");

    const run = await runGeneratedStepExecutor(
      "ChooseDevClaudeExtAction",
      { user_message: userText },
      { max_steps: MAX_FSM_STEPS },
    );

    const message = extractClaudeReply(run);
    return { message };
  },
});

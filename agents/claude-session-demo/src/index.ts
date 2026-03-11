/// <reference path="./baml-runtime.d.ts" />

declare function RequirementsPhase(args: { user_message: string }): Promise<unknown>;
declare function ProduceSpec(args: { requirements_summary: string }): Promise<unknown>;
declare function ChooseClaudeDevAction(args: {
  spec_text: string;
  validation_criteria_json: string;
  last_tool_output: string;
  user_approval_intent: string;
}): Promise<unknown>;
declare function SummarizeDevWorkInPersonality(args: {
  session_report: string;
}): Promise<string>;

// --- BAML return types and guards ---
type NeedMoreInput = { question: string };
type RequirementsReady = { summary: string; requirements: string[] };
type Spec = { specification_text: string; validation_criteria: string[] };
type ClaudeDevAskUser = { action: string; prompt: string };
type ClaudeDevReport = { action: string; message: string };

function isObject(v: unknown): v is Record<string, unknown> {
  return v != null && typeof v === "object";
}

function isNeedMoreInput(v: unknown): v is NeedMoreInput {
  return isObject(v) && typeof (v as NeedMoreInput).question === "string";
}

function isRequirementsReady(v: unknown): v is RequirementsReady {
  return (
    isObject(v) &&
    typeof (v as RequirementsReady).summary === "string" &&
    Array.isArray((v as RequirementsReady).requirements)
  );
}

function isSpec(v: unknown): v is Spec {
  return (
    isObject(v) &&
    typeof (v as Spec).specification_text === "string" &&
    Array.isArray((v as Spec).validation_criteria)
  );
}

function isClaudeDevAskUser(v: unknown): v is ClaudeDevAskUser {
  return isObject(v) && (v as ClaudeDevAskUser).action === "AskUser" && typeof (v as ClaudeDevAskUser).prompt === "string";
}

function isClaudeDevReport(v: unknown): v is ClaudeDevReport {
  return isObject(v) && (v as ClaudeDevReport).action === "Report" && typeof (v as ClaudeDevReport).message === "string";
}

// --- Tool output (runtime executes session plan and returns this) ---
type ClaudeCompletion = "DONE" | "INPUT_REQUIRED" | "INTERRUPTED";
type ClaudeEvent = {
  kind?: string;
  text?: string;
  thinking?: string;
  subtype?: string;
  result?: string;
  is_error?: boolean;
  [key: string]: unknown;
};
type ClaudeNextOutput = { events?: ClaudeEvent[]; completion?: ClaudeCompletion };

const MAX_REQUIREMENTS_TURNS = 5;
const MAX_DEV_ACTIONS = 40;

function asNextOutput(value: unknown): ClaudeNextOutput {
  if (isObject(value) && Array.isArray((value as ClaudeNextOutput).events)) {
    return value as ClaudeNextOutput;
  }
  if (isObject(value) && isObject((value as { output?: unknown }).output)) {
    const out = (value as { output: ClaudeNextOutput }).output;
    if (Array.isArray(out.events)) return out;
  }
  return {};
}

function renderEvents(events: ClaudeEvent[]): string {
  const lines: string[] = [];
  for (const event of events) {
    const kind = event.kind || "unknown";
    if (kind === "assistant_text" && typeof event.text === "string") {
      lines.push(event.text);
      continue;
    }
    if (kind === "assistant_thinking" && typeof event.thinking === "string") {
      lines.push(`[thinking] ${event.thinking}`);
      continue;
    }
    if (kind === "terminal_result") {
      if (typeof event.result === "string" && event.result.length > 0) {
        lines.push(event.result);
      } else {
        lines.push(`[terminal:${String(event.subtype || "unknown")}]`);
      }
      continue;
    }
    if (kind === "system_notice" && typeof event.subtype === "string") {
      lines.push(`[system:${event.subtype}]`);
      continue;
    }
  }
  return lines.join("\n").trim();
}

/** Normalize runtime result to an array of streaming chunks (each { events?, completion? }). */
function asChunkArray(raw: unknown): ClaudeNextOutput[] {
  if (Array.isArray(raw) && raw.length > 0) {
    return raw.map((v) => asNextOutput(v));
  }
  if (raw != null) {
    const one = asNextOutput(raw);
    if (one.events != null || one.completion != null) return [one];
  }
  return [];
}

/** Claude-specific: derive tool-approval intent from message parts (parts may carry toolApproval in extra). */
function userApprovalIntentFromParts(parts: unknown): string {
  if (!Array.isArray(parts)) return "";
  for (const part of parts) {
    if (!isObject(part)) continue;
    const approval = (part as { toolApproval?: { approved?: boolean } }).toolApproval;
    if (approval && typeof approval.approved === "boolean") {
      return approval.approved ? "approved" : "rejected";
    }
  }
  return "";
}

/** Build last_tool_output string for BAML from full chunk history (no discard). */
function formatLastToolOutputFromChunks(chunks: ClaudeNextOutput[]): string {
  const parts = chunks.map((next) => {
    const rendered = renderEvents(next.events || []);
    const completion = next.completion ?? "";
    return `[output]\n${rendered}\n[completion] ${completion}`.trim();
  });
  return parts.join("\n\n").trim() || "";
}

__chat_register({
  run: async (ctx) => {
    try {
      let currentUserMessage = ctx.text?.trim() ?? "";

      // ---------- Phase 1: Requirements ----------
      let requirementsSummary: string | undefined;
      let requirementsList: string[] = [];
      for (let r = 0; r < MAX_REQUIREMENTS_TURNS; r++) {
        const result = await RequirementsPhase({ user_message: currentUserMessage });
        if (isRequirementsReady(result)) {
          requirementsSummary = result.summary;
          requirementsList = result.requirements ?? [];
          break;
        }
        if (isNeedMoreInput(result)) {
          ctx.emit.message(result.question);
          const reply = await ctx.emit.awaitInput("");
          currentUserMessage = messageText(reply) || "";
          if (!currentUserMessage) currentUserMessage = "Continue.";
          continue;
        }
        return { error: "Requirements phase returned an unexpected response." };
      }
      if (requirementsSummary === undefined) {
        return {
          error: "Requirements gathering exceeded the maximum number of rounds.",
        };
      }

      // Report requirements back to the user before writing the spec.
      const requirementsMessage = [
        "--- Requirements ---",
        requirementsSummary,
        ...(requirementsList.length > 0
          ? ["", "Discrete requirements:", ...requirementsList.map((req) => `- ${req}`)]
          : []),
        "",
        "Writing the specification...",
      ].join("\n");
      ctx.emit.message(requirementsMessage);

      // ---------- Phase 2: Spec ----------
      const specResult = await ProduceSpec({ requirements_summary: requirementsSummary });
      if (!isSpec(specResult)) {
        return { error: "Spec phase returned an invalid specification." };
      }
      const spec = specResult;

      // Send the plan to the user in one message (so the client cannot show only the follow-up line).
      const planMessage = [
        "--- Plan ---",
        "Requirements:",
        ...(requirementsList.length > 0 ? requirementsList.map((req) => `- ${req}`) : ["- (see summary)"]),
        "",
        "Specification:",
        spec.specification_text,
        "",
        "Validation criteria:",
        ...spec.validation_criteria.map((c) => `- ${c}`),
        "---",
        "",
        "Starting development via claude/dev (BAML-controlled).",
      ].join("\n");
      ctx.emit.message(planMessage);
      // Emit a clear "still working" so UIs don't treat the plan as the final chunk
      ctx.emit.statusChanged("TASK_STATE_WORKING");
      // Yield so the stream collector can drain and forward the plan chunks to the client before the long ChooseClaudeDevAction/tool session runs.
      await Promise.resolve()

      // ---------- Tool session: pure BAML-driven iteration. ChooseClaudeDevAction decides each step (Report | AskUser | session plan). TS only applies the result and updates context. ----------
      const validationCriteriaJson = JSON.stringify(spec.validation_criteria);
      let lastToolOutput = "";
      const messageParts = (ctx.message as { parts?: unknown })?.parts;
      let userApprovalIntent = userApprovalIntentFromParts(messageParts);

      for (let step = 0; step < MAX_DEV_ACTIONS; step++) {
        const result = await ChooseClaudeDevAction({
          spec_text: spec.specification_text,
          validation_criteria_json: validationCriteriaJson,
          last_tool_output: lastToolOutput,
          user_approval_intent: userApprovalIntent,
        });

        if (isClaudeDevReport(result)) {
          const summary = await SummarizeDevWorkInPersonality({
            session_report: result.message,
          });
          return { message: summary };
        }

        if (isClaudeDevAskUser(result)) {
          // AskUser when last_tool_output is empty is invalid (BAML rule: never AskUser on first call). Retry once.
          if (lastToolOutput === "") {
            const retryResult = await ChooseClaudeDevAction({
              spec_text: spec.specification_text,
              validation_criteria_json: validationCriteriaJson,
              last_tool_output: lastToolOutput,
              user_approval_intent: userApprovalIntent,
            });
            if (isClaudeDevReport(retryResult)) {
              const summary = await SummarizeDevWorkInPersonality({
                session_report: retryResult.message,
              });
              return { message: summary };
            }
            if (!isClaudeDevAskUser(retryResult)) {
              lastToolOutput = formatLastToolOutputFromChunks(asChunkArray(retryResult));
              continue;
            }
          }
          const neutralPrompt = "Your next message will be sent to Claude.";
          ctx.emit.message(neutralPrompt);
          const reply = await ctx.emit.awaitInput(neutralPrompt);
          userApprovalIntent = userApprovalIntentFromParts((reply as { parts?: unknown })?.parts);
          continue;
        }

        // Session plan was executed by runtime; tool output is in result. Client already got toolStreamChunk events.
        lastToolOutput = formatLastToolOutputFromChunks(asChunkArray(result));
      }

      return {
        error: "Tool session exceeded maximum actions.",
      };
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      return {
        error: `An error occurred: ${msg}`,
      };
    }
  },
});

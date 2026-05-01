/// <reference path="./baml-runtime.d.ts" />

type StructuredReply = import("./baml-runtime").StructuredReply;
import type { RunContext, SessionResult } from "./baml-runtime";

// --- BAML return types and guards ---
// BAML-declared functions (RequirementsPhase, ProduceSpec, ChooseDevClaudeExtAction,
// SummarizeDevWorkInPersonality) and their return types come from baml-runtime.d.ts.
type NeedMoreInput = { question: string };
type RequirementsReady = { summary: string; requirements: string[] };
type Spec = { specification_text: string; validation_criteria: string[] };
type DevClaudeExtAskUser = { action: string; prompt: string };
type DevClaudeExtReport = { action: string; message: string };

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

function isDevClaudeExtAskUser(v: unknown): v is DevClaudeExtAskUser {
  return isObject(v) && (v as DevClaudeExtAskUser).action === "AskUser" && typeof (v as DevClaudeExtAskUser).prompt === "string";
}

function isDevClaudeExtReport(v: unknown): v is DevClaudeExtReport {
  return isObject(v) && (v as DevClaudeExtReport).action === "Report" && typeof (v as DevClaudeExtReport).message === "string";
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

function executionMessageId(message: unknown): string {
  if (isObject(message)) {
    if (typeof message.messageId === "string" && message.messageId.trim().length > 0) return message.messageId;
    if (typeof message.id === "string" && message.id.trim().length > 0) return message.id;
  }
  return "msg-claude-session-fallback";
}

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

function formatLastToolOutputFromExecutorRun(rawRun: unknown): string {
  const candidates: unknown[] = [];
  if (isObject(rawRun)) {
    const run = rawRun as { last?: unknown; steps?: unknown[] };
    candidates.push(run.last);
    if (Array.isArray(run.steps)) {
      candidates.push(...run.steps.slice().reverse());
    }
  } else {
    candidates.push(rawRun);
  }

  for (const candidate of candidates) {
    if (candidate == null) continue;
    const rendered = formatLastToolOutputFromChunks(asChunkArray(candidate));
    if (rendered.length > 0) return rendered;
  }
  return "";
}

__chat_register({
  run: async (ctx) => {
    let executionExecutable: {
      startStep?: (stepId: string) => Promise<unknown>;
      completeStep?: (stepId: string) => Promise<unknown>;
      finish: () => Promise<unknown>;
      abort?: (reason: string) => Promise<unknown>;
    } | null = null;
    try {
      let currentUserMessage = ctx.text?.trim() ?? "";
      const originalUserMessage = currentUserMessage;
      let authoritativeUserRequest = originalUserMessage;
      const executionSession = typeof openA2aExecutionSession === "function"
        ? await openA2aExecutionSession("claude-session-" + Date.now().toString())
        : null;
      const messageId = executionMessageId(ctx.message);

      // ---------- Phase 1: Requirements ----------
      let requirementsSummary: string | undefined;
      for (let r = 0; r < MAX_REQUIREMENTS_TURNS; r++) {
        const result = await RequirementsPhase({ user_message: currentUserMessage });
        if (isRequirementsReady(result)) {
          requirementsSummary = result.summary;
          break;
        }
        if (isNeedMoreInput(result)) {
          ctx.emit.message(result.question);
          const reply = await ctx.emit.awaitInput("");
          currentUserMessage = messageText(reply) || "";
          if (!currentUserMessage) currentUserMessage = "Continue.";
          authoritativeUserRequest = [
            originalUserMessage,
            "",
            "Additional user clarification:",
            currentUserMessage,
          ].join("\n");
          continue;
        }
        return { error: "Requirements phase returned an unexpected response." };
      }
      if (requirementsSummary === undefined) {
        return {
          error: "Requirements gathering exceeded the maximum number of rounds.",
        };
      }

      const intentDescription =
        "Translate user requirements into implementation spec and execute iterative development workflow.";
      const intentPhase = executionSession
        ? await executionSession.submitIntent({
            intentId: "intent-claude-dev-workflow",
            description: intentDescription,
          })
        : null;

      // Keep derived requirements internal provenance. The original user request remains
      // the authoritative instruction passed to Claude Code.

      // ---------- Phase 2: Spec ----------
      const specResult = await ProduceSpec({ requirements_summary: requirementsSummary });
      if (!isSpec(specResult)) {
        return { error: "Spec phase returned an invalid specification." };
      }
      const spec = specResult;

      if (intentPhase != null) {
        executionExecutable = await intentPhase.submitPlan({
          intentId: "intent-claude-dev-workflow",
          planId: "plan-claude-dev-workflow",
          steps: [
            {
              stepId: "step-requirements",
              description: "Gather and validate software requirements.",
              order: 0,
              dependsOn: [],
            },
            {
              stepId: "step-specification",
              description: "Produce implementation specification and validation criteria.",
              order: 1,
              dependsOn: ["step-requirements"],
            },
            {
              stepId: "step-development",
              description: "Execute claude/dev workflow and deliver final report.",
              order: 2,
              dependsOn: ["step-specification"],
            },
          ],
        });
      }

      if (executionExecutable != null) {
        await executionExecutable.startStep?.("step-requirements");
        await executionExecutable.completeStep?.("step-requirements");
        await executionExecutable.startStep?.("step-specification");
        await executionExecutable.completeStep?.("step-specification");
      }

      // Do not expose the intermediate Requirements/Plan rewrite as the user-visible answer.
      // It is advisory context for provenance only; Claude Code should work from the raw request.
      ctx.emit.message("Working with Claude Code...");
      ctx.emit.statusChanged("TASK_STATE_WORKING");
      await Promise.resolve();

      // ---------- Tool session: host-driven step executor loop (same pattern as coordinator agents). ----------
      const claudeHandoff = [
        authoritativeUserRequest,
        "",
        "---",
        "Non-authoritative planner context follows. Use it only if helpful.",
        "The original user request above is the source of truth.",
        "If the planner context conflicts with the original request, follow the original request.",
        "",
        "Planner specification:",
        spec.specification_text,
        "",
        "Planner validation criteria:",
        ...spec.validation_criteria.map((c) => `- ${c}`),
        "",
        "Important instructions:",
        "- Do not rename functions, files, APIs, variables, or entities requested by the user.",
        "- Do not add constraints, edge cases, file creation, tests, or execution unless the user asked",
        "  for them or they are necessary for the task.",
      ].join("\n");
      const validationCriteriaJson = JSON.stringify(spec.validation_criteria);
      let lastToolOutput = "";
      const messageParts = (ctx.message as { parts?: unknown })?.parts;
      let userApprovalIntent = userApprovalIntentFromParts(messageParts);
      if (executionExecutable != null) {
        await executionExecutable.startStep?.("step-development");
      }

      for (let operatorRound = 0; operatorRound < MAX_DEV_ACTIONS; operatorRound++) {
        const run = await runGeneratedStepExecutor(
          "ChooseDevClaudeExtAction",
          {
            spec_text: claudeHandoff,
            validation_criteria_json: validationCriteriaJson,
            last_tool_output: lastToolOutput,
            user_approval_intent: userApprovalIntent,
          },
          { max_steps: MAX_DEV_ACTIONS },
        );

        const result = isObject(run) ? (run as { last?: unknown }).last : run;
        if (isDevClaudeExtReport(result)) {
          if (executionExecutable != null) {
            await executionExecutable.completeStep?.("step-development");
            await executionExecutable.finish();
          }
          const summary = await SummarizeDevWorkInPersonality({
            session_report: result.message,
          });
          return { message: summary };
        }

        lastToolOutput = formatLastToolOutputFromExecutorRun(run);

        if (isDevClaudeExtAskUser(result)) {
          const neutralPrompt = "Your next message will be sent to Claude.";
          ctx.emit.message(neutralPrompt);
          const reply = await ctx.emit.awaitInput(neutralPrompt);
          userApprovalIntent = userApprovalIntentFromParts((reply as { parts?: unknown })?.parts);
          if (!userApprovalIntent) {
            // Preserve free-text operator replies for the next hop.
            lastToolOutput = [
              lastToolOutput,
              "[operator_reply]",
              messageText(reply) || "",
            ]
              .filter((part) => part.length > 0)
              .join("\n")
              .trim();
          }
          continue;
        }
      }

      return {
        error: "Tool session exceeded maximum actions.",
      };
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (executionExecutable != null) {
        try {
          await executionExecutable.abort?.("Claude session workflow aborted: " + msg);
        } catch (_) {
          // Best-effort abort only.
        }
      }
      return {
        error: `An error occurred: ${msg}`,
      };
    }
  },
});

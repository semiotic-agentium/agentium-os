/// <reference path="./baml-runtime.d.ts" />
import type {
  ReplyPart,
  RunContext,
  SessionResult,
  StructuredReply,
} from "./baml-runtime";

const MAX_REACT_STEPS = 8;
const MAX_CLARIFY = 2;

function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

type NeedClarification = { question: string };
type NotRelevant = { reason: string };
type NotionIntent = { intent: string };
type NotionPlanStep = { id: string; description: string; kind: "discover" | "read" | "synthesize" };
type NotionPlan = { goal: string; steps: NotionPlanStep[] };
type NotionPageSummary = { id: string; title: string; url: string };
type NotionBlockSummary = { block_type?: string; text?: string | null };
type NotionSource = { page_id: string; url: string };
type NotionSummary = {
  commitments?: string[];
  conflicts?: string[];
  missing?: string[];
  sources?: string[];
};

type NotionOutput = {
  message?: string;
  pages?: NotionPageSummary[];
  blocks?: NotionBlockSummary[];
  sources?: NotionSource[];
};

type ReadOnlyResponse = {
  message: string;
  next_step?: string;
};

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function executionMessageId(message: unknown): string {
  if (isObject(message)) {
    if (typeof message.messageId === "string" && message.messageId.trim().length > 0) return message.messageId;
    if (typeof message.id === "string" && message.id.trim().length > 0) return message.id;
  }
  return "msg-notion-fallback";
}

function isNeedClarification(v: unknown): v is NeedClarification {
  return isObject(v) && typeof v.question === "string" && v.question.trim().length > 0
    && !("goal" in v) && !("steps" in v) && !("message" in v) && !("intent" in v) && !("reason" in v);
}

function isNotRelevant(v: unknown): v is NotRelevant {
  return isObject(v) && typeof v.reason === "string" && !("question" in v) && !("goal" in v) && !("intent" in v);
}

function isNotionIntent(v: unknown): v is NotionIntent {
  return isObject(v) && typeof v.intent === "string" && v.intent.trim().length > 0
    && !("question" in v) && !("reason" in v) && !("steps" in v);
}

function isReadOnlyResponse(action: unknown): action is ReadOnlyResponse {
  if (!action || typeof action !== "object") return false;
  const c = action as Record<string, unknown>;
  if (typeof c.message !== "string") return false;
  return !("pages" in c || "blocks" in c || "sources" in c || "steps" in c || "goal" in c);
}

function isNotionOutput(value: unknown): value is NotionOutput {
  if (!isObject(value)) return false;
  return (
    Array.isArray((value as NotionOutput).pages) ||
    Array.isArray((value as NotionOutput).blocks) ||
    Array.isArray((value as NotionOutput).sources)
  );
}

function extractNotionOutput(value: unknown): NotionOutput | null {
  if (isNotionOutput(value)) return value;
  if (isObject(value) && isNotionOutput(value.output)) return value.output;
  return null;
}

function normalizeUserMessage(text: string): string {
  const trimmed = text.trim();
  const notionDirective = trimmed.match(/^use\s+notion\s*[:,-]?\s*/i);
  if (!notionDirective) return trimmed;
  const withoutDirective = trimmed.slice(notionDirective[0].length).trim();
  return withoutDirective.length > 0 ? withoutDirective : trimmed;
}

function slugGoal(goal: string): string {
  return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
}

/** Collect tool output snapshots from all executor steps for synthesis. */
function collectToolResultsJson(steps: unknown[]): string {
  const outputs: unknown[] = [];
  for (const step of steps) {
    const out = extractNotionOutput(step);
    if (out) outputs.push(out);
    if (isReadOnlyResponse(step)) outputs.push({ message: step.message });
  }
  try {
    return JSON.stringify(outputs.length > 0 ? outputs : steps.slice(-3), null, 2).slice(0, 6000);
  } catch (_) {
    return "{}";
  }
}

function formatPages(pages?: NotionPageSummary[]): string {
  if (!pages || pages.length === 0) return "";
  return "\n\nPages:\n" + pages.map((p) => `• ${p.title} — ${p.url}`).join("\n");
}

function formatSources(sources?: NotionSource[], pages?: NotionPageSummary[]): string {
  if (!sources || sources.length === 0) return "";
  const pageTitleById = new Map<string, string>();
  (pages || []).forEach((p) => pageTitleById.set(p.id, p.title));
  return "\n\nSources:\n" + sources.map((s) => {
    const title = pageTitleById.get(s.page_id);
    return title ? `• ${title} — ${s.url}` : `• ${s.url}`;
  }).join("\n");
}

function formatSummaryLines(label: string, items?: string[]): string {
  if (!items || items.length === 0) return `${label}:\n- None found`;
  return `${label}:\n${items.map((i) => `- ${i}`).join("\n")}`;
}

function renderSummary(summary: NotionSummary): string {
  return [
    formatSummaryLines("Commitments", summary.commitments),
    formatSummaryLines("Conflicts", summary.conflicts),
    formatSummaryLines("Missing", summary.missing),
    formatSummaryLines("Sources", summary.sources),
  ].join("\n");
}

/** Render tool output from the executor as a user-visible response (fallback when synthesis fails). */
async function renderToolOutput(steps: unknown[], goal: string): Promise<string> {
  // Collect the most informative output from the executor run.
  for (const step of [...steps].reverse()) {
    const out = extractNotionOutput(step);
    if (!out) continue;
    let response = out.message || "";
    response += formatPages(out.pages);

    // If we have block content, try the structured summarizer.
    const blocksText = (out.blocks || [])
      .map((b) => b.text).filter((t): t is string => Boolean(t && t.trim())).join("\n");
    if (blocksText.length > 0) {
      try {
        const result = await SummarizeNotionContent({
          user_message: goal,
          page_title: out.pages?.[0]?.title ?? null,
          page_url: out.pages?.[0]?.url ?? null,
          blocks_text: blocksText.slice(0, 8000),
        });
        if (result && typeof result === "object") {
          const s = result as NotionSummary;
          if (s.commitments || s.conflicts || s.missing || s.sources) {
            if (out.sources && !s.sources) s.sources = out.sources.map((src) => src.url);
            return renderSummary(s);
          }
        }
      } catch (_) {
        // fall through
      }
    }
    response += formatSources(out.sources, out.pages);
    if (response.trim()) return response.trim();
  }
  // Last resort: use any ReadOnlyResponse from the loop.
  for (const step of [...steps].reverse()) {
    if (isReadOnlyResponse(step)) return step.message;
  }
  return "Notion returned no usable content for this request.";
}

/** Phase 3+4: Execute the plan — open execution session, run per-step executors, synthesize. */
async function runNotionPlan(
  ctx: RunContext,
  userText: string,
  plan: NotionPlan,
): Promise<SessionResult> {
  const { goal, steps } = plan;

  // ── Open execution session with the goal-derived plan ──────────────────
  const executionSession = typeof openA2aExecutionSession === "function"
    ? await openA2aExecutionSession("notion-" + Date.now().toString())
    : null;
  const messageId = executionMessageId(ctx.message);
  const intentId = "intent-notion-" + slugGoal(goal);
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: goal,
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: "plan-notion-" + slugGoal(goal),
        steps: steps.map((s, i) => ({
          stepId: s.id,
          description: s.description,
          order: i,
          dependsOn: i > 0 ? [steps[i - 1]!.id] : [],
        })),
      })
    : null;

  // ── Execute each tool step independently (persona pattern) ──────────────
  // Each step drives its own support/notion session: Open → Send → Read @N grep=... → Finish.
  // Read results accumulate in intra-turn history for ReactToNotionResults.
  const toolSteps = steps.filter((s) => s.kind !== "synthesize");
  const synthesizeStep = steps.find((s) => s.kind === "synthesize");

  try {
    for (const toolStep of toolSteps) {
      if (executable) {
        await executable.startStep?.(toolStep.id);
      }

      await runGeneratedStepExecutor("ChooseNotionAction", {
        goal,
        step_description: toolStep.description,
      }, { max_steps: MAX_REACT_STEPS });

      if (executable) {
        await executable.completeStep?.(toolStep.id);
      }
    }

    // ── Synthesize from conversation history ────────────────────────────
    // History contains Read results from all ChooseNotionAction runs above.
    if (synthesizeStep && executable) {
      await executable.startStep?.(synthesizeStep.id);
    }

    let finalMessage: StructuredReply;
    try {
      finalMessage = await ReactToNotionResults({
        goal,
        user_message: userText,
      });
    } catch (_) {
      finalMessage = textReply("Notion returned no usable content for this request.");
    }

    if (synthesizeStep && executable) {
      await executable.completeStep?.(synthesizeStep.id);
    }
    if (executable) await executable.finish?.();

    return { message: finalMessage };
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    try { if (executable) await executable.abort?.(errMsg); } catch (_) { /* best-effort */ }
    return { error: `Notion agent error: ${errMsg}` };
  }
}

__chat_register({
  run: async (ctx) => {
    const originalText = ctx.text || "unknown";
    let text = normalizeUserMessage(originalText);

    // ── Phase 1: Intent inference ────────────────────────────────────────────
    // InferNotionIntent classifies whether the message is a valid Notion query,
    // distills it into a clean intent, or asks for clarification / says not relevant.
    // Conversation history is passed via ctx.tags in the BAML prompt so the model
    // always has full context — no need to thread originalText manually.
    let validatedIntent: string | null = null;
    for (let i = 0; i <= MAX_CLARIFY; i++) {
      const intentResult = await InferNotionIntent({ user_message: text });

      if (isNotionIntent(intentResult)) {
        validatedIntent = intentResult.intent;
        break;
      }
      if (isNotRelevant(intentResult)) {
        return {
          message: textReply(`This doesn't look like a Notion question — ${intentResult.reason}`),
        };
      }
      if (isNeedClarification(intentResult) && i < MAX_CLARIFY) {
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = normalizeUserMessage(clarifiedText);
      } else {
        // Clarification exhausted — fall back to the original message as the search topic.
        validatedIntent = originalText;
        break;
      }
    }
    if (!validatedIntent) return { error: "Could not determine a valid Notion intent." };

    // ── Phase 2: Planning ────────────────────────────────────────────────────
    // PlanNotionWork takes the validated intent and produces an explicit step plan.
    // No clarification needed here — intent is already resolved above.
    const resolvedPlan = await PlanNotionWork({ intent: validatedIntent });
    return runNotionPlan(ctx, text, resolvedPlan);
  },
});

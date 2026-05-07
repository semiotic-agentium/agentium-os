/// <reference path="./baml-runtime.d.ts" />

declare function DetermineExtrospectionIntent(args: {
  user_message: string;
}): Promise<unknown>;
declare function GetDiscoverAgentsPlan(args: {
  user_message: string;
}): Promise<unknown>;
declare function SelectAgentFocus(args: {
  user_message: string;
  agents: unknown[];
}): Promise<unknown>;
declare function BuildExtrospectionPlan(args: {
  intent: unknown;
  selected_agent: unknown | null;
}): Promise<unknown>;
declare function SummarizeExtrospectionReport(args: {
  user_message: string;
  intent: unknown;
  selected_agent: unknown | null;
  agents: unknown[];
  primary_payload_json: string;
  secondary_payload_json: string;
}): Promise<import("./baml-runtime").StructuredReply>;

type NeedClarification = { question: string };
// QueryIntent is declared in baml-runtime.d.ts; the local alias just re-exports it
// for type-narrowing guards below.
type QueryIntent = import("./baml-runtime").QueryIntent;

type DiscoverAgentsOutput = {
  agents?: unknown[];
  done?: boolean;
};

type ExtrospectionOutput = {
  payloadJson?: string;
  payload_json?: string;
  done?: boolean;
};
function executionMessageId(message: unknown): string {
  if (isObject(message)) {
    if (typeof message.messageId === "string" && message.messageId.trim().length > 0) return message.messageId;
    if (typeof message.id === "string" && message.id.trim().length > 0) return message.id;
  }
  return "msg-extrospection-fallback";
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function isNeedClarification(value: unknown): value is NeedClarification {
  return isObject(value) && typeof value.question === "string" && value.question.trim().length > 0;
}

function isQueryIntent(value: unknown): value is QueryIntent {
  return (
    isObject(value)
    && typeof value.resource === "string"
    && typeof value.outcome === "string"
    && typeof value.priority_sort === "string"
    && typeof value.page_size === "number"
    && typeof value.top_k === "number"
    && typeof value.reason === "string"
  );
}

function normalizeQueryIntent(value: QueryIntent): QueryIntent {
  const page = Number.isFinite(value.page_size) ? Math.round(value.page_size) : 10;
  const topK = Number.isFinite(value.top_k) ? Math.round(value.top_k) : 8;
  return {
    resource: value.resource,
    outcome: value.outcome,
    priority_sort: value.priority_sort,
    page_size: Math.max(5, Math.min(25, page)),
    top_k: Math.max(3, Math.min(12, topK)),
    reason: value.reason,
  };
}

function extractPayloadJson(raw: unknown): string {
  if (!isObject(raw)) return "{}";
  if (typeof raw.payloadJson === "string") return raw.payloadJson;
  if (typeof raw.payload_json === "string") return raw.payload_json;
  if ("output" in raw && isObject(raw.output)) {
    const nested = raw.output as ExtrospectionOutput;
    if (typeof nested.payloadJson === "string") return nested.payloadJson;
    if (typeof nested.payload_json === "string") return nested.payload_json;
  }
  return "{}";
}

function parseDiscoverAgentsOutput(raw: unknown): DiscoverAgentsOutput {
  if (isObject(raw) && ("agents" in raw || "done" in raw)) return raw as DiscoverAgentsOutput;
  if (isObject(raw) && isObject(raw.output)) {
    const nested = raw.output;
    if ("agents" in nested || "done" in nested) return nested as DiscoverAgentsOutput;
  }
  return { agents: [], done: true };
}

function toText(value: unknown): string {
  if (typeof value === "string") return value.trim();
  return "";
}

function normalizeDelegatedText(text: string): string {
  const trimmed = text.trim();
  if (!trimmed) return "";
  try {
    const parsed = JSON.parse(trimmed) as {
      objective?: unknown;
      plan_steps?: Array<{ agent_package?: unknown; sub_message?: unknown }>;
    };
    if (Array.isArray(parsed.plan_steps)) {
      for (const step of parsed.plan_steps) {
        if (!isObject(step)) continue;
        const agentPackage = typeof step.agent_package === "string" ? step.agent_package : "";
        if (agentPackage !== "extrospection-agent") continue;
        const subMessage = typeof step.sub_message === "string" ? step.sub_message.trim() : "";
        if (subMessage) return subMessage.slice(0, 500);
      }
    }
    if (typeof parsed.objective === "string" && parsed.objective.trim().length > 0) {
      return parsed.objective.trim().slice(0, 500);
    }
  } catch (_) {
    // Best-effort compaction only.
  }
  return trimmed.slice(0, 500);
}

function fallbackIntentFromOpenInput(userText: string): QueryIntent {
  const lower = userText.toLowerCase();
  const wantsFailures =
    lower.includes("fail")
    || lower.includes("error")
    || lower.includes("incident")
    || lower.includes("weak")
    || lower.includes("cull")
    || lower.includes("underperform");
  const wantsTokens = lower.includes("token") || lower.includes("cost") || lower.includes("budget");
  const wantsLatency =
    lower.includes("latency")
    || lower.includes("slow")
    || lower.includes("duration")
    || lower.includes("performance");
  const wantsTools = lower.includes("tool");
  const wantsMessages = lower.includes("message");

  const resource = wantsMessages ? "Messages" : wantsTools ? "Tool_calls" : "Auto";
  const prioritySort = wantsTokens ? "total_tokens" : wantsLatency || wantsFailures ? "duration_ms" : "timestamp_ms";
  const outcome = wantsFailures ? "Failed_only" : "Both";

  return {
    resource,
    outcome,
    priority_sort: prioritySort,
    page_size: 12,
    top_k: 10,
    reason: "Autonomous fallback for broad operator input.",
  };
}

__chat_register({
  run: async (ctx) => {
    // `BuildExtrospectionPlan`: unified step executor (`runGeneratedStepExecutor`, tool-session IR). Intent/summary: direct BAML invokes. Planner-only unions use `unified_step_executors.json` for structured hops.
    let userText = normalizeDelegatedText(toText(ctx.text));
    if (!userText) {
      return {
        message:
          "Invoke thy query, and this extrospection spirit shall inspect the wider machine-host.",
      };
    }

    // Clarification loop: ask the user when intent is too vague before opening any sessions.
    // Capped at 2 rounds so the agent never loops indefinitely.
    const MAX_CLARIFY = 2;
    let intent: QueryIntent | null = null;
    for (let i = 0; i < MAX_CLARIFY; i++) {
      const intentRaw = await DetermineExtrospectionIntent({ user_message: userText });
      if (isQueryIntent(intentRaw)) {
        intent = normalizeQueryIntent(intentRaw);
        break;
      }
      if (isNeedClarification(intentRaw)) {
        const reply = await ctx.emit.awaitInput(intentRaw.question);
        userText = normalizeDelegatedText(messageText(reply));
        // Re-classify with clarified text on next iteration.
      } else {
        // Unexpected shape — use heuristic fallback and proceed.
        intent = fallbackIntentFromOpenInput(userText);
        break;
      }
    }
    // If all clarification rounds were consumed without a clean intent, fall back.
    if (!intent) {
      intent = fallbackIntentFromOpenInput(userText);
    }

    const executionSession = typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession("extrospection-" + Date.now().toString())
      : null;
    const messageId = executionMessageId(ctx.message);
    let executable: {
      startStep?: (stepId: string) => Promise<unknown>;
      completeStep?: (stepId: string) => Promise<unknown>;
      finish?: () => Promise<unknown>;
      abort?: (reason: string) => Promise<unknown>;
    } | null = null;

    try {
      const intentId = "intent-extrospection-" + intent.resource.toLowerCase();
      const intentDescription =
        `Analyze ${intent.resource} with outcome=${intent.outcome} sorted by ${intent.priority_sort}; reason: ${intent.reason}`;
      const intentPhase = executionSession
        ? await executionSession.submitIntent({
            intentId,
            description: intentDescription,
          })
        : null;

      // Drive discover_agents via strict step-executor loop to keep Open/Send/Next/Finish sequencing canonical.
      // Deterministic guard: skip discover_agents execution loop here.
      // The parent persona agent already performs discovery/routing; repeating this inside delegated
      // extrospection introduces additional execution drift risk from generated step-schema envelopes.
      const agents: unknown[] = [];
      const selectedAgent = null;

      executable = intentPhase
        ? await intentPhase.submitPlan({
            intentId,
            planId: "plan-extrospection-main",
            steps: [
              {
                stepId: "step-extrospection-query",
                description: "Execute extrospection query and collect machine telemetry.",
                order: 0,
                dependsOn: [],
              },
              {
                stepId: "step-extrospection-summarize",
                description: "Summarize findings for operator response.",
                order: 1,
                dependsOn: ["step-extrospection-query"],
              },
            ],
          })
        : null;
      if (executable != null) {
        await executable.startStep?.("step-extrospection-query");
      }

      const extrospectionRun = await runGeneratedStepExecutor(
        "BuildExtrospectionPlan",
        {
          intent,
          selected_agent: selectedAgent ?? null,
        },
        { max_steps: 6 },
      );

      const payloadCandidates: unknown[] = [extrospectionRun.last, ...extrospectionRun.steps.slice().reverse()];
      const payloads: string[] = [];
      for (const candidate of payloadCandidates) {
        const payload = extractPayloadJson(candidate);
        if (payload && payload !== "{}") payloads.push(payload);
      }
      const primaryPayloadJson = payloads[0] ?? "{}";
      const secondaryPayloadJson = payloads[1] ?? primaryPayloadJson;
      if (executable != null) {
        await executable.completeStep?.("step-extrospection-query");
        await executable.startStep?.("step-extrospection-summarize");
      }

      const responseRaw = await SummarizeExtrospectionReport({
        user_message: userText,
        intent,
        selected_agent: selectedAgent ?? null,
        agents,
        primary_payload_json: primaryPayloadJson,
        secondary_payload_json: secondaryPayloadJson,
      });
      if (executable != null) {
        await executable.completeStep?.("step-extrospection-summarize");
        await executable.finish?.();
      }
      return { message: responseRaw };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      if (executable != null) {
        try {
          await executable.abort?.("Extrospection flow aborted due to error: " + errMsg);
        } catch (_) {
          // Best-effort abort only.
        }
      }
      return { error: errMsg };
    }

  },
});

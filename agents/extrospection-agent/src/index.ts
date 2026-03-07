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
declare function BuildExtrospectionDrilldownPlan(args: {
  intent: unknown;
  selected_agent: unknown | null;
  pass1_payload_json: string;
}): Promise<unknown>;
declare function SummarizeExtrospectionReport(args: {
  user_message: string;
  intent: unknown;
  selected_agent: unknown | null;
  agents: unknown[];
  pass1_payload_json: string;
  pass2_payload_json: string;
}): Promise<string>;

type NeedClarification = { question: string };
type QueryIntent = {
  resource: string;
  outcome: string;
  priority_sort: string;
  page_size: number;
  top_k: number;
  reason: string;
};

type DiscoverAgentsOutput = {
  agents?: unknown[];
  done?: boolean;
};

type ExtrospectionOutput = {
  payloadJson?: string;
  payload_json?: string;
  done?: boolean;
};

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
    let userText = toText(ctx.text);
    if (!userText) {
      return {
        message:
          "Invoke thy query, and this extrospection spirit shall inspect the wider machine-host.",
      };
    }

    const intentRaw = await DetermineExtrospectionIntent({ user_message: userText });
    const intent = isNeedClarification(intentRaw)
      ? fallbackIntentFromOpenInput(userText)
      : isQueryIntent(intentRaw)
        ? normalizeQueryIntent(intentRaw)
        : fallbackIntentFromOpenInput(userText);

    // BAML orchestrates both tool session plans; TS only executes A2A FSM turns.
    const discoveredRaw = await GetDiscoverAgentsPlan({ user_message: userText });
    const discovered = parseDiscoverAgentsOutput(discoveredRaw);
    const agents = Array.isArray(discovered.agents) ? discovered.agents : [];

    const selectedAgent = await SelectAgentFocus({
      user_message: userText,
      agents,
    });

    const pass1Raw = await BuildExtrospectionPlan({
      intent,
      selected_agent: selectedAgent ?? null,
    });
    const pass1PayloadJson = extractPayloadJson(pass1Raw);

    let pass2PayloadJson = "{}";
    try {
      const pass2Raw = await BuildExtrospectionDrilldownPlan({
        intent,
        selected_agent: selectedAgent ?? null,
        pass1_payload_json: pass1PayloadJson,
      });
      pass2PayloadJson = extractPayloadJson(pass2Raw);
    } catch {
      // Keep pass-1 evidence when drilldown continuation fails.
      pass2PayloadJson = "{}";
    }

    const response = await SummarizeExtrospectionReport({
      user_message: userText,
      intent,
      selected_agent: selectedAgent ?? null,
      agents,
      pass1_payload_json: pass1PayloadJson,
      pass2_payload_json: pass2PayloadJson,
    });
    return { message: response };

  },
});

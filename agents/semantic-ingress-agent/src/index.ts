/// <reference path="./baml-runtime.d.ts" />
import type {
  HostDispatchAck,
  HostDispatchRequest,
  RunContext,
  SessionResult,
} from "./baml-runtime";

// --- Constants ---

const RAW_SOURCE_SCHEMA_VERSION = "host.source-records.v1";
const RAW_SOURCE_ROUTING_KEY = "event:intake";
const DISCOVER_AGENTS_TOOL_NAME = "system/discover_agents";
const INTERNAL_A2A_TOOL_NAME = "system/internal_a2a";
const MAX_SINGLE_SEND_CONTINUE_STEPS = 16;
const MAX_DELEGATION_CONTINUE_STEPS = 64;
const MAX_SUMMARY_CHARS = 1_200;

// --- Types ---

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(readInput?: Record<string, unknown>): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

type SourceContext = {
  source_kind: string;
  source_key: string;
  source_label: string;
};

type SlackRecord = {
  ts: string;
  thread_ts: string | null;
  user: string | null;
  text: string;
};

type RawSourceBatch = {
  schema_version: string;
  emitted_at_unix: number;
  source: SourceContext;
  records: SlackRecord[];
};

type InterpretationResult = {
  executive_summary: string;
  current_objectives: string[];
  workflow_seed: unknown;
  derived_tasks: DerivedTask[];
};

type DerivedTask = {
  key: string;
  title: string;
  description: string;
  priority: string;
};

type RoutingDecisionKind =
  | "create_pm_work"
  | "execute_existing_work"
  | "noop";

type RoutingDecision = {
  kind: RoutingDecisionKind;
  reason: string;
  requiredCapabilities: string[];
};

type DiscoveredAgent = {
  name: string;
  version: string;
  agent_package: string;
  agent_instance_id: string;
  tools: string[];
  description?: string;
  capabilities: string[];
};

type DownstreamSelection =
  | { kind: "none" }
  | { kind: "matched"; agent: DiscoveredAgent }
  | { kind: "ambiguous"; candidates: string[] };

type ThreadInterpretation = {
  threadKey: string;
  records: SlackRecord[];
  interpretation: InterpretationResult;
  decision: RoutingDecision;
};

// --- Utility functions ---

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function normalizeOptionalString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function truncateText(value: string, maxChars: number): string {
  if (value.length <= maxChars) return value;
  return `${value.slice(0, Math.max(0, maxChars - 3)).trimEnd()}...`;
}

function parseStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === "string");
}

function parseObjectArray(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is Record<string, unknown> => isObject(entry));
}

// --- Raw source record parsing ---

function parseRawSourceBatch(message: unknown): RawSourceBatch | null {
  if (!isObject(message)) return null;
  if (message.schema_version !== RAW_SOURCE_SCHEMA_VERSION) return null;

  const source = isObject(message.source) ? message.source : null;
  if (!source) return null;

  const sourceKind = normalizeOptionalString(source.source_kind);
  const sourceKey = normalizeOptionalString(source.source_key);
  const sourceLabel = normalizeOptionalString(source.source_label);
  if (!sourceKind || !sourceKey || !sourceLabel) return null;

  const records = parseObjectArray(message.records).map((record) => ({
    ts: normalizeOptionalString(record.ts) ?? "",
    thread_ts: normalizeOptionalString(record.thread_ts),
    user: normalizeOptionalString(record.user),
    text: normalizeOptionalString(record.text) ?? "",
  }));

  return {
    schema_version: RAW_SOURCE_SCHEMA_VERSION,
    emitted_at_unix: typeof message.emitted_at_unix === "number" ? message.emitted_at_unix : 0,
    source: {
      source_kind: sourceKind,
      source_key: sourceKey,
      source_label: sourceLabel,
    },
    records,
  };
}

function extractRawSourceBatchFromDispatch(
  request: HostDispatchRequest,
): RawSourceBatch | null {
  const messages = extractDispatchMessages<Record<string, unknown>>(request);
  for (const message of messages) {
    const batch = parseRawSourceBatch(message);
    if (batch) return batch;
  }
  return null;
}

// --- Thread grouping ---

function groupRecordsByThread(records: SlackRecord[]): SlackRecord[][] {
  const groups = new Map<string, SlackRecord[]>();
  for (const record of records) {
    const key = record.thread_ts ?? record.ts;
    const group = groups.get(key);
    if (group) {
      group.push(record);
    } else {
      groups.set(key, [record]);
    }
  }
  return [...groups.values()];
}

function threadKeyForRecords(records: SlackRecord[]): string {
  const first = records[0];
  if (!first) return "unknown-thread";
  return first.thread_ts ?? first.ts ?? "unknown-thread";
}

// --- BAML interpretation ---

async function interpretRecords(
  source: SourceContext,
  records: SlackRecord[],
): Promise<InterpretationResult> {
  const result = await InterpretSlackRecords({ source, records });
  return {
    executive_summary: result.executive_summary ?? "",
    current_objectives: result.current_objectives ?? [],
    workflow_seed: result.workflow_seed ?? null,
    derived_tasks: (result.derived_tasks ?? []).map((task) => ({
      key: task.key ?? "",
      title: task.title ?? "",
      description: task.description ?? "",
      priority: task.priority ?? "medium",
    })),
  };
}

// --- Routing decision ---

function deriveRoutingDecision(
  sourceKind: string,
  interpretation: InterpretationResult,
): RoutingDecision {
  if (interpretation.derived_tasks.length === 0) {
    return {
      kind: "noop",
      reason: "The interpretation produced no derived work items.",
      requiredCapabilities: [],
    };
  }

  if (sourceKind === "slack") {
    return {
      kind: "create_pm_work",
      reason: "Slack-origin interpretations should become project-management work items.",
      requiredCapabilities: ["clickup:create-task"],
    };
  }

  return {
    kind: "execute_existing_work",
    reason: "Source records from this source kind fall back to the generic execution path.",
    requiredCapabilities: ["coordination:routing"],
  };
}

// --- Agent discovery and delegation ---

async function runSingleSendSession(
  toolName: string,
  openInput: Record<string, unknown>,
  sendInput: Record<string, unknown>,
): Promise<unknown> {
  let toolSession: ToolSessionHandle | null = null;
  try {
    toolSession = await openToolSession(toolName, openInput);
    await toolSession.send(sendInput);
    for (let step = 0; step < MAX_SINGLE_SEND_CONTINUE_STEPS; step += 1) {
      const next = await toolSession.continue();
      const nextObj = isObject(next) ? next : null;
      const status =
        nextObj && typeof nextObj.status === "string" ? nextObj.status.toLowerCase() : null;
      if (status === "streaming") continue;
      if (status === "error") {
        const errorMessage =
          nextObj &&
          isObject(nextObj.error) &&
          typeof nextObj.error.message === "string"
            ? nextObj.error.message
            : "tool session returned error status";
        throw new Error(errorMessage);
      }
      await toolSession.finish();
      toolSession = null;
      return nextObj && "output" in nextObj ? nextObj.output : next;
    }
    throw new Error(`tool session ${toolName} exceeded continue step budget`);
  } catch (error) {
    if (toolSession) {
      try {
        await toolSession.abort(error instanceof Error ? error.message : String(error));
      } catch {
        // Ignore abort failures while already handling the upstream error.
      }
    }
    throw error;
  }
}

function parseDiscoveredAgent(value: unknown): DiscoveredAgent | null {
  if (!isObject(value)) return null;
  const name = normalizeOptionalString(value.name);
  const version = normalizeOptionalString(value.version);
  const agentPackage =
    normalizeOptionalString(value.agent_package) ?? normalizeOptionalString(value.agentPackage);
  const agentInstanceId =
    normalizeOptionalString(value.agent_instance_id) ??
    normalizeOptionalString(value.agentInstanceId);
  const tools = parseStringArray(value.tools);
  if (!name || !version || !agentPackage || !agentInstanceId) return null;

  return {
    name,
    version,
    agent_package: agentPackage,
    agent_instance_id: agentInstanceId,
    tools,
    description: normalizeOptionalString(value.description) ?? undefined,
    capabilities: parseStringArray(value.capabilities),
  };
}

async function discoverAgentsByCapabilities(
  requiredCapabilities: string[],
): Promise<DiscoveredAgent[]> {
  const output = await runSingleSendSession(
    DISCOVER_AGENTS_TOOL_NAME,
    { reason: "semantic-ingress routing" },
    { requiredCapabilities },
  );
  const normalized = isObject(output) && "output" in output ? output.output : output;
  if (!isObject(normalized) || !Array.isArray(normalized.agents)) return [];

  return normalized.agents
    .map((entry: unknown) => parseDiscoveredAgent(entry))
    .filter((entry: DiscoveredAgent | null): entry is DiscoveredAgent => entry != null);
}

function preferredPackageForDecision(decision: RoutingDecision): string | null {
  if (decision.kind === "create_pm_work") return "clickup-agent";
  if (decision.kind === "execute_existing_work") return "coordinator-agent";
  return null;
}

function selectDownstreamAgent(
  agents: DiscoveredAgent[],
  decision: RoutingDecision,
): DownstreamSelection {
  const eligible = agents
    .filter((agent) => agent.agent_package !== "semantic-ingress-agent")
    .filter((agent) => agent.agent_instance_id === "default")
    .slice()
    .sort((left, right) => left.agent_package.localeCompare(right.agent_package));

  if (eligible.length === 0) return { kind: "none" };

  const preferredPackage = preferredPackageForDecision(decision);
  if (preferredPackage) {
    const preferred = eligible.find((agent) => agent.agent_package === preferredPackage);
    if (preferred) return { kind: "matched", agent: preferred };
  }

  if (eligible.length === 1) return { kind: "matched", agent: eligible[0] };

  return {
    kind: "ambiguous",
    candidates: eligible.map((agent) => `${agent.agent_package}/${agent.agent_instance_id}`),
  };
}

// --- Downstream prompt rendering ---

function renderDownstreamPrompt(
  source: SourceContext,
  interpretation: InterpretationResult,
  decision: RoutingDecision,
): string {
  const lines: string[] = [];
  lines.push(`Source: ${source.source_label} (${source.source_kind})`);
  lines.push(`Decision: ${decision.kind} — ${decision.reason}`);
  lines.push("");

  if (interpretation.executive_summary) {
    lines.push("Executive Summary:");
    lines.push(interpretation.executive_summary);
    lines.push("");
  }

  if (interpretation.current_objectives.length > 0) {
    lines.push("Current Objectives:");
    for (const objective of interpretation.current_objectives) {
      lines.push(`- ${objective}`);
    }
    lines.push("");
  }

  if (interpretation.derived_tasks.length > 0) {
    lines.push("Derived Tasks:");
    for (const [index, task] of interpretation.derived_tasks.entries()) {
      const priority = task.priority ? ` | priority: ${task.priority}` : "";
      lines.push(`${index + 1}. ${task.title} — ${task.description}${priority}`);
    }
    lines.push("");
  }

  if (isObject(interpretation.workflow_seed)) {
    const seed = interpretation.workflow_seed as Record<string, unknown>;
    const goal = normalizeOptionalString(seed.goal);
    if (goal) {
      lines.push(`Workflow Goal: ${goal}`);
    }
    const nodes = parseObjectArray(seed.investigation_nodes);
    if (nodes.length > 0) {
      lines.push("Investigation Nodes:");
      for (const node of nodes) {
        const title = normalizeOptionalString(node.title) ?? "(untitled)";
        const prompt = normalizeOptionalString(node.prompt);
        lines.push(`- ${title}${prompt ? `: ${truncateText(prompt, 300)}` : ""}`);
      }
    }
    lines.push("");
  }

  return lines.join("\n");
}

function renderCombinedDownstreamPrompt(
  source: SourceContext,
  threads: ThreadInterpretation[],
  decision: RoutingDecision,
): string {
  const lines: string[] = [];
  lines.push(`Source: ${source.source_label} (${source.source_kind})`);
  lines.push(`Decision: ${decision.kind} — ${decision.reason}`);
  lines.push(`Actionable Threads: ${threads.length}`);
  lines.push("");

  for (const [index, thread] of threads.entries()) {
    lines.push(`Thread ${index + 1}: ${thread.threadKey}`);
    lines.push(`Messages: ${thread.records.length}`);
    lines.push("");
    lines.push(renderDownstreamPrompt(source, thread.interpretation, thread.decision));
    lines.push("");
  }

  return lines.join("\n").trim();
}

// --- Delegation ---

async function delegateToAgent(
  target: DiscoveredAgent,
  prompt: string,
): Promise<string[]> {
  let toolSession: ToolSessionHandle | null = null;
  const texts: string[] = [];
  try {
    toolSession = await openToolSession(INTERNAL_A2A_TOOL_NAME, {
      target: {
        agent_package: target.agent_package,
        agent_instance_id: target.agent_instance_id,
      },
    });
    await toolSession.send({ parts: [{ text: prompt }] });

    for (let step = 0; step < MAX_DELEGATION_CONTINUE_STEPS; step += 1) {
      const next = await toolSession.continue();
      const nextObj = isObject(next) ? next : null;
      const status =
        nextObj && typeof nextObj.status === "string" ? nextObj.status.toLowerCase() : null;

      const output = nextObj && "output" in nextObj ? nextObj.output : next;
      if (isObject(output) && Array.isArray(output.chunks)) {
        for (const chunk of output.chunks) {
          if (!isObject(chunk)) continue;
          if (isObject(chunk.message) && Array.isArray(chunk.message.parts)) {
            for (const part of chunk.message.parts) {
              if (isObject(part) && typeof part.text === "string") {
                const text = part.text.trim();
                if (text.length > 0) texts.push(text);
              }
            }
          }
        }
      }

      if (status === "streaming") continue;

      if (status === "suspended") {
        await toolSession.abort("downstream agent suspended (requires input/auth); cannot fulfill dispatch");
        toolSession = null;
        throw new Error(
          `delegation to ${target.agent_package}/${target.agent_instance_id} suspended — ` +
          `downstream agent requires interactive input that dispatch cannot provide`,
        );
      }

      if (status === "error") {
        const errorMessage =
          nextObj &&
          isObject(nextObj.error) &&
          typeof nextObj.error.message === "string"
            ? nextObj.error.message
            : "delegation target returned error status";
        throw new Error(errorMessage);
      }

      if (status === "done") {
        break;
      }

      const completion =
        isObject(output) && typeof output.completion === "string"
          ? output.completion.toUpperCase()
          : null;
      if (completion === "DONE") {
        break;
      }
    }

    await toolSession.finish();
    toolSession = null;
    return texts;
  } catch (error) {
    if (toolSession) {
      try {
        await toolSession.abort(error instanceof Error ? error.message : String(error));
      } catch {
        // Ignore abort failures while already handling the upstream error.
      }
    }
    throw error;
  }
}

// --- Summary rendering ---

function renderRouteSummary(
  source: SourceContext,
  decision: RoutingDecision,
  target: DiscoveredAgent,
  downstreamTexts: string[],
): string {
  const lines: string[] = [];
  lines.push(
    `Routed ${source.source_label} (${decision.kind}) → ` +
    `${target.agent_package}/${target.agent_instance_id}.`,
  );
  if (downstreamTexts.length > 0) {
    lines.push("");
    lines.push("Downstream response:");
    for (const text of downstreamTexts.slice(0, 6)) {
      lines.push(`- ${truncateText(text, 600)}`);
    }
  }
  return lines.join("\n");
}

// --- Entry points ---

async function handleRawSourceDispatch(
  request: HostDispatchRequest,
): Promise<HostDispatchAck> {
  const batch = extractRawSourceBatchFromDispatch(request);
  if (!batch) {
    return {
      accepted: false,
      detail:
        `semantic-ingress-agent expected ${RAW_SOURCE_SCHEMA_VERSION} payload ` +
        `in dispatch.messages[], got none.`,
    };
  }

  if (batch.records.length === 0) {
    return {
      accepted: true,
      detail: `${batch.source.source_label}: no records in batch, skipping.`,
    };
  }

  const threadGroups = groupRecordsByThread(batch.records);
  const noopSummaries: string[] = [];
  const actionableThreads: ThreadInterpretation[] = [];
  let selectedDecision: RoutingDecision | null = null;

  for (const threadRecords of threadGroups) {
    const interpretation = await interpretRecords(batch.source, threadRecords);
    const decision = deriveRoutingDecision(batch.source.source_kind, interpretation);
    const threadKey = threadKeyForRecords(threadRecords);

    if (decision.kind === "noop") {
      noopSummaries.push(`thread(${threadKey}): ${decision.reason}`);
      continue;
    }

    if (selectedDecision && selectedDecision.kind !== decision.kind) {
      return {
        accepted: false,
        detail:
          `semantic-ingress-agent cannot mix routing decisions within one source batch: ` +
          `${selectedDecision.kind} and ${decision.kind}.`,
      };
    }
    selectedDecision = decision;
    actionableThreads.push({
      threadKey,
      records: threadRecords,
      interpretation,
      decision,
    });
  }

  if (actionableThreads.length === 0) {
    const detail =
      noopSummaries.join("\n") ||
      `${batch.source.source_label}: processed ${threadGroups.length} thread(s) with no derived work.`;
    return {
      accepted: true,
      detail: truncateText(detail, MAX_SUMMARY_CHARS),
    };
  }

  const decision = selectedDecision;
  if (!decision) {
    return {
      accepted: false,
      detail: "semantic-ingress-agent internal error: actionable threads had no routing decision.",
    };
  }
  const agents = await discoverAgentsByCapabilities(decision.requiredCapabilities);
  const selection = selectDownstreamAgent(agents, decision);

  if (selection.kind === "none") {
    return {
      accepted: false,
      detail:
        `No downstream agent matched required capabilities: ` +
        `${decision.requiredCapabilities.join(", ")}.`,
    };
  }
  if (selection.kind === "ambiguous") {
    return {
      accepted: false,
      detail:
        `Multiple downstream agents matched required capabilities ` +
        `${decision.requiredCapabilities.join(", ")}: ${selection.candidates.join(", ")}.`,
    };
  }

  const target = selection.agent;
  const prompt = renderCombinedDownstreamPrompt(batch.source, actionableThreads, decision);
  const downstreamTexts = await delegateToAgent(target, prompt);
  const routedSummary =
    `Processed ${threadGroups.length} thread(s); routed ${actionableThreads.length} actionable thread(s).`;
  const detail = [
    routedSummary,
    renderRouteSummary(batch.source, decision, target, downstreamTexts),
    ...noopSummaries,
  ]
    .filter((entry) => entry.trim().length > 0)
    .join("\n");

  return {
    accepted: true,
    detail: truncateText(detail, MAX_SUMMARY_CHARS),
  }
}

__chat_register({
  run: async (_ctx: RunContext): Promise<SessionResult> => {
    return {
      error:
        "semantic-ingress-agent processes host.source-records.v1 dispatch events. " +
        "It does not handle direct A2A messages.",
    };
  },

  onDispatch: async (request: HostDispatchRequest): Promise<HostDispatchAck> => {
    const messageType = normalizeOptionalString(request.message_type);
    if (messageType !== RAW_SOURCE_SCHEMA_VERSION) {
      return {
        accepted: false,
        detail:
          `semantic-ingress-agent expected message_type ${RAW_SOURCE_SCHEMA_VERSION}, ` +
          `got ${messageType ?? "missing"}.`,
      };
    }

    const routingKey = normalizeOptionalString(request.routing_key);
    if (routingKey !== RAW_SOURCE_ROUTING_KEY) {
      return {
        accepted: false,
        detail:
          `semantic-ingress-agent expected routing_key ${RAW_SOURCE_ROUTING_KEY}, ` +
          `got ${routingKey ?? "missing"}.`,
      };
    }

    try {
      return await handleRawSourceDispatch(request);
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      return {
        accepted: false,
        detail: `semantic-ingress-agent failed: ${reason}`,
      };
    }
  },
});

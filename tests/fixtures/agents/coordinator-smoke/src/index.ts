/// <reference path="./baml-runtime.d.ts" />
import type { ChatMessage, RunContext, SessionEmitter, SessionResult } from "./baml-runtime";

type DelegatedChunk = {
  message?: {
    parts?: Array<{ text?: string }>;
  };
  task?: string;
  statusUpdate?: string;
};

type DelegatedResult = {
  chunks?: DelegatedChunk[];
  message?: {
    parts?: Array<{ text?: string }>;
  };
};

type CoordinatorAnswer = {
  answer: string;
  actionable_goals: CoordinatorGoal[];
  sources: string[];
  confidence: number;
  gaps: string[];
  clarification_question?: string | null;
};

type CoordinatorGoal = {
  goal: string;
  owner?: string | null;
  due_date?: string | null;
};

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

type RouteTarget = {
  agent_package: string;
  agent_instance_id: string;
};

type DiscoveredAgent = {
  name: string;
  version: string;
  agent_package: string;
  agent_instance_id: string;
  tools: string[];
  description?: string;
  capabilities?: string[];
};

type DiscoverAgentsOutput = {
  agents: DiscoveredAgent[];
  done?: boolean;
};

type AgentCandidate = {
  agent_package: string;
  agent_instance_id: string;
  name: string;
  description: string | null;
  capabilities: string[];
  tools: string[];
};

declare function PlanCoordinatorWorkflow(args: {
  user_message: string;
  available_agents: AgentCandidate[];
  conversation_context?: string | null;
}): Promise<unknown>;

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

const MAX_FANOUT_CONCURRENCY = 3;
const MAX_TRANSCRIPT_CHARS = 12_000;
const MAX_CONVERSATION_CONTEXT_CHARS = 4_000;
const MAX_SINGLE_SEND_CONTINUE_STEPS = 16;
const MAX_DELEGATION_CONTINUE_STEPS = 128;
const MAX_WORKFLOW_NODES = 30;
const MAX_FOREACH_EXPANSIONS = 50;
const MAX_WORKFLOW_ITERATIONS = 8;

const INTERNAL_A2A_TOOL_NAME = "system/internal_a2a";
const DISCOVER_AGENTS_TOOL_NAME = "system/discover_agents";

type WorkflowNodeKind =
  | "call_agent"
  | "foreach"
  | "synthesize"
  | "clarify"
  | "direct_answer";

type WorkflowForeachTemplate = {
  id_prefix: string;
  target: RouteTarget;
  prompt_template: string;
  max_items?: number;
};

type WorkflowNode = {
  id: string;
  kind: WorkflowNodeKind;
  depends_on: string[];
  target?: RouteTarget;
  prompt_template?: string;
  foreach_from?: string;
  foreach_template?: WorkflowForeachTemplate;
  synthesis_template?: string;
  rationale?: string;
};

type WorkflowPlan = {
  goal: string;
  nodes: WorkflowNode[];
  final_node_id?: string | null;
};

type NodeArtifactStatus = "pending" | "running" | "completed" | "failed" | "skipped";

type NodeArtifact = {
  node_id: string;
  status: NodeArtifactStatus;
  output_text?: string;
  output_data?: unknown;
  sources?: string[];
  error?: string;
  started_at?: string;
  ended_at?: string;
};

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function normalizeText(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function normalizeOptionalString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function getChatMessageText(message: ChatMessage | null | undefined): string {
  if (!message) return "";
  try {
    const extracted = messageText(message);
    if (typeof extracted === "string" && extracted.trim().length > 0) {
      return extracted.trim();
    }
  } catch {
    // Fallback below when runtime helper is unavailable.
  }

  if (!Array.isArray(message.parts)) return "";
  const parts = message.parts
    .map((part) => normalizeOptionalString(part.text))
    .filter((entry): entry is string => entry != null);
  return normalizeText(parts.join(" "));
}

function parseStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === "string");
}

function parseRouteTarget(value: unknown): RouteTarget | null {
  if (!isObject(value)) return null;
  if (typeof value.agent_package !== "string") return null;
  if (typeof value.agent_instance_id !== "string") return null;
  return {
    agent_package: value.agent_package,
    agent_instance_id: value.agent_instance_id,
  };
}

function parseWorkflowNodeKind(value: unknown): WorkflowNodeKind | null {
  if (typeof value !== "string") return null;
  if (value === "call_agent" || value === "CallAgent") return "call_agent";
  if (value === "foreach" || value === "Foreach") return "foreach";
  if (value === "synthesize" || value === "Synthesize") return "synthesize";
  if (value === "clarify" || value === "Clarify") return "clarify";
  if (value === "direct_answer" || value === "DirectAnswer") return "direct_answer";
  return null;
}

function parseWorkflowForeachTemplate(value: unknown): WorkflowForeachTemplate | null {
  if (!isObject(value)) return null;
  if (typeof value.id_prefix !== "string" || value.id_prefix.trim().length === 0) return null;
  if (typeof value.prompt_template !== "string" || value.prompt_template.trim().length === 0) {
    return null;
  }
  const target = parseRouteTarget(value.target);
  if (!target) return null;

  const maxItemsRaw =
    typeof value.max_items === "number" ? value.max_items : Number(value.max_items);
  const maxItems = Number.isFinite(maxItemsRaw) ? Math.floor(maxItemsRaw) : undefined;

  return {
    id_prefix: value.id_prefix.trim(),
    target,
    prompt_template: value.prompt_template.trim(),
    max_items: maxItems,
  };
}

function parseWorkflowNode(value: unknown): WorkflowNode | null {
  if (!isObject(value)) return null;
  const id = normalizeOptionalString(value.id);
  const kind = parseWorkflowNodeKind(value.kind);
  if (!id || !kind) return null;

  return {
    id,
    kind,
    depends_on: parseStringArray(value.depends_on),
    target: parseRouteTarget(value.target) || undefined,
    prompt_template: normalizeOptionalString(value.prompt_template) || undefined,
    foreach_from: normalizeOptionalString(value.foreach_from) || undefined,
    foreach_template: parseWorkflowForeachTemplate(value.foreach_template) || undefined,
    synthesis_template: normalizeOptionalString(value.synthesis_template) || undefined,
    rationale: normalizeOptionalString(value.rationale) || undefined,
  };
}

function parseWorkflowPlan(value: unknown): WorkflowPlan | null {
  if (!isObject(value) || !Array.isArray(value.nodes)) return null;
  const nodes = value.nodes
    .map((node) => parseWorkflowNode(node))
    .filter((node): node is WorkflowNode => node != null);
  if (nodes.length === 0) return null;
  if (nodes.length !== value.nodes.length) return null;

  return {
    goal: normalizeOptionalString(value.goal) || "Coordinate user request",
    nodes,
    final_node_id: normalizeOptionalString(value.final_node_id),
  };
}

function workflowTargetKey(target: RouteTarget): string {
  return `${target.agent_package}/${target.agent_instance_id}`;
}

function validateWorkflowPlan(plan: WorkflowPlan, agentRegistry: Set<string>): void {
  if (!Array.isArray(plan.nodes) || plan.nodes.length === 0) {
    throw new Error("Workflow plan must include at least one node.");
  }
  if (plan.nodes.length > MAX_WORKFLOW_NODES) {
    throw new Error(
      `Workflow plan exceeds max node count (${plan.nodes.length} > ${MAX_WORKFLOW_NODES}).`,
    );
  }

  const nodeById = new Map<string, WorkflowNode>();
  for (const node of plan.nodes) {
    if (nodeById.has(node.id)) {
      throw new Error(`Workflow plan has duplicate node id: ${node.id}`);
    }
    nodeById.set(node.id, node);
  }

  if (plan.final_node_id && !nodeById.has(plan.final_node_id)) {
    throw new Error(`Workflow final_node_id does not exist: ${plan.final_node_id}`);
  }

  for (const node of plan.nodes) {
    for (const depId of node.depends_on) {
      if (!nodeById.has(depId)) {
        throw new Error(`Workflow node ${node.id} depends on unknown node ${depId}.`);
      }
      if (depId === node.id) {
        throw new Error(`Workflow node ${node.id} cannot depend on itself.`);
      }
    }

    if (node.kind === "call_agent") {
      if (!node.target) {
        throw new Error(`Workflow node ${node.id} (call_agent) is missing target.`);
      }
      if (node.target.agent_package === "coordinator-agent") {
        throw new Error(`Workflow node ${node.id} cannot target coordinator-agent.`);
      }
      if (!node.prompt_template) {
        throw new Error(`Workflow node ${node.id} (call_agent) is missing prompt_template.`);
      }
      const key = workflowTargetKey(node.target);
      if (!agentRegistry.has(key)) {
        throw new Error(`Workflow node ${node.id} targets unknown agent ${key}.`);
      }
      continue;
    }

    if (node.kind === "foreach") {
      if (!node.foreach_from) {
        throw new Error(`Workflow node ${node.id} (foreach) is missing foreach_from.`);
      }
      if (!node.foreach_template) {
        throw new Error(`Workflow node ${node.id} (foreach) is missing foreach_template.`);
      }
      if (node.foreach_template.target.agent_package === "coordinator-agent") {
        throw new Error(`Workflow node ${node.id} foreach cannot target coordinator-agent.`);
      }
      const key = workflowTargetKey(node.foreach_template.target);
      if (!agentRegistry.has(key)) {
        throw new Error(`Workflow node ${node.id} foreach targets unknown agent ${key}.`);
      }
      const maxItems = node.foreach_template.max_items ?? MAX_FOREACH_EXPANSIONS;
      if (maxItems < 1 || maxItems > MAX_FOREACH_EXPANSIONS) {
        throw new Error(
          `Workflow node ${node.id} has invalid foreach max_items=${maxItems}; max=${MAX_FOREACH_EXPANSIONS}.`,
        );
      }
      continue;
    }

    if (node.kind === "clarify" || node.kind === "direct_answer") {
      if (!node.prompt_template) {
        throw new Error(`Workflow node ${node.id} (${node.kind}) is missing prompt_template.`);
      }
    }
  }

  const indegree = new Map<string, number>();
  const dependents = new Map<string, string[]>();
  for (const node of plan.nodes) {
    indegree.set(node.id, node.depends_on.length);
    for (const depId of node.depends_on) {
      const bucket = dependents.get(depId) || [];
      bucket.push(node.id);
      dependents.set(depId, bucket);
    }
  }

  const queue: string[] = [];
  for (const [id, degree] of indegree) {
    if (degree === 0) queue.push(id);
  }

  let visited = 0;
  while (queue.length > 0) {
    const id = queue.shift()!;
    visited += 1;
    const next = dependents.get(id) || [];
    for (const depId of next) {
      const current = indegree.get(depId);
      if (current == null) continue;
      const updated = current - 1;
      indegree.set(depId, updated);
      if (updated === 0) queue.push(depId);
    }
  }

  if (visited !== plan.nodes.length) {
    throw new Error("Workflow plan contains a dependency cycle.");
  }
}

// ---------------------------------------------------------------------------
// Delegated text extraction
// ---------------------------------------------------------------------------

function collectMessageParts(value: unknown, out: Set<string>): void {
  if (!isObject(value)) return;
  const message = value.message;
  if (!isObject(message) || !Array.isArray(message.parts)) return;
  for (const part of message.parts) {
    if (!isObject(part) || typeof part.text !== "string") continue;
    const text = part.text.trim();
    if (text.length > 0) out.add(text);
  }
}

function extractTextsFromSerializedChunkField(value: unknown): string[] {
  if (typeof value !== "string") return [];
  const trimmed = value.trim();
  if (!trimmed) return [];

  try {
    const parsed = JSON.parse(trimmed);
    const out = new Set<string>();

    collectMessageParts(parsed, out);
    if (isObject(parsed)) {
      collectMessageParts(parsed.task, out);
      collectMessageParts(parsed.statusUpdate, out);
      collectMessageParts(parsed.status, out);
    }

    return Array.from(out);
  } catch {
    return [];
  }
}

function collectDelegatedTexts(value: unknown): string[] {
  const out = new Set<string>();
  const visited = new WeakSet<object>();

  const pushMessageParts = (message: {
    parts?: Array<{ text?: string }>;
  }): void => {
    if (!Array.isArray(message.parts)) return;
    for (const part of message.parts) {
      if (!part || typeof part.text !== "string") continue;
      const text = part.text.trim();
      if (text.length > 0) out.add(text);
    }
  };

  const visit = (candidate: unknown): void => {
    if (Array.isArray(candidate)) {
      if (visited.has(candidate)) return;
      visited.add(candidate);
      for (const item of candidate) visit(item);
      return;
    }
    if (!isObject(candidate)) return;
    if (visited.has(candidate)) return;
    visited.add(candidate);

    const typed = candidate as DelegatedResult;
    if (typed.message) pushMessageParts(typed.message);
    if (Array.isArray(typed.chunks)) {
      for (const chunk of typed.chunks) {
        if (!chunk) continue;
        if (chunk.message) pushMessageParts(chunk.message);
        for (const text of extractTextsFromSerializedChunkField(chunk.task)) out.add(text);
        for (const text of extractTextsFromSerializedChunkField(chunk.statusUpdate)) {
          out.add(text);
        }
      }
    }

    for (const nested of Object.values(candidate)) {
      visit(nested);
    }
  };

  visit(value);
  return Array.from(out);
}

// ---------------------------------------------------------------------------
// Discovery parsing
// ---------------------------------------------------------------------------

function parseDiscoveredAgent(value: unknown): DiscoveredAgent | null {
  if (!isObject(value)) return null;
  const name = typeof value.name === "string" ? value.name : null;
  const version = typeof value.version === "string" ? value.version : null;
  const agentPackage =
    typeof value.agent_package === "string"
      ? value.agent_package
      : typeof value.agentPackage === "string"
        ? value.agentPackage
        : null;
  const agentInstanceId =
    typeof value.agent_instance_id === "string"
      ? value.agent_instance_id
      : typeof value.agentInstanceId === "string"
        ? value.agentInstanceId
        : null;
  if (!name || !version || !agentPackage || !agentInstanceId) return null;
  if (!Array.isArray(value.tools) || !value.tools.every((entry) => typeof entry === "string")) {
    return null;
  }

  const description = normalizeOptionalString(value.description);
  const capabilities = Array.isArray(value.capabilities)
    ? value.capabilities.filter((entry): entry is string => typeof entry === "string")
    : [];

  return {
    name,
    version,
    agent_package: agentPackage,
    agent_instance_id: agentInstanceId,
    tools: value.tools,
    description: description || undefined,
    capabilities,
  };
}

function unwrapToolSessionNextOutput(value: unknown): unknown {
  if (!isObject(value)) return value;
  if ("output" in value) return value.output;
  return value;
}

function parseDiscoverAgentsOutput(value: unknown): DiscoverAgentsOutput | null {
  const normalized = unwrapToolSessionNextOutput(value);
  if (!isObject(normalized) || !Array.isArray(normalized.agents)) return null;

  const agents = normalized.agents
    .map((entry) => parseDiscoveredAgent(entry))
    .filter((entry): entry is DiscoveredAgent => entry != null);

  if (agents.length === 0 && normalized.agents.length > 0) return null;

  return {
    agents,
    done: typeof normalized.done === "boolean" ? normalized.done : undefined,
  };
}

type ToolSessionStepStatus = "streaming" | "suspended" | "done" | "error" | "unknown";

type ParsedToolSessionStep = {
  status: ToolSessionStepStatus;
  output: unknown;
  errorMessage: string | null;
};

type DrainToolSessionResult = {
  steps: ParsedToolSessionStep[];
  hitStepLimit: boolean;
};

function parseToolSessionStep(step: unknown): ParsedToolSessionStep {
  if (!isObject(step)) {
    return { status: "unknown", output: step, errorMessage: null };
  }

  const rawStatus = typeof step.status === "string" ? step.status.toLowerCase() : null;
  const status: ToolSessionStepStatus =
    rawStatus === "streaming" ||
    rawStatus === "suspended" ||
    rawStatus === "done" ||
    rawStatus === "error"
      ? rawStatus
      : "unknown";

  const output = "output" in step ? step.output : step;
  const errObj = isObject(step.error) ? step.error : null;
  const errorMessage =
    errObj && typeof errObj.message === "string"
      ? errObj.message
      : status === "error"
        ? "Tool session returned error status."
        : null;

  return { status, output, errorMessage };
}

async function drainToolSession(
  sessionHandle: ToolSessionHandle,
  maxSteps: number,
): Promise<DrainToolSessionResult> {
  const steps: ParsedToolSessionStep[] = [];
  for (let step = 0; step < maxSteps; step++) {
    const parsed = parseToolSessionStep(await sessionHandle.continue());
    steps.push(parsed);
    if (parsed.status !== "streaming") return { steps, hitStepLimit: false };
  }
  return { steps, hitStepLimit: true };
}

// ---------------------------------------------------------------------------
// Tool session management
// ---------------------------------------------------------------------------

async function runSingleSendSession(
  toolName: string,
  openInput: Record<string, unknown>,
  sendInput: Record<string, unknown>,
): Promise<unknown> {
  let sessionHandle: ToolSessionHandle | null = null;
  try {
    sessionHandle = await openToolSession(toolName, openInput);
    await sessionHandle.send(sendInput);
    const drained = await drainToolSession(sessionHandle, MAX_SINGLE_SEND_CONTINUE_STEPS);
    if (drained.hitStepLimit) {
      throw new Error(
        `Tool session did not reach terminal status within ${MAX_SINGLE_SEND_CONTINUE_STEPS} continue steps.`,
      );
    }
    const terminal = drained.steps[drained.steps.length - 1] || null;
    if (terminal?.status === "error") {
      throw new Error(terminal.errorMessage || "Tool session returned error status.");
    }
    await sessionHandle.finish();
    sessionHandle = null;
    return terminal ? terminal.output : null;
  } catch (err) {
    if (sessionHandle) {
      const reason = err instanceof Error ? err.message : String(err);
      try {
        await sessionHandle.abort(reason);
      } catch {
        // Ignore abort errors while already handling an upstream failure.
      }
    }
    throw err;
  }
}

async function discoverAgents(userText: string): Promise<DiscoveredAgent[]> {
  const response = await runSingleSendSession(
    DISCOVER_AGENTS_TOOL_NAME,
    { reason: "Discover available specialist agents for coordinator routing" },
    { query: userText, limit: 100, offset: 0 },
  );
  const parsed = parseDiscoverAgentsOutput(response);
  return parsed?.agents || [];
}

async function delegateToAgent(target: RouteTarget, prompt: string): Promise<string[]> {
  let sessionHandle: ToolSessionHandle | null = null;
  try {
    sessionHandle = await openToolSession(INTERNAL_A2A_TOOL_NAME, {
      target,
    });
    await sessionHandle.send({ parts: [{ text: prompt }] });
    const drained = await drainToolSession(sessionHandle, MAX_DELEGATION_CONTINUE_STEPS);
    if (drained.hitStepLimit) {
      throw new Error(
        `Delegated session did not reach terminal status within ${MAX_DELEGATION_CONTINUE_STEPS} continue steps.`,
      );
    }
    const terminal = drained.steps[drained.steps.length - 1] || null;
    if (terminal?.status === "error") {
      throw new Error(terminal.errorMessage || "Delegated session returned error status.");
    }
    await sessionHandle.finish();
    sessionHandle = null;
    return collectDelegatedTexts(drained.steps.map((step) => step.output));
  } catch (err) {
    if (sessionHandle) {
      const reason = err instanceof Error ? err.message : String(err);
      try {
        await sessionHandle.abort(reason);
      } catch {
        // Ignore abort errors while on error path.
      }
    }
    throw err;
  }
}

function buildAgentCandidates(agents: DiscoveredAgent[]): AgentCandidate[] {
  return agents
    .filter((a) => a.agent_package !== "coordinator-agent")
    .filter((a) => a.agent_instance_id === "default")
    .map((a) => ({
      agent_package: a.agent_package,
      agent_instance_id: a.agent_instance_id,
      name: a.name,
      description: a.description || null,
      capabilities: a.capabilities || [],
      tools: a.tools,
    }));
}

function buildAgentRegistry(agents: DiscoveredAgent[]): Set<string> {
  return new Set(
    agents.map((agent) => `${agent.agent_package}/${agent.agent_instance_id}`),
  );
}

async function planWorkflow(
  userText: string,
  agents: DiscoveredAgent[],
  conversationSummary: string | null,
): Promise<WorkflowPlan> {
  const candidates = buildAgentCandidates(agents);
  const agentRegistry = buildAgentRegistry(agents);

  try {
    const rawPlan = await PlanCoordinatorWorkflow({
      user_message: userText,
      available_agents: candidates,
      conversation_context: conversationSummary || null,
    });
    const parsed = parseWorkflowPlan(rawPlan);
    if (!parsed) {
      throw new Error("PlanCoordinatorWorkflow returned an unparsable plan.");
    }
    validateWorkflowPlan(parsed, agentRegistry);
    return parsed;
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    throw new Error(`Workflow planner failed: ${reason}`);
  }
}

// ---------------------------------------------------------------------------
// Conversation context
// ---------------------------------------------------------------------------

function getConversationSummary(ctx: RunContext): string | null {
  const tags = (ctx as unknown as { tags?: unknown }).tags;
  if (!isObject(tags)) return null;
  const history = tags.conversation_history;
  if (typeof history !== "string" || history.trim().length === 0) return null;
  return history.slice(0, MAX_CONVERSATION_CONTEXT_CHARS);
}

// ---------------------------------------------------------------------------
// Synthesis and rendering
// ---------------------------------------------------------------------------

function toCoordinatorAnswer(value: unknown): CoordinatorAnswer | null {
  if (!isObject(value) || typeof value.answer !== "string") return null;

  const actionable = Array.isArray(value.actionable_goals)
    ? value.actionable_goals
        .map((entry): CoordinatorGoal | null => {
          if (!isObject(entry)) return null;
          const goal = normalizeOptionalString(entry.goal);
          if (!goal) return null;
          return {
            goal,
            owner: normalizeOptionalString(entry.owner),
            due_date: normalizeOptionalString(entry.due_date),
          };
        })
        .filter((entry): entry is CoordinatorGoal => entry != null)
    : [];

  const sources = Array.isArray(value.sources)
    ? value.sources.filter((entry): entry is string => typeof entry === "string")
    : [];
  const gaps = Array.isArray(value.gaps)
    ? value.gaps.filter((entry): entry is string => typeof entry === "string")
    : [];

  const confidenceRaw = value.confidence;
  const parsedConfidence =
    typeof confidenceRaw === "number" ? confidenceRaw : Number(confidenceRaw);
  const confidence = Number.isFinite(parsedConfidence)
    ? Math.max(0, Math.min(1, parsedConfidence))
    : 0.0;

  const clarificationQuestion =
    typeof value.clarification_question === "string"
      ? value.clarification_question
      : null;

  return {
    answer: value.answer.trim(),
    actionable_goals:
      actionable.length > 0
        ? actionable
        : [{ goal: "None identified from current evidence", owner: null, due_date: null }],
    sources: sources.length > 0 ? sources : ["None"],
    confidence,
    gaps: gaps.length > 0 ? gaps : ["None observed"],
    clarification_question: clarificationQuestion,
  };
}

function goalHasOwnerOrDate(goal: CoordinatorGoal): boolean {
  return Boolean(
    (goal.owner && goal.owner.length > 0) || (goal.due_date && goal.due_date.length > 0),
  );
}

function renderCoordinatorAnswer(answer: CoordinatorAnswer): string {
  const lines: string[] = [];
  lines.push("Answer:");
  lines.push(answer.answer || "No answer available.");
  lines.push("");

  const hasOwnerOrDate = answer.actionable_goals.some(goalHasOwnerOrDate);
  lines.push(
    hasOwnerOrDate
      ? "Actionable Goals (Owner/Date Present):"
      : "Actionable Goals (Owner/Date Missing In Evidence):",
  );
  for (const goal of answer.actionable_goals) {
    if (!goalHasOwnerOrDate(goal)) {
      lines.push(`- ${goal.goal}`);
      continue;
    }
    const tags: string[] = [];
    if (goal.owner) tags.push(`owner: ${goal.owner}`);
    if (goal.due_date) tags.push(`due: ${goal.due_date}`);
    lines.push(`- ${goal.goal} (${tags.join("; ")})`);
  }
  if (!hasOwnerOrDate) {
    lines.push("- Owner/date details were not explicit in the current sources.");
  }
  lines.push("");
  lines.push("Sources:");
  for (const source of answer.sources) lines.push(`- ${source}`);
  lines.push("");
  lines.push(`Confidence: ${answer.confidence.toFixed(2)}`);
  lines.push("");
  lines.push("Gaps:");
  for (const gap of answer.gaps) lines.push(`- ${gap}`);

  if (answer.clarification_question) {
    lines.push("");
    lines.push("Clarification:");
    lines.push(`- ${answer.clarification_question}`);
  }

  return lines.join("\n");
}

// ---------------------------------------------------------------------------
// Evidence collection (single target)
// ---------------------------------------------------------------------------

async function collectEvidence(
  delegationPrompt: string,
  target: RouteTarget,
): Promise<string[]> {
  let chunkTexts: string[];
  try {
    chunkTexts = await delegateToAgent(target, delegationPrompt);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    return [`Delegation error: ${reason}`];
  }

  const joined = normalizeText(chunkTexts.join("\n"));
  if (!joined) return [];

  return [joined];
}

// ---------------------------------------------------------------------------
// Synthesis
// ---------------------------------------------------------------------------

async function synthesize(
  userText: string,
  transcript: string,
  _conversationSummary: string | null,
): Promise<string> {
  let synthesizedRaw: unknown;
  try {
    synthesizedRaw = await SynthesizeCoordinatorResponse({
      user_message: userText,
      delegated_transcript: transcript,
    });
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    return [
      "Answer:",
      "I gathered delegated evidence but synthesis failed this turn.",
      "",
      "Actionable Goals (Owner/Date Missing In Evidence):",
      "- None identified from current evidence",
      "- Owner/date details were not explicit in the current sources.",
      "",
      "Sources:",
      "- None",
      "",
      "Confidence: 0.35",
      "",
      "Gaps:",
      `- Synthesis failure: ${reason}`,
      "",
      "Clarification:",
      "- Which exact source should I prioritize?",
    ].join("\n");
  }

  const synthesized = toCoordinatorAnswer(synthesizedRaw);
  if (!synthesized) {
    return [
      "Answer:",
      "I collected evidence but could not produce a structured synthesis.",
      "",
      "Evidence snapshot:",
      `- ${transcript.slice(0, 1200)}`,
      "",
      "Confidence: 0.40",
      "",
      "Gaps:",
      "- Structured synthesis unavailable for this turn.",
      "",
      "Clarification:",
      "- Which specific source should I prioritize?",
    ].join("\n");
  }

  return renderCoordinatorAnswer(synthesized);
}

// ---------------------------------------------------------------------------
// Workflow execution (Phase 3)
// ---------------------------------------------------------------------------

const MAX_INTERPOLATION_CHARS = 8_000;

function isTerminalArtifactStatus(status: NodeArtifactStatus): boolean {
  return status === "completed" || status === "failed" || status === "skipped";
}

function resolveArtifactPath(path: string, artifacts: Map<string, NodeArtifact>): unknown {
  const segments = path.split(".").filter((segment) => segment.length > 0);
  if (segments.length < 2) return undefined;

  const [nodeId, ...rest] = segments;
  let current: unknown = artifacts.get(nodeId);
  for (const segment of rest) {
    if (!isObject(current)) return undefined;
    current = current[segment];
  }
  return current;
}

function interpolateTemplate(template: string, artifacts: Map<string, NodeArtifact>): string {
  return template.replace(/\{\{([\w.]+)\}\}/g, (match, path) => {
    const resolved = resolveArtifactPath(path, artifacts);
    if (resolved === undefined) return match;
    if (typeof resolved === "string") return resolved.slice(0, MAX_INTERPOLATION_CHARS);
    try {
      return JSON.stringify(resolved).slice(0, MAX_INTERPOLATION_CHARS);
    } catch {
      return String(resolved).slice(0, MAX_INTERPOLATION_CHARS);
    }
  });
}

function nodeHasFailedDependency(
  node: WorkflowNode,
  artifacts: Map<string, NodeArtifact>,
): boolean {
  for (const depId of node.depends_on) {
    const dep = artifacts.get(depId);
    if (dep && (dep.status === "failed" || dep.status === "skipped")) {
      return true;
    }
  }
  return false;
}

function nodeDependenciesCompleted(
  node: WorkflowNode,
  artifacts: Map<string, NodeArtifact>,
): boolean {
  return node.depends_on.every((depId) => artifacts.get(depId)?.status === "completed");
}

function computeWaves(nodes: WorkflowNode[]): WorkflowNode[][] {
  const nodeById = new Map<string, WorkflowNode>();
  const indegree = new Map<string, number>();
  const dependents = new Map<string, string[]>();

  for (const node of nodes) {
    nodeById.set(node.id, node);
    indegree.set(node.id, node.depends_on.length);
    for (const depId of node.depends_on) {
      const bucket = dependents.get(depId) || [];
      bucket.push(node.id);
      dependents.set(depId, bucket);
    }
  }

  let queue = nodes
    .filter((node) => (indegree.get(node.id) || 0) === 0)
    .map((node) => node.id);
  const waves: WorkflowNode[][] = [];

  while (queue.length > 0) {
    const waveIds = queue;
    queue = [];
    const waveNodes: WorkflowNode[] = [];

    for (const id of waveIds) {
      const node = nodeById.get(id);
      if (!node) continue;
      waveNodes.push(node);
      const next = dependents.get(id) || [];
      for (const nextId of next) {
        const current = indegree.get(nextId);
        if (current == null) continue;
        const updated = current - 1;
        indegree.set(nextId, updated);
        if (updated === 0) queue.push(nextId);
      }
    }

    if (waveNodes.length > 0) waves.push(waveNodes);
  }

  return waves;
}

function resolvePathFromObject(base: unknown, dottedPath: string): unknown {
  if (!dottedPath) return base;
  const segments = dottedPath.split(".").filter((segment) => segment.length > 0);
  let current: unknown = base;
  for (const segment of segments) {
    if (!isObject(current)) return undefined;
    current = current[segment];
  }
  return current;
}

function interpolateItemTemplate(template: string, item: unknown): string {
  return template.replace(/\{\{\s*(item(?:\.[\w]+)*)\s*\}\}/g, (match, expr) => {
    const path = expr === "item" ? "" : expr.slice("item.".length);
    const resolved = resolvePathFromObject(item, path);
    if (resolved === undefined) return match;
    if (typeof resolved === "string") return resolved.slice(0, MAX_INTERPOLATION_CHARS);
    try {
      return JSON.stringify(resolved).slice(0, MAX_INTERPOLATION_CHARS);
    } catch {
      return String(resolved).slice(0, MAX_INTERPOLATION_CHARS);
    }
  });
}

function expandForeachNodeInPlan(
  plan: WorkflowPlan,
  node: WorkflowNode,
  artifacts: Map<string, NodeArtifact>,
): NodeArtifact {
  const started_at = new Date().toISOString();
  if (!node.foreach_from || !node.foreach_template) {
    return {
      node_id: node.id,
      status: "failed",
      error: "foreach node missing foreach_from or foreach_template.",
      started_at,
      ended_at: new Date().toISOString(),
    };
  }

  const resolved = resolveArtifactPath(node.foreach_from, artifacts);
  if (!Array.isArray(resolved)) {
    return {
      node_id: node.id,
      status: "failed",
      error: `foreach source '${node.foreach_from}' did not resolve to an array.`,
      started_at,
      ended_at: new Date().toISOString(),
    };
  }

  const maxItems = Math.min(
    node.foreach_template.max_items ?? MAX_FOREACH_EXPANSIONS,
    MAX_FOREACH_EXPANSIONS,
  );
  const items = resolved.slice(0, maxItems);
  const existingNodeIds = new Set(plan.nodes.map((entry) => entry.id));
  const childIds: string[] = [];
  const children: WorkflowNode[] = [];

  for (let i = 0; i < items.length; i++) {
    const childId = `${node.foreach_template.id_prefix}_${i}`;
    if (existingNodeIds.has(childId)) {
      return {
        node_id: node.id,
        status: "failed",
        error: `foreach expansion produced duplicate node id: ${childId}.`,
        started_at,
        ended_at: new Date().toISOString(),
      };
    }
    existingNodeIds.add(childId);
    childIds.push(childId);
    children.push({
      id: childId,
      kind: "call_agent",
      depends_on: [...node.depends_on],
      target: node.foreach_template.target,
      prompt_template: interpolateItemTemplate(node.foreach_template.prompt_template, items[i]),
      rationale: `Expanded from foreach node ${node.id}`,
    });
  }

  plan.nodes.push(...children);

  for (const downstream of plan.nodes) {
    if (!downstream.depends_on.includes(node.id)) continue;
    const remainingDeps = downstream.depends_on.filter((depId) => depId !== node.id);
    downstream.depends_on = Array.from(new Set([...remainingDeps, ...childIds]));
  }

  return {
    node_id: node.id,
    status: "completed",
    output_data: {
      expanded_node_ids: childIds,
      expanded_count: childIds.length,
    },
    started_at,
    ended_at: new Date().toISOString(),
  };
}

async function executeWorkflowNode(
  node: WorkflowNode,
  artifacts: Map<string, NodeArtifact>,
): Promise<NodeArtifact> {
  const started_at = new Date().toISOString();

  try {
    if (node.kind === "call_agent") {
      if (!node.target || !node.prompt_template) {
        return {
          node_id: node.id,
          status: "failed",
          error: "call_agent node missing target or prompt_template.",
          started_at,
          ended_at: new Date().toISOString(),
        };
      }

      const prompt = interpolateTemplate(node.prompt_template, artifacts);
      const snippets = await collectEvidence(prompt, node.target);
      const joined = normalizeText(snippets.join("\n\n---\n\n"));
      if (!joined) {
        return {
          node_id: node.id,
          status: "failed",
          error: "Delegation returned no usable evidence.",
          started_at,
          ended_at: new Date().toISOString(),
        };
      }
      if (/^Delegation error:/i.test(joined)) {
        return {
          node_id: node.id,
          status: "failed",
          error: joined,
          output_text: joined,
          started_at,
          ended_at: new Date().toISOString(),
        };
      }

      const callArtifact: NodeArtifact = {
        node_id: node.id,
        status: "completed",
        output_text: joined,
        started_at,
        ended_at: new Date().toISOString(),
      };

      try {
        const parsed = JSON.parse(joined);
        if (parsed && typeof parsed === "object") {
          callArtifact.output_data = parsed;
        }
      } catch {
        /* prose response — leave output_data undefined */
      }

      return callArtifact;
    }

    if (node.kind === "direct_answer") {
      const rendered = interpolateTemplate(node.prompt_template || "", artifacts);
      return {
        node_id: node.id,
        status: "completed",
        output_text: rendered,
        started_at,
        ended_at: new Date().toISOString(),
      };
    }

    if (node.kind === "synthesize") {
      return {
        node_id: node.id,
        status: "completed",
        started_at,
        ended_at: new Date().toISOString(),
      };
    }

    if (node.kind === "foreach") {
      return {
        node_id: node.id,
        status: "failed",
        error: "foreach nodes must be expanded before execution.",
        started_at,
        ended_at: new Date().toISOString(),
      };
    }

    if (node.kind === "clarify") {
      return {
        node_id: node.id,
        status: "completed",
        output_text: node.prompt_template || "Can you clarify your request?",
        started_at,
        ended_at: new Date().toISOString(),
      };
    }

    return {
      node_id: node.id,
      status: "failed",
      error: `Unsupported node kind: ${node.kind}`,
      started_at,
      ended_at: new Date().toISOString(),
    };
  } catch (err) {
    return {
      node_id: node.id,
      status: "failed",
      error: err instanceof Error ? err.message : String(err),
      started_at,
      ended_at: new Date().toISOString(),
    };
  }
}

async function executeWave(
  wave: WorkflowNode[],
  artifacts: Map<string, NodeArtifact>,
  emit?: SessionEmitter,
): Promise<NodeArtifact[]> {
  const outputs: NodeArtifact[] = [];
  for (let i = 0; i < wave.length; i += MAX_FANOUT_CONCURRENCY) {
    const batch = wave.slice(i, i + MAX_FANOUT_CONCURRENCY);
    if (emit) {
      emit.message(
        `Executing ${batch.length} workflow node(s): ${batch.map((node) => node.id).join(", ")}`,
      );
    }
    const settled = await Promise.allSettled(
      batch.map((node) => executeWorkflowNode(node, artifacts)),
    );
    for (let j = 0; j < settled.length; j++) {
      const outcome = settled[j];
      if (outcome.status === "fulfilled") {
        outputs.push(outcome.value);
      } else {
        const node = batch[j];
        outputs.push({
          node_id: node.id,
          status: "failed",
          error:
            outcome.reason instanceof Error
              ? outcome.reason.message
              : String(outcome.reason),
          started_at: new Date().toISOString(),
          ended_at: new Date().toISOString(),
        });
      }
    }
  }
  return outputs;
}

type WorkflowExecutionOutcome =
  | { kind: "final"; result: SessionResult }
  | { kind: "await_input"; prompt: string };

function findFinalWorkflowNode(plan: WorkflowPlan): WorkflowNode | null {
  if (plan.final_node_id) {
    const explicit = plan.nodes.find((node) => node.id === plan.final_node_id);
    if (explicit) return explicit;
  }

  for (let i = plan.nodes.length - 1; i >= 0; i--) {
    if (plan.nodes[i].kind === "synthesize") {
      return plan.nodes[i];
    }
  }

  return plan.nodes.length > 0 ? plan.nodes[plan.nodes.length - 1] : null;
}

function collectAncestorNodeIds(plan: WorkflowPlan, startNodeId: string): Set<string> {
  const nodeById = new Map(plan.nodes.map((node) => [node.id, node] as const));
  const seen = new Set<string>();
  const stack = [startNodeId];

  while (stack.length > 0) {
    const nodeId = stack.pop();
    if (!nodeId || seen.has(nodeId)) continue;
    seen.add(nodeId);
    const node = nodeById.get(nodeId);
    if (!node) continue;
    for (const depId of node.depends_on) stack.push(depId);
  }

  return seen;
}

function buildSynthesisUserMessage(userText: string, synthesisTemplate?: string): string {
  const normalizedTemplate = normalizeOptionalString(synthesisTemplate);
  if (!normalizedTemplate) return userText;
  return `${userText}\n\nSynthesis instructions:\n${normalizedTemplate}`;
}

function buildWorkflowTranscript(
  plan: WorkflowPlan,
  artifacts: Map<string, NodeArtifact>,
  includeNodeIds?: Set<string>,
): string {
  const summary = summarizeWorkflowExecution(plan, artifacts, includeNodeIds);
  const parts: string[] = [];
  parts.push(
    `[workflow] completed=${summary.completed} failed=${summary.failed} skipped=${summary.skipped} unresolved=${summary.unresolved}`,
  );
  if (summary.failed_nodes.length > 0) {
    parts.push(
      `[workflow] failed_nodes=${summary.failed_nodes
        .slice(0, 8)
        .map((entry) => `${entry.id}:${entry.error}`)
        .join(" | ")}`,
    );
  }
  if (summary.skipped_nodes.length > 0) {
    parts.push(
      `[workflow] skipped_nodes=${summary.skipped_nodes
        .slice(0, 8)
        .map((entry) => `${entry.id}:${entry.reason}`)
        .join(" | ")}`,
    );
  }

  for (const node of plan.nodes) {
    if (includeNodeIds && !includeNodeIds.has(node.id)) continue;
    const artifact = artifacts.get(node.id);
    if (!artifact) continue;
    const status = artifact.status;
    const prefix = `[${node.id}|${node.kind}|${status}]`;
    if (artifact.status === "completed" && artifact.output_text) {
      parts.push(`${prefix}\n${artifact.output_text}`);
      continue;
    }
    if (artifact.status === "failed") {
      parts.push(`${prefix} ERROR: ${artifact.error || "unknown failure"}`);
      continue;
    }
    if (artifact.status === "skipped") {
      parts.push(`${prefix} SKIPPED: ${artifact.error || "dependency failed"}`);
      continue;
    }
    parts.push(`${prefix} STATUS: ${artifact.error || "no output text"}`);
  }
  return parts.join("\n\n---\n\n").slice(0, MAX_TRANSCRIPT_CHARS);
}

type WorkflowExecutionSummary = {
  completed: number;
  failed: number;
  skipped: number;
  unresolved: number;
  failed_nodes: Array<{ id: string; error: string }>;
  skipped_nodes: Array<{ id: string; reason: string }>;
};

function finalizeUnresolvedNodes(plan: WorkflowPlan, artifacts: Map<string, NodeArtifact>): void {
  const now = new Date().toISOString();
  for (const node of plan.nodes) {
    const existing = artifacts.get(node.id);
    if (existing && isTerminalArtifactStatus(existing.status)) continue;
    artifacts.set(node.id, {
      node_id: node.id,
      status: "skipped",
      error: "Unresolved after execution budget.",
      started_at: existing?.started_at || now,
      ended_at: now,
    });
  }
}

function summarizeWorkflowExecution(
  plan: WorkflowPlan,
  artifacts: Map<string, NodeArtifact>,
  includeNodeIds?: Set<string>,
): WorkflowExecutionSummary {
  const summary: WorkflowExecutionSummary = {
    completed: 0,
    failed: 0,
    skipped: 0,
    unresolved: 0,
    failed_nodes: [],
    skipped_nodes: [],
  };

  for (const node of plan.nodes) {
    if (includeNodeIds && !includeNodeIds.has(node.id)) continue;
    const artifact = artifacts.get(node.id);
    if (!artifact) {
      summary.unresolved += 1;
      continue;
    }
    if (artifact.status === "completed") {
      summary.completed += 1;
      continue;
    }
    if (artifact.status === "failed") {
      summary.failed += 1;
      summary.failed_nodes.push({
        id: node.id,
        error: artifact.error || "unknown failure",
      });
      continue;
    }
    if (artifact.status === "skipped") {
      summary.skipped += 1;
      summary.skipped_nodes.push({
        id: node.id,
        reason: artifact.error || "dependency failed",
      });
      continue;
    }
    summary.unresolved += 1;
  }

  return summary;
}

function renderWorkflowExecutionNotes(summary: WorkflowExecutionSummary): string {
  const lines: string[] = [];
  lines.push("Execution Notes:");
  lines.push(`- Completed nodes: ${summary.completed}`);
  lines.push(`- Failed nodes: ${summary.failed}`);
  lines.push(`- Skipped nodes: ${summary.skipped}`);
  if (summary.unresolved > 0) {
    lines.push(`- Unresolved nodes: ${summary.unresolved}`);
  }
  for (const entry of summary.failed_nodes.slice(0, 5)) {
    lines.push(`- Failed ${entry.id}: ${entry.error}`);
  }
  for (const entry of summary.skipped_nodes.slice(0, 5)) {
    lines.push(`- Skipped ${entry.id}: ${entry.reason}`);
  }
  return lines.join("\n");
}

async function executeWorkflowPlanPhase3(
  plan: WorkflowPlan,
  userText: string,
  conversationSummary: string | null,
  emit?: SessionEmitter,
): Promise<WorkflowExecutionOutcome> {
  const artifacts = new Map<string, NodeArtifact>();
  const maxPasses = MAX_WORKFLOW_NODES + MAX_FOREACH_EXPANSIONS + 10;

  for (let pass = 0; pass < maxPasses; pass++) {
    let progressed = false;
    if (emit) {
      emit.message(`Workflow execution pass ${pass + 1}`);
    }
    const waves = computeWaves(plan.nodes);

    for (const wave of waves) {
      for (const node of wave) {
        if (artifacts.has(node.id)) continue;
        if (nodeHasFailedDependency(node, artifacts)) {
          artifacts.set(node.id, {
            node_id: node.id,
            status: "skipped",
            error: "Skipped due to failed dependency.",
            started_at: new Date().toISOString(),
            ended_at: new Date().toISOString(),
          });
          progressed = true;
        }
      }

      for (const node of wave) {
        if (artifacts.has(node.id)) continue;
        if (node.kind !== "foreach") continue;
        if (!nodeDependenciesCompleted(node, artifacts)) continue;
        const expansionArtifact = expandForeachNodeInPlan(plan, node, artifacts);
        artifacts.set(node.id, expansionArtifact);
        progressed = true;
      }

      const ready = wave.filter((node) => {
        if (artifacts.has(node.id)) return false;
        if (node.kind === "foreach") return false;
        return nodeDependenciesCompleted(node, artifacts);
      });

      if (ready.length === 0) continue;

      const clarifyNode = ready.find((node) => node.kind === "clarify");
      if (clarifyNode) {
        return {
          kind: "await_input",
          prompt: clarifyNode.prompt_template || "Can you clarify your request?",
        };
      }

      const results = await executeWave(ready, artifacts, emit);
      for (const result of results) {
        artifacts.set(result.node_id, result);
      }
      progressed = true;
    }

    const allResolved = plan.nodes.every((node) =>
      isTerminalArtifactStatus(artifacts.get(node.id)?.status || "pending"),
    );
    if (allResolved) break;
    if (!progressed) break;
  }

  finalizeUnresolvedNodes(plan, artifacts);
  const summary = summarizeWorkflowExecution(plan, artifacts);

  const appendExecutionNotes = (message: string): SessionResult => {
    if (summary.failed > 0 || summary.skipped > 0 || summary.unresolved > 0) {
      return { message: `${message}\n\n${renderWorkflowExecutionNotes(summary)}` };
    }
    return { message };
  };

  const finalNode = findFinalWorkflowNode(plan);
  if (finalNode) {
    const finalArtifact = artifacts.get(finalNode.id);
    if (
      finalArtifact?.status === "completed" &&
      (finalNode.kind === "call_agent" || finalNode.kind === "direct_answer") &&
      finalArtifact.output_text
    ) {
      return { kind: "final", result: appendExecutionNotes(finalArtifact.output_text) };
    }

    if (finalNode.kind === "synthesize") {
      const scope = collectAncestorNodeIds(plan, finalNode.id);
      scope.delete(finalNode.id);
      const transcript = buildWorkflowTranscript(plan, artifacts, scope);
      if (transcript) {
        const synthesisUserMessage = buildSynthesisUserMessage(
          userText,
          finalNode.synthesis_template,
        );
        const base = await synthesize(synthesisUserMessage, transcript, conversationSummary);
        return { kind: "final", result: appendExecutionNotes(base) };
      }
    }
  }

  const directOnly = plan.nodes.every((node) => {
    if (node.kind !== "direct_answer" && node.kind !== "synthesize") return false;
    const status = artifacts.get(node.id)?.status;
    return status === "completed" || status === "skipped";
  });
  if (directOnly) {
    const message = plan.nodes
      .map((node) => artifacts.get(node.id)?.output_text)
      .filter((entry): entry is string => typeof entry === "string" && entry.trim().length > 0)
      .join("\n");
    if (!message) {
      return {
        kind: "final",
        result: { message: "No direct response was produced by the workflow plan." },
      };
    }
    return { kind: "final", result: appendExecutionNotes(message) };
  }

  const transcript = buildWorkflowTranscript(plan, artifacts);
  if (!transcript) {
    return {
      kind: "final",
      result: appendExecutionNotes(
        "I delegated according to the workflow plan, but received no usable evidence.",
      ),
    };
  }
  const base = await synthesize(userText, transcript, conversationSummary);
  return { kind: "final", result: appendExecutionNotes(base) };
}

async function runWorkflowCoordinator(ctx: RunContext): Promise<SessionResult> {
  const baseUserText = (ctx.text || "").trim();
  if (!baseUserText) {
    return { message: "Please share what you want me to coordinate." };
  }

  ctx.emit.statusChanged("TASK_STATE_WORKING");
  const baseConversationSummary = getConversationSummary(ctx);
  const clarificationTurns: string[] = [];

  let agents: DiscoveredAgent[];
  try {
    ctx.emit.message("Discovering available specialist agents...");
    agents = await discoverAgents(baseUserText);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    return { message: `Agent discovery failed: ${reason}` };
  }

  for (let iteration = 0; iteration < MAX_WORKFLOW_ITERATIONS; iteration++) {
    const conversationSummary = [
      baseConversationSummary,
      clarificationTurns.length > 0
        ? clarificationTurns.join("\n").slice(0, MAX_CONVERSATION_CONTEXT_CHARS)
        : null,
    ]
      .filter((entry): entry is string => typeof entry === "string" && entry.trim().length > 0)
      .join("\n\n")
      .slice(0, MAX_CONVERSATION_CONTEXT_CHARS);

    const effectiveUserText =
      clarificationTurns.length === 0
        ? baseUserText
        : `${baseUserText}\n\nAdditional user clarifications:\n${clarificationTurns.join("\n")}`;

    let plan: WorkflowPlan;
    try {
      ctx.emit.message(`Planning workflow (iteration ${iteration + 1})...`);
      plan = await planWorkflow(
        effectiveUserText,
        agents,
        conversationSummary.length > 0 ? conversationSummary : null,
      );
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      return { message: `Workflow planning failed: ${reason}` };
    }

    const outcome = await executeWorkflowPlanPhase3(
      plan,
      effectiveUserText,
      conversationSummary.length > 0 ? conversationSummary : null,
      ctx.emit,
    );
    if (outcome.kind === "final") {
      return outcome.result;
    }

    if (iteration >= MAX_WORKFLOW_ITERATIONS - 1) {
      return {
        message: `${outcome.prompt}\n\nReached clarification iteration limit. Please send a more specific request.`,
      };
    }

    const nextMessage = await ctx.emit.awaitInput(outcome.prompt);
    const userReply = normalizeOptionalString(getChatMessageText(nextMessage));
    if (!userReply) {
      return { message: "No clarification was provided. Please resend your request with details." };
    }

    clarificationTurns.push(`- ${userReply}`);
  }

  return {
    message: "Workflow iteration limit reached before completion. Please narrow the request.",
  };
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

async function runCoordinator(ctx: RunContext): Promise<SessionResult> {
  return runWorkflowCoordinator(ctx);
}

__chat_register({ run: runCoordinator });

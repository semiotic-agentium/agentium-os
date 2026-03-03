/// <reference path="./baml-runtime.d.ts" />
import type { RunContext, SessionResult } from "./baml-runtime";

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

declare function RouteToSpecialists(args: {
  user_message: string;
  available_agents: AgentCandidate[];
  conversation_summary?: string | null;
}): Promise<unknown>;

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
const USE_WORKFLOW_COORDINATOR = false;

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
  if (
    value === "call_agent" ||
    value === "foreach" ||
    value === "synthesize" ||
    value === "clarify" ||
    value === "direct_answer"
  ) {
    return value;
  }
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

// ---------------------------------------------------------------------------
// LLM routing
// ---------------------------------------------------------------------------

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

type LlmRoutingDecision = {
  action: string;
  targets: Array<{
    agent_package: string;
    agent_instance_id: string;
    prompt: string;
    rationale: string;
  }>;
  direct_message?: string | null;
  reasoning: string;
};

function parseLlmRoutingDecision(value: unknown): LlmRoutingDecision | null {
  if (!isObject(value)) return null;
  if (typeof value.action !== "string") return null;
  if (!Array.isArray(value.targets)) return null;
  if (typeof value.reasoning !== "string") return null;

  const targets = value.targets
    .filter(
      (t): t is Record<string, unknown> =>
        isObject(t) &&
        typeof t.agent_package === "string" &&
        typeof t.agent_instance_id === "string" &&
        typeof t.prompt === "string" &&
        typeof t.rationale === "string",
    )
    .map((t) => ({
      agent_package: t.agent_package as string,
      agent_instance_id: t.agent_instance_id as string,
      prompt: t.prompt as string,
      rationale: t.rationale as string,
    }));

  return {
    action: value.action,
    targets,
    direct_message: typeof value.direct_message === "string" ? value.direct_message : null,
    reasoning: value.reasoning,
  };
}

async function llmRoute(
  userText: string,
  agents: DiscoveredAgent[],
  conversationSummary: string | null,
): Promise<LlmRoutingDecision> {
  const candidates = buildAgentCandidates(agents);

  const raw = await RouteToSpecialists({
    user_message: userText,
    available_agents: candidates,
    conversation_summary: conversationSummary || null,
  });

  const parsed = parseLlmRoutingDecision(raw);
  if (!parsed) {
    return {
      action: "DirectAnswer",
      targets: [],
      direct_message: "I could not determine the best routing. Please rephrase your request.",
      reasoning: "LLM routing returned an unparsable response",
    };
  }
  return parsed;
}

function buildFallbackWorkflowPlanFromRouting(
  userText: string,
  decision: LlmRoutingDecision,
): WorkflowPlan {
  const directMessage =
    normalizeOptionalString(decision.direct_message) ||
    "I could not determine the best routing. Please rephrase your request.";

  if (decision.action === "DirectAnswer") {
    return {
      goal: userText,
      nodes: [
        {
          id: "n1",
          kind: "direct_answer",
          depends_on: [],
          prompt_template: directMessage,
          rationale: decision.reasoning,
        },
      ],
      final_node_id: "n1",
    };
  }

  if (decision.action === "Clarify") {
    return {
      goal: userText,
      nodes: [
        {
          id: "n1",
          kind: "clarify",
          depends_on: [],
          prompt_template:
            directMessage ||
            "I need more information to route your request. Could you clarify which service or data source you'd like me to query?",
          rationale: decision.reasoning,
        },
      ],
      final_node_id: "n1",
    };
  }

  if (decision.targets.length === 0) {
    return {
      goal: userText,
      nodes: [
        {
          id: "n1",
          kind: "direct_answer",
          depends_on: [],
          prompt_template:
            "No specialist agents matched your request. Try rephrasing or ask what agents are available.",
          rationale: decision.reasoning,
        },
      ],
      final_node_id: "n1",
    };
  }

  const callNodes: WorkflowNode[] = decision.targets.map((target, index) => ({
    id: `n${index + 1}`,
    kind: "call_agent",
    depends_on: [],
    target: {
      agent_package: target.agent_package,
      agent_instance_id: target.agent_instance_id,
    },
    prompt_template: target.prompt,
    rationale: target.rationale,
  }));

  if (callNodes.length === 1) {
    return {
      goal: userText,
      nodes: callNodes,
      final_node_id: callNodes[0].id,
    };
  }

  const synthNodeId = `n${callNodes.length + 1}`;
  return {
    goal: userText,
    nodes: [
      ...callNodes,
      {
        id: synthNodeId,
        kind: "synthesize",
        depends_on: callNodes.map((node) => node.id),
        rationale: "Merge parallel specialist outputs into one final response.",
      },
    ],
    final_node_id: synthNodeId,
  };
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
  } catch {
    const fallbackDecision = await llmRoute(userText, agents, conversationSummary);
    const fallbackPlan = buildFallbackWorkflowPlanFromRouting(userText, fallbackDecision);
    validateWorkflowPlan(fallbackPlan, agentRegistry);
    return fallbackPlan;
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

type LlmTargetEvidence = {
  key: string;
  snippets: string[];
};

function routeTargetKey(target: RouteTarget): string {
  return `${target.agent_package}/${target.agent_instance_id}`;
}

async function collectLlmTargetEvidence(
  delegationPrompt: string,
  target: RouteTarget,
): Promise<LlmTargetEvidence> {
  const snippets = await collectEvidence(delegationPrompt, target);
  return {
    key: routeTargetKey(target),
    snippets,
  };
}

// ---------------------------------------------------------------------------
// Workflow planning (Phase 2)
// ---------------------------------------------------------------------------

function isPhase2ExecutableWorkflowPlan(plan: WorkflowPlan): boolean {
  for (const node of plan.nodes) {
    if (node.kind === "foreach") return false;
    if (
      (node.kind === "call_agent" ||
        node.kind === "clarify" ||
        node.kind === "direct_answer") &&
      node.depends_on.length > 0
    ) {
      return false;
    }
  }
  return true;
}

async function executePhase2WorkflowPlan(
  plan: WorkflowPlan,
  userText: string,
  conversationSummary: string | null,
): Promise<SessionResult> {
  const clarifyNode =
    plan.nodes.find((node) => node.kind === "clarify" && node.depends_on.length === 0) || null;
  if (clarifyNode?.prompt_template) {
    return { message: clarifyNode.prompt_template };
  }

  const directNodes = plan.nodes.filter(
    (node) => node.kind === "direct_answer" && node.depends_on.length === 0,
  );
  const callNodes = plan.nodes.filter(
    (node): node is WorkflowNode & { kind: "call_agent"; target: RouteTarget; prompt_template: string } =>
      node.kind === "call_agent" &&
      node.depends_on.length === 0 &&
      !!node.target &&
      typeof node.prompt_template === "string",
  );

  if (directNodes.length > 0 && callNodes.length === 0) {
    const message = directNodes
      .map((node) => node.prompt_template)
      .filter((entry): entry is string => typeof entry === "string" && entry.trim().length > 0)
      .join("\n");
    return { message: message || "No direct response was produced by the workflow plan." };
  }

  if (callNodes.length === 0) {
    return { message: "Workflow plan contains no executable call_agent nodes for this phase." };
  }

  const settled = await Promise.allSettled(
    callNodes.map(async (node) => ({
      nodeId: node.id,
      snippets: await collectEvidence(node.prompt_template, node.target),
    })),
  );

  const evidenceParts: string[] = [];
  for (const outcome of settled) {
    if (outcome.status === "rejected") {
      const reason =
        outcome.reason instanceof Error ? outcome.reason.message : String(outcome.reason);
      evidenceParts.push(`[unknown]\nDelegation error: ${reason}`);
      continue;
    }
    const transcript = outcome.value.snippets.join("\n\n---\n\n").slice(0, MAX_TRANSCRIPT_CHARS);
    const joined = normalizeText(transcript);
    if (!joined) continue;
    evidenceParts.push(`[${outcome.value.nodeId}]\n${joined}`);
  }

  if (evidenceParts.length === 0) {
    return {
      message: "I delegated according to the workflow plan, but received no usable evidence.",
    };
  }

  const transcript = evidenceParts.join("\n\n---\n\n").slice(0, MAX_TRANSCRIPT_CHARS);
  return { message: await synthesize(userText, transcript, conversationSummary) };
}

async function runWorkflowCoordinatorPhase2(ctx: RunContext): Promise<SessionResult> {
  const userText = (ctx.text || "").trim();
  if (!userText) {
    return { message: "Please share what you want me to coordinate." };
  }

  const conversationSummary = getConversationSummary(ctx);

  let agents: DiscoveredAgent[];
  try {
    agents = await discoverAgents(userText);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    return { message: `Agent discovery failed: ${reason}` };
  }

  let plan: WorkflowPlan;
  try {
    plan = await planWorkflow(userText, agents, conversationSummary);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    return { message: `Workflow planning failed: ${reason}` };
  }

  if (!isPhase2ExecutableWorkflowPlan(plan)) {
    // Phase 3 will handle dependency-aware and foreach execution.
    return runLlmCoordinator(ctx);
  }

  return executePhase2WorkflowPlan(plan, userText, conversationSummary);
}

// ---------------------------------------------------------------------------
// LLM-routed coordinator (rollback/fallback path)
// ---------------------------------------------------------------------------

async function runLlmCoordinator(ctx: RunContext): Promise<SessionResult> {
  const userText = (ctx.text || "").trim();
  if (!userText) {
    return { message: "Please share what you want me to coordinate." };
  }

  const conversationSummary = getConversationSummary(ctx);

  let agents: DiscoveredAgent[];
  try {
    agents = await discoverAgents(userText);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    return { message: `Agent discovery failed: ${reason}` };
  }

  let decision: LlmRoutingDecision;
  try {
    decision = await llmRoute(userText, agents, conversationSummary);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    return { message: `Routing failed: ${reason}` };
  }

  if (decision.action === "DirectAnswer") {
    return {
      message: decision.direct_message || "I can help coordinate requests to specialist agents. What would you like to know?",
    };
  }

  if (decision.action === "Clarify") {
    return {
      message: decision.direct_message || "I need more information to route your request. Could you clarify which service or data source you'd like me to query?",
    };
  }

  if (decision.targets.length === 0) {
    return {
      message: "No specialist agents matched your request. Try rephrasing or ask what agents are available.",
    };
  }

  if (decision.action === "FanOut" && decision.targets.length > 1) {
    const cappedTargets = decision.targets.slice(0, MAX_FANOUT_CONCURRENCY);
    const settled = await Promise.allSettled(
      cappedTargets.map(async (t) => {
        const target: RouteTarget = {
          agent_package: t.agent_package,
          agent_instance_id: t.agent_instance_id,
        };
        return collectLlmTargetEvidence(t.prompt, target);
      }),
    );
    const evidenceParts: string[] = [];

    for (let i = 0; i < settled.length; i++) {
      const outcome = settled[i];
      const target = cappedTargets[i];
      const label = `${target.agent_package}/${target.agent_instance_id}`;

      if (outcome.status === "rejected") {
        const reason =
          outcome.reason instanceof Error ? outcome.reason.message : String(outcome.reason);
        evidenceParts.push(`[${label}]\nDelegation error: ${reason}`);
        continue;
      }

      const transcript = outcome.value.snippets.join("\n\n---\n\n").slice(0, MAX_TRANSCRIPT_CHARS);
      const joined = normalizeText(transcript);
      if (!joined) continue;
      evidenceParts.push(`[${outcome.value.key}]\n${joined}`);
    }

    if (evidenceParts.length === 0) {
      return {
        message: "I delegated to multiple specialists but received no usable evidence. Try a more specific query.",
      };
    }

    const transcript = evidenceParts.join("\n\n---\n\n").slice(0, MAX_TRANSCRIPT_CHARS);
    return { message: await synthesize(userText, transcript, conversationSummary) };
  }

  // Single delegation (Delegate action or FanOut with one target)
  const primary = decision.targets[0];
  const target: RouteTarget = {
    agent_package: primary.agent_package,
    agent_instance_id: primary.agent_instance_id,
  };

  const collected = await collectLlmTargetEvidence(primary.prompt, target);

  if (collected.snippets.length === 0) {
    return {
      message: "I could not collect evidence from the delegated agent. Try a more specific query.",
    };
  }

  const transcript = collected.snippets.join("\n\n---\n\n").slice(0, MAX_TRANSCRIPT_CHARS);
  return { message: await synthesize(userText, transcript, conversationSummary) };
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

async function runCoordinator(ctx: RunContext): Promise<SessionResult> {
  if (USE_WORKFLOW_COORDINATOR) {
    return runWorkflowCoordinatorPhase2(ctx);
  }
  return runLlmCoordinator(ctx);
}

__chat_register({ run: runCoordinator });

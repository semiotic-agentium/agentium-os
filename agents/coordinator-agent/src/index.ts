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

type TaskDaemonCoordinatorHandoff = {
  schema_version: string;
  batch: Record<string, unknown>;
};

type PlannerUserTextFromHandoff = {
  userText: string;
  structuredHandoff: boolean;
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

declare function NormalizeIterableOutput(args: {
  user_message: string;
  producer_output_text?: string | null;
  producer_output_data_json?: string | null;
  consumer_action_hint?: string | null;
  required_item_fields: string[];
  max_items?: number | null;
}): Promise<unknown>;

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

const MAX_FANOUT_CONCURRENCY = 3;
const MAX_TRANSCRIPT_CHARS = 12_000;
const MAX_NODE_TRANSCRIPT_TEXT_CHARS = 1_200;
const MAX_NODE_TRANSCRIPT_DATA_CHARS = 900;
const MAX_NODE_TRANSCRIPT_URLS = 24;
const MAX_CONVERSATION_CONTEXT_CHARS = 4_000;
const MAX_SINGLE_SEND_CONTINUE_STEPS = 16;
const MAX_DELEGATION_CONTINUE_STEPS = 128;
const MAX_WORKFLOW_NODES = 30;
const MAX_FOREACH_EXPANSIONS = 50;
const MAX_WORKFLOW_ITERATIONS = 8;
const MAX_HANDOFF_LIST_ITEMS = 12;
const MAX_HANDOFF_FIELD_CHARS = 700;
const MAX_HANDOFF_PROMPT_CHARS = 12_000;
const MAX_NORMALIZATION_INPUT_CHARS = 16_000;

const INTERNAL_A2A_TOOL_NAME = "system/internal_a2a";
const DISCOVER_AGENTS_TOOL_NAME = "system/discover_agents";
const TASK_DAEMON_COORDINATOR_HANDOFF_SCHEMA_VERSION = "task-daemon.coordinator-handoff.v1";

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

function isMeaningfulDelegatedText(text: string): boolean {
  const normalized = text.trim().toLowerCase();
  if (normalized.length === 0) return false;
  if (normalized === "null" || normalized === "undefined" || normalized === "[object object]") {
    return false;
  }
  return true;
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

function parseObjectArray(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is Record<string, unknown> => isObject(entry));
}

function parseOptionalFiniteNumber(value: unknown): number | null {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(parsed)) return null;
  return parsed;
}

function parseOptionalBoolean(value: unknown): boolean | null {
  if (typeof value === "boolean") return value;
  return null;
}

function parseObjectField(
  object: Record<string, unknown>,
  key: string,
): Record<string, unknown> | null {
  const value = object[key];
  if (!isObject(value)) return null;
  return value;
}

function truncateForPrompt(text: string, maxChars: number = MAX_HANDOFF_FIELD_CHARS): string {
  if (text.length <= maxChars) return text;
  return `${text.slice(0, Math.max(0, maxChars - 3))}...`;
}

function appendNumberedSection(lines: string[], title: string, entries: string[]): void {
  if (entries.length === 0) return;
  lines.push(`${title}:`);
  const shown = entries.slice(0, MAX_HANDOFF_LIST_ITEMS);
  shown.forEach((entry, index) => {
    lines.push(`${index + 1}. ${truncateForPrompt(entry)}`);
  });
  if (entries.length > shown.length) {
    lines.push(`... ${entries.length - shown.length} more`);
  }
  lines.push("");
}

function formatSourceRef(source: Record<string, unknown>): string | null {
  const permalink = normalizeOptionalString(source.permalink);
  if (permalink) return permalink;
  return normalizeOptionalString(source.reference);
}

function formatTaskSources(task: Record<string, unknown>): string {
  const sourceRefs = parseObjectArray(task.sources)
    .map((source) => formatSourceRef(source))
    .filter((value): value is string => value != null)
    .slice(0, 2);
  if (sourceRefs.length === 0) return "";
  return ` | sources: ${sourceRefs.join(", ")}`;
}

function isLikelyTaskDaemonPrompt(text: string): boolean {
  const normalized = text.toLowerCase();
  return (
    normalized.includes("based on a slack discussion in")
    && normalized.includes("tasks to create")
  );
}

function parseTaskDaemonCoordinatorHandoff(
  message: ChatMessage | null | undefined,
): TaskDaemonCoordinatorHandoff | null {
  if (!message || !Array.isArray(message.parts)) return null;

  for (const part of message.parts) {
    if (!isObject(part)) continue;
    const data = part.data;
    if (!isObject(data)) continue;

    const schemaVersion = normalizeOptionalString(data.schema_version);
    if (schemaVersion !== TASK_DAEMON_COORDINATOR_HANDOFF_SCHEMA_VERSION) continue;

    const batch = data.batch;
    if (!isObject(batch)) continue;
    return {
      schema_version: schemaVersion,
      batch,
    };
  }

  return null;
}

function renderHandoffPrompt(
  handoff: TaskDaemonCoordinatorHandoff,
  fallbackText: string,
): string {
  const batch = handoff.batch;
  const project = parseObjectField(batch, "project");
  const interpretation = parseObjectField(batch, "interpretation");
  const workflowSeed =
    interpretation != null ? parseObjectField(interpretation, "workflow_seed") : null;

  const projectKey = project != null ? normalizeOptionalString(project.project_key) : null;
  const repoAvailable = project != null ? parseOptionalBoolean(project.repo_available) : null;
  const repoPath = project != null ? normalizeOptionalString(project.repo_path) : null;
  const sourceLabel = normalizeOptionalString(batch.source_label);
  const messagesScanned = parseOptionalFiniteNumber(batch.messages_scanned);
  const summary =
    interpretation != null ? normalizeOptionalString(interpretation.executive_summary) : null;

  const objectives =
    interpretation != null
      ? parseStringArray(interpretation.current_objectives)
          .map((entry) => normalizeText(entry))
          .filter((entry) => entry.length > 0)
      : [];

  const decisions =
    interpretation != null
      ? parseObjectArray(interpretation.decisions_made)
          .map((decision) => {
            const decisionText = normalizeOptionalString(decision.decision);
            if (!decisionText) return null;
            const rationale = normalizeOptionalString(decision.rationale);
            const confidence = normalizeOptionalString(decision.confidence);
            const parts = [decisionText];
            if (rationale) parts.push(`rationale: ${rationale}`);
            if (confidence) parts.push(`confidence: ${confidence}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const openQuestions =
    interpretation != null
      ? parseObjectArray(interpretation.open_questions)
          .map((question) => {
            const text = normalizeOptionalString(question.question);
            if (!text) return null;
            const blocking = parseOptionalBoolean(question.blocking);
            const owner = normalizeOptionalString(question.suggested_owner);
            const parts = [text];
            if (blocking === true) parts.push("blocking");
            if (owner) parts.push(`owner: ${owner}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const risks =
    interpretation != null
      ? parseObjectArray(interpretation.risks)
          .map((risk) => {
            const riskText = normalizeOptionalString(risk.risk);
            if (!riskText) return null;
            const impact = normalizeOptionalString(risk.impact);
            const mitigation = normalizeOptionalString(risk.mitigation);
            const confidence = normalizeOptionalString(risk.confidence);
            const parts = [riskText];
            if (impact) parts.push(`impact: ${impact}`);
            if (mitigation) parts.push(`mitigation: ${mitigation}`);
            if (confidence) parts.push(`confidence: ${confidence}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const followUps =
    interpretation != null
      ? parseObjectArray(interpretation.follow_ups)
          .map((followUp) => {
            const prompt = normalizeOptionalString(followUp.prompt);
            if (!prompt) return null;
            const kind = normalizeOptionalString(followUp.kind);
            const urgency = normalizeOptionalString(followUp.urgency);
            const parts = [prompt];
            if (kind) parts.push(`kind: ${kind}`);
            if (urgency) parts.push(`urgency: ${urgency}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const workflowGoal = workflowSeed != null ? normalizeOptionalString(workflowSeed.goal) : null;

  const investigationNodes =
    workflowSeed != null
      ? parseObjectArray(workflowSeed.investigation_nodes)
          .map((node) => {
            const title = normalizeOptionalString(node.title);
            const key = normalizeOptionalString(node.key);
            const prompt = normalizeOptionalString(node.prompt);
            const goal = normalizeOptionalString(node.goal);
            const runCondition = normalizeOptionalString(node.when_to_run);
            const dependencies = parseStringArray(node.depends_on);
            const parts = [title || key];
            if (goal) parts.push(`goal: ${goal}`);
            if (runCondition) parts.push(`when: ${runCondition}`);
            if (dependencies.length > 0) parts.push(`depends_on: ${dependencies.join(", ")}`);
            if (prompt) parts.push(`prompt: ${prompt}`);
            return parts.filter((entry): entry is string => entry != null).join(" | ");
          })
          .filter((entry) => entry.length > 0)
      : [];

  const clarificationNodes =
    workflowSeed != null
      ? parseObjectArray(workflowSeed.clarification_nodes)
          .map((node) => {
            const question = normalizeOptionalString(node.question);
            if (!question) return null;
            const key = normalizeOptionalString(node.key);
            const owner = normalizeOptionalString(node.suggested_owner);
            const blocking = parseOptionalBoolean(node.blocking);
            const dependencies = parseStringArray(node.depends_on);
            const parts = [question];
            if (key) parts.push(`key: ${key}`);
            if (blocking === true) parts.push("blocking");
            if (owner) parts.push(`owner: ${owner}`);
            if (dependencies.length > 0) parts.push(`depends_on: ${dependencies.join(", ")}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const workflowFollowUps =
    workflowSeed != null
      ? parseObjectArray(workflowSeed.follow_up_nodes)
          .map((node) => {
            const prompt = normalizeOptionalString(node.prompt);
            if (!prompt) return null;
            const kind = normalizeOptionalString(node.kind);
            const urgency = normalizeOptionalString(node.urgency);
            const parts = [prompt];
            if (kind) parts.push(`kind: ${kind}`);
            if (urgency) parts.push(`urgency: ${urgency}`);
            return parts.join(" | ");
          })
          .filter((entry): entry is string => entry != null)
      : [];

  const derivedTasks = parseObjectArray(batch.derived_tasks)
    .map((task) => {
      const title = normalizeOptionalString(task.title);
      if (!title) return null;
      const key = normalizeOptionalString(task.key);
      const description = normalizeOptionalString(task.description);
      const priority = normalizeOptionalString(task.priority);
      const parts = [title];
      if (priority) parts.push(`priority: ${priority}`);
      if (key) parts.push(`key: ${key}`);
      if (description) parts.push(`description: ${description}`);
      const rendered = parts.join(" | ");
      return `${rendered}${formatTaskSources(task)}`;
    })
    .filter((entry): entry is string => entry != null);

  const lines: string[] = [];
  lines.push("Structured task-daemon handoff:");
  lines.push("Use this interpretation as the canonical input for workflow planning.");
  lines.push(`Handoff schema: ${handoff.schema_version}`);
  lines.push(`Project: ${projectKey || "unknown-project"}`);
  lines.push(`Source channel: ${sourceLabel || "unknown-source"}`);
  if (messagesScanned != null) {
    lines.push(`Messages scanned: ${Math.max(0, Math.floor(messagesScanned))}`);
  }
  if (repoAvailable != null) {
    lines.push(`Repository available: ${repoAvailable ? "yes" : "no"}`);
  }
  if (repoPath) {
    lines.push(`Repository path: ${repoPath}`);
  }
  lines.push("");

  if (summary) {
    lines.push(`Executive summary: ${truncateForPrompt(summary, 1_200)}`);
    lines.push("");
  }

  appendNumberedSection(lines, "Current objectives", objectives);
  appendNumberedSection(lines, "Decisions made", decisions);
  appendNumberedSection(lines, "Open questions", openQuestions);
  appendNumberedSection(lines, "Risks", risks);
  appendNumberedSection(lines, "Interpretation follow-ups", followUps);

  if (workflowGoal) {
    lines.push(`Workflow goal: ${truncateForPrompt(workflowGoal, 1_000)}`);
    lines.push("");
  }

  appendNumberedSection(lines, "Workflow investigation nodes", investigationNodes);
  appendNumberedSection(lines, "Workflow clarification nodes", clarificationNodes);
  appendNumberedSection(lines, "Workflow follow-up nodes", workflowFollowUps);
  appendNumberedSection(lines, "Derived tasks", derivedTasks);

  lines.push("Planning constraints:");
  lines.push("1. Prioritize workflow_seed and derived tasks over free-form phrasing.");
  lines.push("2. Treat interpretation as project-context understanding, not keyword matches.");
  if (repoAvailable === false) {
    lines.push("3. Repository is unavailable; favor clarification and follow-up workflows.");
  }

  const normalizedFallbackText = normalizeOptionalString(fallbackText);
  if (
    normalizedFallbackText
    && !isLikelyTaskDaemonPrompt(normalizedFallbackText)
  ) {
    lines.push("");
    lines.push("Additional operator message:");
    lines.push(truncateForPrompt(normalizedFallbackText, 1_600));
  }

  return lines.join("\n").slice(0, MAX_HANDOFF_PROMPT_CHARS);
}

function buildPlannerUserTextFromTaskDaemonHandoff(
  message: ChatMessage | null | undefined,
  fallbackUserText: string,
): PlannerUserTextFromHandoff {
  const handoff = parseTaskDaemonCoordinatorHandoff(message);
  if (!handoff) {
    return {
      userText: fallbackUserText,
      structuredHandoff: false,
    };
  }

  const rendered = renderHandoffPrompt(handoff, fallbackUserText);
  if (rendered.trim().length === 0) {
    return {
      userText: fallbackUserText,
      structuredHandoff: true,
    };
  }

  return {
    userText: rendered,
    structuredHandoff: true,
  };
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

function tryEvaluateSimpleArithmetic(text: string): string | null {
  const match = text.match(/(-?\d+(?:\.\d+)?)\s*([+\-*/])\s*(-?\d+(?:\.\d+)?)/);
  if (!match) return null;

  const left = Number(match[1]);
  const op = match[2];
  const right = Number(match[3]);
  if (!Number.isFinite(left) || !Number.isFinite(right)) return null;

  let result: number;
  if (op === "+") result = left + right;
  else if (op === "-") result = left - right;
  else if (op === "*") result = left * right;
  else {
    if (right === 0) return null;
    result = left / right;
  }

  if (!Number.isFinite(result)) return null;
  if (Math.abs(result - Math.round(result)) < 1e-9) {
    return String(Math.round(result));
  }

  return String(result);
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
    if (isMeaningfulDelegatedText(text)) out.add(text);
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
      if (isMeaningfulDelegatedText(text)) out.add(text);
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
        const before = out.size;
        if (chunk.message) pushMessageParts(chunk.message);
        // Avoid duplicate echoes: task/status payloads usually mirror chunk.message text.
        if (out.size === before) {
          for (const text of extractTextsFromSerializedChunkField(chunk.task)) out.add(text);
          for (const text of extractTextsFromSerializedChunkField(chunk.statusUpdate)) {
            out.add(text);
          }
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

function extractTextFromMessagePayload(value: unknown): string | null {
  if (!isObject(value) || !Array.isArray(value.parts)) return null;
  const texts: string[] = [];
  for (const part of value.parts) {
    if (!isObject(part) || typeof part.text !== "string") continue;
    const text = part.text.trim();
    if (!isMeaningfulDelegatedText(text)) continue;
    texts.push(text);
  }
  if (texts.length === 0) return null;
  return normalizeText(texts.join(" "));
}

function detectDelegatedTaskFailure(value: unknown): string | null {
  const queue: unknown[] = [value];
  const visited = new WeakSet<object>();

  while (queue.length > 0) {
    const current = queue.shift();
    if (current === undefined || current === null) continue;

    if (typeof current === "string") {
      const parsed = tryParseJsonValue(current);
      if (parsed) {
        queue.push(parsed);
      }
      continue;
    }

    if (Array.isArray(current)) {
      if (visited.has(current)) continue;
      visited.add(current);
      for (const entry of current) queue.push(entry);
      continue;
    }

    if (!isObject(current)) continue;
    if (visited.has(current)) continue;
    visited.add(current);

    const state = normalizeOptionalString(current.state);
    if (state && state.toUpperCase() === "TASK_STATE_FAILED") {
      return (
        extractTextFromMessagePayload(current.message) ||
        "Delegated task reported TASK_STATE_FAILED."
      );
    }

    const statusCandidates = [current.status, current.status_update, current.statusUpdate];
    for (const status of statusCandidates) {
      if (!isObject(status)) continue;
      const statusState = normalizeOptionalString(status.state);
      if (!statusState || statusState.toUpperCase() !== "TASK_STATE_FAILED") continue;
      const nestedStatus = isObject(status.status) ? status.status : null;
      return (
        extractTextFromMessagePayload(status.message) ||
        extractTextFromMessagePayload(nestedStatus?.message) ||
        "Delegated task reported TASK_STATE_FAILED."
      );
    }

    for (const nested of Object.values(current)) queue.push(nested);
  }

  return null;
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
  const delegatedFailure = detectDelegatedTaskFailure(output);
  if (delegatedFailure) {
    return { status: "error", output, errorMessage: delegatedFailure };
  }

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
    const allOutputs = drained.steps.map((step) => step.output);
    const terminal = drained.steps[drained.steps.length - 1] || null;
    const isTerminalError = terminal?.status === "error";

    if (drained.hitStepLimit || isTerminalError) {
      // Attempt to salvage useful output from steps collected before the
      // terminal failure.  If any meaningful text was streamed by the child
      // agent, return it rather than discarding everything.
      const partial = collectDelegatedTexts(allOutputs);
      if (partial.length > 0) {
        try {
          await sessionHandle.finish();
        } catch {
          try {
            await sessionHandle.abort("Partial output salvaged after terminal error");
          } catch {
            // Ignore cleanup errors.
          }
        }
        sessionHandle = null;
        return partial;
      }
      if (isTerminalError) {
        throw new Error(terminal.errorMessage || "Delegated session returned error status.");
      }
      throw new Error(
        `Delegated session did not reach terminal status within ${MAX_DELEGATION_CONTINUE_STEPS} continue steps.`,
      );
    }

    await sessionHandle.finish();
    sessionHandle = null;
    return collectDelegatedTexts(allOutputs);
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

type ForeachNormalizationResult = {
  items: unknown[];
  confidence: number;
  notes: string[];
};

function isTerminalArtifactStatus(status: NodeArtifactStatus): boolean {
  return status === "completed" || status === "failed" || status === "skipped";
}

function resolveArtifactPath(path: string, artifacts: Map<string, NodeArtifact>): unknown {
  const segments = path.split(".").filter((segment) => segment.length > 0);
  if (segments.length < 1) return undefined;

  const [nodeId, ...rest] = segments;
  let current: unknown = artifacts.get(nodeId);
  if (rest.length === 0) return current;
  for (const segment of rest) {
    if (!isObject(current)) return undefined;
    current = current[segment];
  }
  return current;
}

function tryParseJsonValue(text: string): unknown | null {
  const trimmed = text.trim();
  if (!trimmed) return null;

  try {
    return JSON.parse(trimmed);
  } catch {
    // Try extracting the first JSON object/array from surrounding text.
  }

  for (let start = 0; start < trimmed.length; start++) {
    const ch = trimmed[start];
    if (ch !== "{" && ch !== "[") continue;

    const stack: string[] = [ch === "{" ? "}" : "]"];
    let inString = false;
    let escaping = false;

    for (let i = start + 1; i < trimmed.length; i++) {
      const current = trimmed[i];

      if (inString) {
        if (escaping) {
          escaping = false;
          continue;
        }
        if (current === "\\") {
          escaping = true;
          continue;
        }
        if (current === '"') {
          inString = false;
        }
        continue;
      }

      if (current === '"') {
        inString = true;
        continue;
      }

      if (current === "{") stack.push("}");
      else if (current === "[") stack.push("]");
      else if (current === "}" || current === "]") {
        const expected = stack.pop();
        if (!expected || expected !== current) break;
        if (stack.length === 0) {
          const candidate = trimmed.slice(start, i + 1);
          try {
            return JSON.parse(candidate);
          } catch {
            break;
          }
        }
      }
    }
  }

  return null;
}

function findFirstArray(value: unknown, depth = 0): unknown[] | null {
  if (depth > 4) return null;
  if (Array.isArray(value)) return value;
  if (!isObject(value)) return null;

  const preferredKeys = ["items", "results", "data", "entries", "documents", "pages"];
  for (const key of preferredKeys) {
    const candidate = value[key];
    if (Array.isArray(candidate)) return candidate;
  }

  for (const nested of Object.values(value)) {
    const found = findFirstArray(nested, depth + 1);
    if (found) return found;
  }

  return null;
}

function resolveForeachItemsFromSource(
  sourcePath: string,
  artifacts: Map<string, NodeArtifact>,
): unknown[] | null {
  const direct = resolveArtifactPath(sourcePath, artifacts);
  const directArray = findFirstArray(direct);
  if (directArray) return directArray;

  if (typeof direct === "string") {
    const parsed = tryParseJsonValue(direct);
    const parsedArray = findFirstArray(parsed);
    if (parsedArray) return parsedArray;
  }

  const sourceSegments = sourcePath.split(".").filter((segment) => segment.length > 0);
  if (sourceSegments.length >= 1) {
    const artifact = artifacts.get(sourceSegments[0]);
    if (!artifact) return null;

    // Even when planner points to nX.output_text, prefer any structured artifact data first.
    const outputDataArray = findFirstArray(artifact.output_data);
    if (outputDataArray) return outputDataArray;

    if (typeof artifact.output_text === "string") {
      const parsed = tryParseJsonValue(artifact.output_text);
      const parsedArray = findFirstArray(parsed);
      if (parsedArray) return parsedArray;
    }
  }

  return null;
}

function stringifyForPrompt(value: unknown, maxChars = MAX_NORMALIZATION_INPUT_CHARS): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed.slice(0, maxChars) : null;
  }
  try {
    const json = JSON.stringify(value);
    if (!json) return null;
    return json.slice(0, maxChars);
  } catch {
    const text = String(value).trim();
    return text.length > 0 ? text.slice(0, maxChars) : null;
  }
}

function getSourceArtifactForPath(
  sourcePath: string,
  artifacts: Map<string, NodeArtifact>,
): NodeArtifact | null {
  const sourceSegments = sourcePath.split(".").filter((segment) => segment.length > 0);
  if (sourceSegments.length === 0) return null;
  return artifacts.get(sourceSegments[0]) || null;
}

function collectItemFieldHints(template: string): string[] {
  const fields = new Set<string>();
  const placeholderMatches = template.match(/\{\{([^}]+)\}\}/g) || [];

  for (const placeholder of placeholderMatches) {
    const body = placeholder.replace(/^\{\{/, "").replace(/\}\}$/, "").trim();
    const candidates = body.split("||").map((entry) => entry.trim());
    for (const candidate of candidates) {
      if (!/^item(?:\.[\w]+)*$/.test(candidate)) continue;
      if (candidate === "item") continue;
      fields.add(candidate.slice("item.".length));
    }
  }

  return Array.from(fields).slice(0, 20);
}

function buildConsumerActionHint(node: WorkflowNode): string | null {
  if (!node.foreach_template?.prompt_template) return null;
  const normalized = normalizeText(node.foreach_template.prompt_template);
  if (!normalized) return null;
  return normalized.slice(0, 500);
}

function parseNormalizedItemList(
  value: unknown,
  maxItems: number,
): ForeachNormalizationResult | null {
  if (!isObject(value)) return null;
  if (typeof value.items_json !== "string") return null;

  const parsed = tryParseJsonValue(value.items_json);
  let items: unknown[] | null = null;
  if (Array.isArray(parsed)) {
    items = parsed;
  } else {
    items = findFirstArray(parsed);
  }
  if (!items) return null;

  const confidenceRaw =
    typeof value.confidence === "number" ? value.confidence : Number(value.confidence);
  const confidence = Number.isFinite(confidenceRaw)
    ? Math.max(0, Math.min(1, confidenceRaw))
    : 0.0;

  const notes = Array.isArray(value.notes)
    ? value.notes.filter((entry): entry is string => typeof entry === "string")
    : [];

  return {
    items: items.slice(0, maxItems),
    confidence,
    notes,
  };
}

function toMatchableText(value: unknown): string {
  if (value === undefined || value === null) return "";
  if (typeof value === "string") return value.toLowerCase();
  if (typeof value === "number" || typeof value === "boolean") return String(value).toLowerCase();
  return "";
}

function flattenPrimitiveStrings(value: unknown, out: Set<string>, depth = 0): void {
  if (depth > 4 || value === undefined || value === null) return;
  if (typeof value === "string") {
    const t = value.trim();
    if (t.length > 0) out.add(t.toLowerCase());
    return;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    out.add(String(value).toLowerCase());
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) flattenPrimitiveStrings(item, out, depth + 1);
    return;
  }
  if (!isObject(value)) return;
  for (const nested of Object.values(value)) {
    flattenPrimitiveStrings(nested, out, depth + 1);
  }
}

function dedupeItems(items: unknown[]): unknown[] {
  const seen = new Set<string>();
  const deduped: unknown[] = [];

  for (const item of items) {
    let key = "";
    if (isObject(item)) {
      const id = toMatchableText(item.id);
      const url = toMatchableText(item.url);
      const title = toMatchableText(item.title);
      key = `${id}|${url}|${title}`;
    } else {
      key = toMatchableText(item);
    }
    if (!key) {
      let fallback = "";
      try {
        fallback = JSON.stringify(item) || "";
      } catch {
        fallback = String(item);
      }
      key = fallback;
    }
    if (seen.has(key)) continue;
    seen.add(key);
    deduped.push(item);
  }

  return deduped;
}

function filterItemsByEvidence(items: unknown[], evidenceCorpus: string): unknown[] {
  const corpus = evidenceCorpus.toLowerCase();
  if (!corpus.trim()) return dedupeItems(items);

  const grounded = items.filter((item) => {
    const candidates = new Set<string>();
    flattenPrimitiveStrings(item, candidates);

    // Require at least one non-trivial field to appear in evidence.
    for (const candidate of candidates) {
      if (candidate.length < 4) continue;
      if (corpus.includes(candidate)) return true;
    }
    return false;
  });

  return dedupeItems(grounded);
}

async function normalizeForeachItemsWithModel(
  userText: string,
  node: WorkflowNode,
  artifacts: Map<string, NodeArtifact>,
): Promise<ForeachNormalizationResult | null> {
  if (!node.foreach_from || !node.foreach_template) return null;

  const direct = resolveArtifactPath(node.foreach_from, artifacts);
  const sourceArtifact = getSourceArtifactForPath(node.foreach_from, artifacts);
  const producerOutputText =
    (typeof direct === "string" ? direct : null) ||
    sourceArtifact?.output_text ||
    stringifyForPrompt(direct);
  const producerOutputDataJson =
    stringifyForPrompt((isObject(direct) || Array.isArray(direct)) ? direct : undefined) ||
    stringifyForPrompt(sourceArtifact?.output_data);

  const requiredItemFields = collectItemFieldHints(node.foreach_template.prompt_template);
  const consumerActionHint = buildConsumerActionHint(node);
  const maxItems = Math.min(
    node.foreach_template.max_items ?? MAX_FOREACH_EXPANSIONS,
    MAX_FOREACH_EXPANSIONS,
  );

  try {
    const raw = await NormalizeIterableOutput({
      user_message: userText,
      producer_output_text: producerOutputText || null,
      producer_output_data_json: producerOutputDataJson || null,
      consumer_action_hint: consumerActionHint,
      required_item_fields: requiredItemFields,
      max_items: maxItems,
    });
    const parsed = parseNormalizedItemList(raw, maxItems);
    if (!parsed) return null;

    const evidenceCorpus = `${producerOutputText || ""}\n${producerOutputDataJson || ""}`;
    const filtered = filterItemsByEvidence(parsed.items, evidenceCorpus).slice(0, maxItems);
    if (filtered.length === 0) {
      return {
        items: [],
        confidence: Math.min(parsed.confidence, 0.5),
        notes: [...parsed.notes, "No normalized items were grounded in source evidence."],
      };
    }

    return {
      items: filtered,
      confidence: parsed.confidence,
      notes: parsed.notes,
    };
  } catch {
    return null;
  }
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

function resolveItemExpression(item: unknown, expression: string): unknown {
  const candidates = expression
    .split("||")
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);

  for (const candidate of candidates) {
    if (!/^item(?:\.[\w]+)*$/.test(candidate)) continue;
    const path = candidate === "item" ? "" : candidate.slice("item.".length);
    const resolved = resolvePathFromObject(item, path);
    if (resolved === undefined || resolved === null) continue;
    if (typeof resolved === "string" && resolved.trim().length === 0) continue;
    return resolved;
  }

  return undefined;
}

function interpolateItemTemplate(template: string, item: unknown): string {
  return template.replace(/\{\{\s*([^}]+)\s*\}\}/g, (match, exprRaw) => {
    const expr = exprRaw.trim();
    const resolved = resolveItemExpression(item, expr);
    if (resolved === undefined) return match;
    if (typeof resolved === "string") return resolved.slice(0, MAX_INTERPOLATION_CHARS);
    try {
      return JSON.stringify(resolved).slice(0, MAX_INTERPOLATION_CHARS);
    } catch {
      return String(resolved).slice(0, MAX_INTERPOLATION_CHARS);
    }
  });
}

function buildForeachChildPrompt(
  promptTemplate: string,
  item: unknown,
  itemIndex: number,
  totalItems: number,
): string {
  const rendered = interpolateItemTemplate(promptTemplate, item);
  const itemContext = stringifyForPrompt(item, 2_000) || "null";
  const guardrailBlock = [
    "Coordinator constraints for this foreach item:",
    `- Item index: ${itemIndex + 1} of ${totalItems}.`,
    "- Process only this single item and do not iterate over additional items.",
    "- Perform at most one primary action for this item, then stop.",
    "- If completion is not possible, return a failure instead of retrying the same write repeatedly.",
    "Item context JSON:",
    itemContext,
  ].join("\n");

  return `${rendered}\n\n${guardrailBlock}`;
}

function extractUrlsFromText(text: string, maxUrls = MAX_NODE_TRANSCRIPT_URLS): string[] {
  const matches = text.match(/https?:\/\/[^\s)>\]}]+/g) || [];
  if (matches.length === 0) return [];
  const deduped: string[] = [];
  const seen = new Set<string>();
  for (const raw of matches) {
    const cleaned = raw.replace(/[.,;:!?]+$/g, "");
    if (!cleaned || seen.has(cleaned)) continue;
    seen.add(cleaned);
    deduped.push(cleaned);
    if (deduped.length >= maxUrls) break;
  }
  return deduped;
}

async function expandForeachNodeInPlan(
  plan: WorkflowPlan,
  node: WorkflowNode,
  artifacts: Map<string, NodeArtifact>,
  userText: string,
): Promise<NodeArtifact> {
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

  let resolvedItems = resolveForeachItemsFromSource(node.foreach_from, artifacts);
  let normalizationMeta: {
    confidence: number;
    notes: string[];
  } | null = null;
  if (!resolvedItems) {
    const normalized = await normalizeForeachItemsWithModel(userText, node, artifacts);
    if (normalized) {
      resolvedItems = normalized.items;
      normalizationMeta = {
        confidence: normalized.confidence,
        notes: normalized.notes,
      };
    }
  }

  if (!resolvedItems) {
    return {
      node_id: node.id,
      status: "failed",
      error: `foreach source '${node.foreach_from}' did not resolve to an array-compatible value, and coordinator normalization could not recover iterable items.`,
      started_at,
      ended_at: new Date().toISOString(),
    };
  }

  const maxItems = Math.min(
    node.foreach_template.max_items ?? MAX_FOREACH_EXPANSIONS,
    MAX_FOREACH_EXPANSIONS,
  );
  const items = resolvedItems.slice(0, maxItems);
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
      prompt_template: buildForeachChildPrompt(
        node.foreach_template.prompt_template,
        items[i],
        i,
        items.length,
      ),
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
      normalization: normalizationMeta || undefined,
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

      const parsedOutputData = tryParseJsonValue(joined);

      return {
        node_id: node.id,
        status: "completed",
        output_text: joined,
        output_data: parsedOutputData ?? undefined,
        started_at,
        ended_at: new Date().toISOString(),
      };
    }

    if (node.kind === "direct_answer") {
      const rendered = interpolateTemplate(node.prompt_template || "", artifacts);
      const arithmeticAnswer = tryEvaluateSimpleArithmetic(rendered);
      return {
        node_id: node.id,
        status: "completed",
        output_text: arithmeticAnswer || rendered,
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
  const targetExecutionKey = (node: WorkflowNode): string => {
    if (node.kind === "call_agent" && node.target) {
      return `target:${workflowTargetKey(node.target)}`;
    }
    return `node:${node.id}`;
  };

  const buildBatches = (nodes: WorkflowNode[]): WorkflowNode[][] => {
    const pending = [...nodes];
    const batches: WorkflowNode[][] = [];

    while (pending.length > 0) {
      const usedKeys = new Set<string>();
      const batch: WorkflowNode[] = [];

      for (let i = 0; i < pending.length && batch.length < MAX_FANOUT_CONCURRENCY; ) {
        const node = pending[i];
        const key = targetExecutionKey(node);
        if (usedKeys.has(key)) {
          i += 1;
          continue;
        }
        usedKeys.add(key);
        batch.push(node);
        pending.splice(i, 1);
      }

      if (batch.length === 0) {
        batch.push(pending.shift()!);
      }
      batches.push(batch);
    }

    return batches;
  };

  const outputs: NodeArtifact[] = [];
  for (const batch of buildBatches(wave)) {
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
      const condensed = artifact.output_text.replace(/\s+/g, " ").trim();
      parts.push(`${prefix}\n${condensed.slice(0, MAX_NODE_TRANSCRIPT_TEXT_CHARS)}`);
      const urls = extractUrlsFromText(artifact.output_text);
      if (urls.length > 0) {
        parts.push(`${prefix} URLS: ${urls.join(" | ")}`);
      }
      const dataSnippet = stringifyForPrompt(artifact.output_data, MAX_NODE_TRANSCRIPT_DATA_CHARS);
      if (dataSnippet) {
        parts.push(`${prefix} DATA: ${dataSnippet}`);
      }
      continue;
    }
    if (artifact.status === "completed" && artifact.output_data !== undefined) {
      const dataSnippet = stringifyForPrompt(artifact.output_data, MAX_NODE_TRANSCRIPT_DATA_CHARS);
      if (dataSnippet) {
        parts.push(`${prefix} DATA: ${dataSnippet}`);
        continue;
      }
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
        const expansionArtifact = await expandForeachNodeInPlan(
          plan,
          node,
          artifacts,
          userText,
        );
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
  const rawUserText = (ctx.text || "").trim();
  const plannerUserText = buildPlannerUserTextFromTaskDaemonHandoff(ctx.message, rawUserText);
  const baseUserText = plannerUserText.userText;
  if (!baseUserText) {
    return { message: "Please share what you want me to coordinate." };
  }

  ctx.emit.statusChanged("TASK_STATE_WORKING");
  if (plannerUserText.structuredHandoff) {
    ctx.emit.message("Received structured task-daemon handoff. Planning from interpretation payload.");
  }
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
      ctx.emit.message("Planner returned an invalid workflow; using direct-answer fallback.");
      plan = {
        goal: "Fallback direct response",
        nodes: [
          {
            id: "fallback_direct_answer",
            kind: "direct_answer",
            depends_on: [],
            prompt_template: effectiveUserText,
            rationale: `Fallback after planner error: ${reason}`,
          },
        ],
        final_node_id: "fallback_direct_answer",
      };
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

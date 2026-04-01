/// <reference path="./baml-runtime.d.ts" />
import type {
  HostDispatchAck,
  HostDispatchRequest,
  ReplyPart,
  RunContext,
  SessionResult,
  StructuredReply,
} from "./baml-runtime";

function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

/** Plain text for dispatch ack / logging — handles legacy string or StructuredReply. */
function plainTextFromSessionMessage(
  message: string | StructuredReply | undefined,
): string | null {
  if (message == null) return null;
  if (typeof message === "string") {
    const t = message.trim();
    return t.length > 0 ? t : null;
  }
  const parts = message.parts;
  if (!Array.isArray(parts)) return null;
  const texts: string[] = [];
  for (const p of parts) {
    if (p != null && typeof p === "object" && p.type === "text" && typeof p.text === "string") {
      const t = p.text.trim();
      if (t.length > 0) texts.push(t);
    }
  }
  if (texts.length === 0) return null;
  return texts.join("\n");
}

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

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

type RouteTarget = {
  agent_package: string;
  agent_instance_id: string;
};

type DownstreamSelection =
  | { kind: "matched"; agent: DiscoveredAgent }
  | { kind: "none" }
  | { kind: "ambiguous"; candidates: string[] };

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

type DelegatedTaskStateMatch = {
  state: string;
  message: string | null;
};

type TaskDaemonSourceKind = "slack" | "clickup" | "github_issues";

type TaskDaemonSource = {
  source_key: string;
  source: TaskDaemonSourceKind;
  source_label: string;
};

type TaskDaemonProject = {
  project_key?: string;
  repo_available?: boolean;
  repo_path?: string | null;
};

type TaskDaemonInterpretation = {
  executive_summary?: string;
  current_objectives?: string[];
  workflow_seed?: unknown;
};

type TaskDaemonDerivedTask = {
  key?: string;
  title?: string;
  description?: string;
  priority?: string;
};

type SemanticIngressKind = "task_daemon_interpretation" | "slack_raw_source_records";

type SemanticIngressEvent = {
  ingress_kind: SemanticIngressKind;
  schema_version: string;
  source: TaskDaemonSource;
  project: TaskDaemonProject;
  messages_scanned?: number;
  interpretation: TaskDaemonInterpretation;
  derived_tasks: TaskDaemonDerivedTask[];
  transcript_lines: string[];
  no_work_reason?: string;
  raw_source_records?: SlackRawSourceRecord[];
  conversation_groups?: SlackConversationGroup[];
};

type SlackRawSourceRecord = {
  channel_id?: string;
  ts?: string;
  thread_ts?: string;
  user_id?: string;
  user?: string;
  user_name?: string;
  username?: string;
  bot_id?: string;
  text?: string;
  subtype?: string;
  source_ref?: string;
  permalink?: string;
};

type SlackConversationGroup = {
  conversationKey: string;
  records: SlackRawSourceRecord[];
};

type SlackConversationEvaluation = {
  conversationKey: string;
  records: SlackRawSourceRecord[];
  transcriptLines: string[];
  summary: string | null;
  actionable: boolean;
  usedThreadExpansion: boolean;
};

type DispatchSemanticIngress = {
  event: SemanticIngressEvent;
  expectedRoutingKey: string;
};

type IntakeDecisionKind =
  | "create_pm_work"
  | "execute_existing_work"
  | "cancel_or_close_work"
  | "noop";

type IntakeDecision = {
  kind: IntakeDecisionKind;
  reason: string;
  requiredCapabilities: string[];
};

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

// The manifest also declares `system/discover_tools` because the host registers
// the current SystemBundle as a unified allowlist. This agent only calls the
// agent-discovery and internal A2A tools directly.
const DISCOVER_AGENTS_TOOL_NAME = "system/discover_agents";
const INTERNAL_A2A_TOOL_NAME = "system/internal_a2a";
const SLACK_TOOL_NAME = "support/slack";
const TASK_DAEMON_INTERPRETATION_SCHEMA_VERSION = "task-daemon.interpretation.v1";
const RAW_SOURCE_SCHEMA_VERSION = "host.source-records.v1";
const RAW_SOURCE_ROUTING_KEY = "event:intake";
const MAX_SINGLE_SEND_CONTINUE_STEPS = 16;
const MAX_DELEGATION_CONTINUE_STEPS = 64;
const MAX_SUMMARY_CHARS = 1_200;
const MAX_SLACK_THREAD_REPLY_LIMIT = 50;
const MAX_SLACK_TRANSCRIPT_LINES_PER_CONVERSATION = 8;
const MAX_SLACK_TRANSCRIPT_LINES_PER_EVENT = 20;
const MAX_SLACK_CONVERSATION_EVALUATIONS_CONCURRENCY = 4;
const SLACK_ACTION_CUE_PATTERN =
  /\b(please|can you|could you|need(?:s)? to|action item|todo|follow up|follow-up|next step|track (?:this|it)|create (?:a )?(?:task|ticket)|open (?:an )?(?:issue|ticket)|assign|blocking|blocker|urgent|asap)\b/i;

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function normalizeOptionalString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function normalizeText(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function normalizeCapabilities(capabilities: string[]): string[] {
  return capabilities
    .map((capability) => capability.trim().toLowerCase())
    .filter((capability) => capability.length > 0);
}

function agentMatchesRequiredCapabilities(
  agent: DiscoveredAgent,
  requiredCapabilities: string[],
): boolean {
  const required = normalizeCapabilities(requiredCapabilities);
  const advertised = new Set(normalizeCapabilities(agent.capabilities || []));
  return required.every((capability) => advertised.has(capability));
}

function truncateText(value: string, maxChars: number): string {
  if (value.length <= maxChars) return value;
  return `${value.slice(0, Math.max(0, maxChars - 3)).trimEnd()}...`;
}

function parseObjectField(
  value: Record<string, unknown>,
  key: string,
): Record<string, unknown> | null {
  const candidate = value[key];
  return isObject(candidate) ? candidate : null;
}

function parseObjectArray(value: unknown): Record<string, unknown>[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is Record<string, unknown> => isObject(entry));
}

function parseStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === "string");
}

function parseTaskDaemonSourceKind(value: unknown): TaskDaemonSourceKind | null {
  if (value === "slack" || value === "clickup" || value === "github_issues") {
    return value;
  }
  return null;
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

function unwrapToolSessionNextOutput(value: unknown): unknown {
  if (!isObject(value)) return value;
  return "output" in value ? value.output : value;
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

function tryParseJsonValue(value: string): unknown | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return null;
  try {
    return JSON.parse(trimmed);
  } catch {
    return null;
  }
}

function extractTextFromMessagePayload(value: unknown): string | null {
  if (!isObject(value) || !Array.isArray(value.parts)) return null;
  const texts: string[] = [];
  for (const part of value.parts) {
    if (!isObject(part) || typeof part.text !== "string") continue;
    const text = part.text.trim();
    if (text.length === 0) continue;
    texts.push(text);
  }
  if (texts.length === 0) return null;
  return normalizeText(texts.join(" "));
}

function detectDelegatedTaskState(
  value: unknown,
  targetStates: readonly string[],
): DelegatedTaskStateMatch | null {
  const wanted = new Set(targetStates.map((state) => state.toUpperCase()));
  const queue: unknown[] = [value];
  const visited = new WeakSet<object>();

  while (queue.length > 0) {
    const current = queue.shift();
    if (current === undefined || current === null) continue;

    if (typeof current === "string") {
      const parsed = tryParseJsonValue(current);
      if (parsed !== null) queue.push(parsed);
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

    const directState = normalizeOptionalString(current.state);
    if (directState && wanted.has(directState.toUpperCase())) {
      return {
        state: directState.toUpperCase(),
        message: extractTextFromMessagePayload(current.message),
      };
    }

    const statusCandidates = [current.status, current.statusUpdate];
    for (const candidate of statusCandidates) {
      if (!isObject(candidate)) continue;
      const nestedStatus = isObject(candidate.status) ? candidate.status : null;
      for (const statusObject of [candidate, nestedStatus]) {
        if (!isObject(statusObject)) continue;
        const state = normalizeOptionalString(statusObject.state);
        if (!state || !wanted.has(state.toUpperCase())) continue;
        return {
          state: state.toUpperCase(),
          message:
            extractTextFromMessagePayload(candidate.message) ||
            extractTextFromMessagePayload(statusObject.message),
        };
      }
    }

    for (const nested of Object.values(current)) queue.push(nested);
  }

  return null;
}

function delegatedFailureMessage(outputs: unknown[]): string | null {
  const match = detectDelegatedTaskState(outputs, ["TASK_STATE_FAILED"]);
  return match?.message || (match ? "Delegated task reported TASK_STATE_FAILED." : null);
}

function delegatedSuspensionMessage(outputs: unknown[]): string | null {
  const match = detectDelegatedTaskState(outputs, [
    "TASK_STATE_INPUT_REQUIRED",
    "TASK_STATE_AUTH_REQUIRED",
  ]);
  if (!match) return null;
  if (match.message) return match.message;
  if (match.state === "TASK_STATE_AUTH_REQUIRED") {
    return "Delegated task requires authentication before it can continue.";
  }
  return "Delegated task requires additional input before it can continue.";
}

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
  const errorObject = isObject(step.error) ? step.error : null;
  const errorMessage =
    errorObject && typeof errorObject.message === "string"
      ? errorObject.message
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
  for (let step = 0; step < maxSteps; step += 1) {
    const parsed = parseToolSessionStep(await sessionHandle.continue());
    steps.push(parsed);
    // `done`, `error`, and `suspended` all end a single drain. Callers decide
    // whether to resume a suspended session in a later turn.
    if (parsed.status !== "streaming") {
      return { steps, hitStepLimit: false };
    }
  }
  return { steps, hitStepLimit: true };
}

function lastMeaningfulStepOutput(steps: ParsedToolSessionStep[]): unknown {
  for (let index = steps.length - 1; index >= 0; index -= 1) {
    const output = steps[index]?.output;
    if (output !== undefined && output !== null) return output;
  }
  return null;
}

async function runSingleSendSession(
  toolName: string,
  openInput: Record<string, unknown>,
  sendInput: Record<string, unknown>,
  maxSteps: number,
  abortReasonForOutputs?: (steps: ParsedToolSessionStep[]) => string | null,
): Promise<DrainToolSessionResult> {
  let sessionHandle: ToolSessionHandle | null = null;
  try {
    sessionHandle = await openToolSession(toolName, openInput);
    await sessionHandle.send(sendInput);
    const drained = await drainToolSession(sessionHandle, maxSteps);
    const terminal = drained.steps[drained.steps.length - 1] || null;
    const abortReason =
      (drained.hitStepLimit
        ? `${toolName} did not finish within ${maxSteps} continue steps.`
        : null) ||
      abortReasonForOutputs?.(drained.steps) ||
      (terminal?.status === "suspended"
        ? `${toolName} suspended and cannot be resumed from this workflow-intake turn.`
        : null) ||
      (terminal?.status === "error"
        ? terminal.errorMessage || `${toolName} returned error status.`
        : null) ||
      null;

    if (abortReason) {
      try {
        await sessionHandle.abort(abortReason);
      } catch {
        // Ignore cleanup failures while surfacing the root error to the caller.
      }
      sessionHandle = null;
      return drained;
    }

    await sessionHandle.finish();
    sessionHandle = null;
    return drained;
  } catch (error) {
    if (sessionHandle) {
      const reason = error instanceof Error ? error.message : String(error);
      try {
        await sessionHandle.abort(reason);
      } catch {
        // Ignore cleanup failures while surfacing the root error.
      }
    }
    throw error;
  }
}

function collectDelegatedTexts(outputs: unknown[]): string[] {
  const texts = new Set<string>();

  const visit = (candidate: unknown): void => {
    if (Array.isArray(candidate)) {
      for (const entry of candidate) visit(entry);
      return;
    }
    if (!isObject(candidate)) return;

    const message = candidate.message;
    if (isObject(message) && Array.isArray(message.parts)) {
      for (const part of message.parts) {
        if (!isObject(part) || typeof part.text !== "string") continue;
        const text = part.text.trim();
        if (text.length > 0) texts.add(text);
      }
    }

    const chunks = candidate.chunks;
    if (Array.isArray(chunks)) {
      for (const chunk of chunks) visit(chunk);
    }

    if ("output" in candidate) visit(candidate.output);
  };

  for (const output of outputs) {
    visit(output);
  }
  return Array.from(texts);
}

async function discoverAgentsByCapabilities(
  requiredCapabilities: string[],
): Promise<DiscoveredAgent[]> {
  const normalizedCapabilities = normalizeCapabilities(requiredCapabilities);
  const drained = await runSingleSendSession(
    DISCOVER_AGENTS_TOOL_NAME,
    { reason: "Find the downstream agent that can handle this semantic-ingress event." },
    { requiredCapabilities: normalizedCapabilities, limit: 100, offset: 0 },
    MAX_SINGLE_SEND_CONTINUE_STEPS,
  );

  if (drained.hitStepLimit) {
    throw new Error(
      `discover_agents did not finish within ${MAX_SINGLE_SEND_CONTINUE_STEPS} continue steps.`,
    );
  }

  const terminal = drained.steps[drained.steps.length - 1] || null;
  if (terminal?.status === "error") {
    throw new Error(terminal.errorMessage || "discover_agents returned error status.");
  }

  const parsed = parseDiscoverAgentsOutput(lastMeaningfulStepOutput(drained.steps));
  return (parsed?.agents ?? []).filter((agent) =>
    agentMatchesRequiredCapabilities(agent, normalizedCapabilities),
  );
}

async function delegateToAgent(target: RouteTarget, prompt: string): Promise<string[]> {
  const drained = await runSingleSendSession(
    INTERNAL_A2A_TOOL_NAME,
    { target },
    { parts: [{ text: prompt }] },
    MAX_DELEGATION_CONTINUE_STEPS,
    (steps) => {
      const outputs = steps.map((step) => step.output);
      return delegatedFailureMessage(outputs) || delegatedSuspensionMessage(outputs);
    },
  );

  if (drained.hitStepLimit) {
    throw new Error(
      `system/internal_a2a did not finish within ${MAX_DELEGATION_CONTINUE_STEPS} continue steps.`,
    );
  }

  const allOutputs = drained.steps.map((step) => step.output);
  const delegatedFailure = delegatedFailureMessage(allOutputs);
  if (delegatedFailure) {
    throw new Error(delegatedFailure);
  }

  const delegatedSuspension = delegatedSuspensionMessage(allOutputs);
  if (delegatedSuspension) {
    throw new Error(delegatedSuspension);
  }

  const terminal = drained.steps[drained.steps.length - 1] || null;
  if (terminal?.status === "error") {
    throw new Error(terminal.errorMessage || "system/internal_a2a returned error status.");
  }
  if (terminal?.status === "suspended") {
    throw new Error(
      "Delegated task requires follow-up input or authentication and cannot be completed from this workflow-intake turn.",
    );
  }

  return collectDelegatedTexts(allOutputs);
}

function parseInterpretationDerivedTasks(value: unknown): TaskDaemonDerivedTask[] {
  return parseObjectArray(value).map((task) => ({
    key: normalizeOptionalString(task.key) ?? undefined,
    title: normalizeOptionalString(task.title) ?? undefined,
    description: normalizeOptionalString(task.description) ?? undefined,
    priority: normalizeOptionalString(task.priority) ?? undefined,
  }));
}

function parseInterpretationEventValue(value: unknown): SemanticIngressEvent | null {
  if (!isObject(value)) return null;

  const schemaVersion = normalizeOptionalString(value.schema_version);
  if (schemaVersion !== TASK_DAEMON_INTERPRETATION_SCHEMA_VERSION) return null;

  const source = parseObjectField(value, "source");
  const project = parseObjectField(value, "project");
  const interpretation = parseObjectField(value, "interpretation");
  if (!source || !project || !interpretation) return null;

  const sourceKind = parseTaskDaemonSourceKind(source.source);
  const sourceKey = normalizeOptionalString(source.source_key);
  const sourceLabel = normalizeOptionalString(source.source_label);
  if (!sourceKind || !sourceKey || !sourceLabel) return null;

  return {
    ingress_kind: "task_daemon_interpretation",
    schema_version: schemaVersion,
    source: {
      source_key: sourceKey,
      source: sourceKind,
      source_label: sourceLabel,
    },
    project: {
      project_key: normalizeOptionalString(project.project_key) ?? undefined,
      repo_available:
        typeof project.repo_available === "boolean" ? project.repo_available : undefined,
      repo_path: normalizeOptionalString(project.repo_path),
    },
    messages_scanned:
      typeof value.messages_scanned === "number" ? value.messages_scanned : undefined,
    interpretation: {
      executive_summary:
        normalizeOptionalString(interpretation.executive_summary) ?? undefined,
      current_objectives: parseStringArray(interpretation.current_objectives),
      workflow_seed: interpretation.workflow_seed,
    },
    derived_tasks: parseInterpretationDerivedTasks(value.derived_tasks),
    transcript_lines: [],
    no_work_reason: undefined,
  };
}

function parseSlackRawSourceRecord(value: unknown): SlackRawSourceRecord | null {
  if (!isObject(value)) return null;
  return {
    channel_id: normalizeOptionalString(value.channel_id) ?? undefined,
    ts: normalizeOptionalString(value.ts) ?? undefined,
    thread_ts: normalizeOptionalString(value.thread_ts) ?? undefined,
    user_id: normalizeOptionalString(value.user_id) ?? undefined,
    user: normalizeOptionalString(value.user) ?? undefined,
    user_name: normalizeOptionalString(value.user_name) ?? undefined,
    username: normalizeOptionalString(value.username) ?? undefined,
    bot_id: normalizeOptionalString(value.bot_id) ?? undefined,
    text: normalizeOptionalString(value.text) ?? undefined,
    subtype: normalizeOptionalString(value.subtype) ?? undefined,
    source_ref: normalizeOptionalString(value.source_ref) ?? undefined,
    permalink: normalizeOptionalString(value.permalink) ?? undefined,
  };
}

function isSlackSystemSubtype(subtype: string | undefined): boolean {
  return (
    subtype === "channel_join" ||
    subtype === "channel_leave" ||
    subtype === "channel_topic" ||
    subtype === "channel_purpose" ||
    subtype === "channel_name" ||
    subtype === "channel_archive" ||
    subtype === "channel_unarchive"
  );
}

function isSlackConversationRecord(record: SlackRawSourceRecord): boolean {
  const text = normalizeOptionalString(record.text);
  if (!text) return false;
  return !isSlackSystemSubtype(record.subtype);
}

function slackConversationKey(record: SlackRawSourceRecord): string | null {
  return normalizeOptionalString(record.thread_ts) ?? normalizeOptionalString(record.ts);
}

function groupSlackConversationRecords(records: SlackRawSourceRecord[]): SlackConversationGroup[] {
  const groups = new Map<string, SlackRawSourceRecord[]>();
  for (const record of records) {
    const key = slackConversationKey(record);
    if (!key) continue;
    const existing = groups.get(key);
    if (existing) {
      existing.push(record);
      continue;
    }
    groups.set(key, [record]);
  }
  return Array.from(groups.entries()).map(([conversationKey, groupedRecords]) => ({
    conversationKey,
    records: groupedRecords,
  }));
}

function slackChannelIdFromSourceKey(sourceKey: string): string | null {
  const normalized = normalizeOptionalString(sourceKey);
  if (!normalized) return null;
  const separatorIndex = normalized.indexOf(":");
  if (separatorIndex < 0) return null;
  const sourceKind = normalized.slice(0, separatorIndex).toLowerCase();
  if (sourceKind !== "slack") return null;
  const channelId = normalized.slice(separatorIndex + 1).trim();
  return channelId.length > 0 ? channelId : null;
}

function preferredSlackChannelId(
  sourceKey: string,
  records: SlackRawSourceRecord[],
): string | null {
  const sourceKeyChannelId = slackChannelIdFromSourceKey(sourceKey);
  if (sourceKeyChannelId) return sourceKeyChannelId;
  for (const record of records) {
    const channelId = normalizeOptionalString(record.channel_id);
    if (channelId) return channelId;
  }
  return null;
}

function renderSlackTranscriptLines(
  records: SlackRawSourceRecord[],
  maxLines: number = MAX_SLACK_TRANSCRIPT_LINES_PER_CONVERSATION,
): string[] {
  const rendered: string[] = [];
  for (const record of records) {
    const text = normalizeOptionalString(record.text);
    if (!text) continue;
    const author =
      record.user_name ||
      record.username ||
      record.user ||
      record.user_id ||
      record.bot_id;
    const prefix = author ? `${author}: ` : "";
    rendered.push(truncateText(`${prefix}${normalizeText(text)}`, 320));
    if (rendered.length >= maxLines) break;
  }
  return rendered;
}

function summarizeSlackTranscript(transcriptLines: string[]): string | null {
  if (transcriptLines.length === 0) return null;
  return truncateText(transcriptLines.join(" | "), 600);
}

function slackTranscriptHasActionCue(transcriptLines: string[]): boolean {
  return transcriptLines.some((line) => SLACK_ACTION_CUE_PATTERN.test(line));
}

function inferSlackPriority(summary: string | null): string | undefined {
  const lowered = (summary || "").toLowerCase();
  if (/(urgent|asap|blocker|critical|sev[0-9]?|p0|p1)/.test(lowered)) {
    return "high";
  }
  if (/(todo|follow up|follow-up|action item|need to|please|next step)/.test(lowered)) {
    return "medium";
  }
  return transcriptHasMeaning(summary) ? "medium" : undefined;
}

function transcriptHasMeaning(summary: string | null): boolean {
  return typeof summary === "string" && summary.trim().length > 0;
}

function deriveSlackTaskTitle(sourceLabel: string, summary: string | null): string {
  if (!summary) return `Review Slack discussion from ${sourceLabel}`;
  return truncateText(`Review Slack discussion: ${summary}`, 96);
}

function parseSlackRawSourceEventValue(value: unknown): SemanticIngressEvent | null {
  if (!isObject(value)) return null;

  const schemaVersion = normalizeOptionalString(value.schema_version);
  if (schemaVersion !== RAW_SOURCE_SCHEMA_VERSION) return null;

  const source = parseObjectField(value, "source");
  if (!source) return null;

  const sourceKind = parseTaskDaemonSourceKind(source.source_kind);
  const sourceKey = normalizeOptionalString(source.source_key);
  const sourceLabel = normalizeOptionalString(source.source_label);
  if (sourceKind !== "slack" || !sourceKey || !sourceLabel) return null;

  const records = parseObjectArray(value.records)
    .map((record) => parseSlackRawSourceRecord(record))
    .filter((record): record is SlackRawSourceRecord => record != null);
  const readableRecords = records.filter((record) => isSlackConversationRecord(record));
  const conversationGroups = groupSlackConversationRecords(readableRecords);
  const noWorkReason =
    readableRecords.length === 0
      ? "The Slack raw batch contained no readable conversation text."
      : conversationGroups.length === 0
        ? "The Slack raw batch could not be grouped into conversation units."
        : undefined;

  return {
    ingress_kind: "slack_raw_source_records",
    schema_version: schemaVersion,
    source: {
      source_key: sourceKey,
      source: sourceKind,
      source_label: sourceLabel,
    },
    project: {},
    messages_scanned: Array.isArray(value.records) ? value.records.length : records.length,
    interpretation: {
      executive_summary:
        noWorkReason ||
        `Slack raw ingress captured ${readableRecords.length} readable message(s) across ${conversationGroups.length} conversation unit(s) from ${sourceLabel}.`,
      current_objectives: [],
    },
    derived_tasks: [],
    transcript_lines: [],
    no_work_reason: noWorkReason,
    raw_source_records: records,
    conversation_groups: conversationGroups,
  };
}

function shouldExpandSlackConversationGroup(group: SlackConversationGroup): boolean {
  return group.records.some((record) => normalizeOptionalString(record.thread_ts) != null);
}

function parseSlackToolOutputRecords(value: unknown): SlackRawSourceRecord[] {
  const normalized = unwrapToolSessionNextOutput(value);
  if (!isObject(normalized) || !Array.isArray(normalized.messages)) return [];

  return normalized.messages
    .map((message) => parseSlackRawSourceRecord(message))
    .filter((record): record is SlackRawSourceRecord => record != null)
    .filter((record) => isSlackConversationRecord(record));
}

async function fetchSlackConversationHistory(
  sourceKey: string,
  group: SlackConversationGroup,
): Promise<{ records: SlackRawSourceRecord[]; usedThreadExpansion: boolean }> {
  if (!shouldExpandSlackConversationGroup(group)) {
    return { records: group.records, usedThreadExpansion: false };
  }

  const channelId = preferredSlackChannelId(sourceKey, group.records);
  if (!channelId) {
    return { records: group.records, usedThreadExpansion: false };
  }

  try {
    const drained = await runSingleSendSession(
      SLACK_TOOL_NAME,
      {},
      {
        channel_id: channelId,
        thread_ts: group.conversationKey,
        inclusive: true,
        limit: MAX_SLACK_THREAD_REPLY_LIMIT,
        resolve_users: "none",
      },
      MAX_SINGLE_SEND_CONTINUE_STEPS,
    );

    if (drained.hitStepLimit) {
      return { records: group.records, usedThreadExpansion: false };
    }

    const terminal = drained.steps[drained.steps.length - 1] || null;
    if (terminal?.status === "error" || terminal?.status === "suspended") {
      return { records: group.records, usedThreadExpansion: false };
    }

    const records = parseSlackToolOutputRecords(lastMeaningfulStepOutput(drained.steps));
    if (records.length === 0) {
      return { records: group.records, usedThreadExpansion: false };
    }

    return { records, usedThreadExpansion: true };
  } catch (err) {
    if (typeof console !== "undefined" && typeof console.warn === "function") {
      console.warn(
        `[workflow-intake-agent] Slack thread expansion failed for ${sourceKey}/${group.conversationKey}: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
    return { records: group.records, usedThreadExpansion: false };
  }
}

async function evaluateSlackConversationGroup(
  sourceKey: string,
  group: SlackConversationGroup,
): Promise<SlackConversationEvaluation> {
  const fetched = await fetchSlackConversationHistory(sourceKey, group);
  const transcriptLines = renderSlackTranscriptLines(fetched.records);
  const summary = summarizeSlackTranscript(transcriptLines);
  const actionable = summary != null && slackTranscriptHasActionCue(transcriptLines);

  return {
    conversationKey: group.conversationKey,
    records: fetched.records,
    transcriptLines,
    summary,
    actionable,
    usedThreadExpansion: fetched.usedThreadExpansion,
  };
}

function renderSlackEventTranscript(evaluations: SlackConversationEvaluation[]): string[] {
  const lines: string[] = [];
  for (const [index, evaluation] of evaluations.entries()) {
    const label =
      evaluations.length > 1
        ? `Conversation ${index + 1} (${evaluation.conversationKey})`
        : null;
    for (const line of evaluation.transcriptLines) {
      lines.push(label ? `[${label}] ${line}` : line);
      if (lines.length >= MAX_SLACK_TRANSCRIPT_LINES_PER_EVENT) {
        return lines;
      }
    }
  }
  return lines;
}

function slackConversationNoWorkReason(
  sourceLabel: string,
  totalConversationCount: number,
  actionableCount: number,
): string | undefined {
  if (totalConversationCount === 0) {
    return "The Slack raw batch did not contain any conversation units.";
  }
  if (actionableCount === 0) {
    return `Slack semantic ingress reviewed ${totalConversationCount} conversation unit(s) from ${sourceLabel} and did not find a clear actionable request.`;
  }
  return undefined;
}

async function materializeSlackSemanticIngressEvent(
  event: SemanticIngressEvent,
): Promise<SemanticIngressEvent> {
  if (event.ingress_kind !== "slack_raw_source_records") {
    return event;
  }

  const conversationGroups = event.conversation_groups ?? [];
  if (conversationGroups.length === 0) {
    return event;
  }

  const evaluations: SlackConversationEvaluation[] = [];
  for (let i = 0; i < conversationGroups.length; i += MAX_SLACK_CONVERSATION_EVALUATIONS_CONCURRENCY) {
    const batch = conversationGroups.slice(
      i,
      i + MAX_SLACK_CONVERSATION_EVALUATIONS_CONCURRENCY,
    );
    const batchEvaluations = await Promise.all(
      batch.map((group) => evaluateSlackConversationGroup(event.source.source_key, group)),
    );
    evaluations.push(...batchEvaluations);
  }
  const actionable = evaluations.filter((evaluation) => evaluation.actionable);
  const derivedTasks = actionable.map((evaluation) => ({
    key: `slack-raw:${event.source.source_key}:${evaluation.conversationKey}`,
    title: deriveSlackTaskTitle(event.source.source_label, evaluation.summary),
    description: `Interpret the Slack discussion from ${event.source.source_label} and convert actionable requests into tracked PM work. Transcript: ${evaluation.summary}`,
    priority: inferSlackPriority(evaluation.summary),
  }));
  const transcriptLines = renderSlackEventTranscript(
    actionable.length > 0 ? actionable : evaluations.filter((evaluation) => evaluation.transcriptLines.length > 0),
  );
  const expandedConversationCount = evaluations.filter(
    (evaluation) => evaluation.usedThreadExpansion,
  ).length;
  const noWorkReason = slackConversationNoWorkReason(
    event.source.source_label,
    evaluations.length,
    actionable.length,
  );
  const executiveSummary =
    derivedTasks.length === 0
      ? noWorkReason ||
        `Slack semantic ingress reviewed ${evaluations.length} conversation unit(s) from ${event.source.source_label}.`
      : expandedConversationCount > 0
        ? `Slack semantic ingress derived ${derivedTasks.length} actionable work item(s) from ${evaluations.length} conversation unit(s) in ${event.source.source_label}, including ${expandedConversationCount} expanded thread(s).`
        : `Slack semantic ingress derived ${derivedTasks.length} actionable work item(s) from ${evaluations.length} conversation unit(s) in ${event.source.source_label}.`;

  return {
    ...event,
    interpretation: {
      ...event.interpretation,
      executive_summary: executiveSummary,
      current_objectives:
        derivedTasks.length === 0
          ? []
          : [
              "Preserve the actionable request details from each Slack conversation.",
              "Create tracked project-management follow-up for every actionable conversation.",
            ],
    },
    derived_tasks: derivedTasks,
    transcript_lines: transcriptLines,
    no_work_reason: derivedTasks.length === 0 ? noWorkReason : undefined,
  };
}

function extractSemanticIngressEvent(
  message: ChatMessage | null | undefined,
): SemanticIngressEvent | null {
  if (!message || !Array.isArray(message.parts)) return null;

  for (const part of message.parts) {
    if (!isObject(part)) continue;
    const event = parseInterpretationEventValue(part.data);
    if (event) return event;
    const rawEvent = parseSlackRawSourceEventValue(part.data);
    if (rawEvent) return rawEvent;
  }

  return null;
}

function extractTaskDaemonInterpretationEventFromDispatch(
  request: HostDispatchRequest | null | undefined,
): SemanticIngressEvent | null {
  if (!request || !Array.isArray(request.messages)) return null;
  for (const message of request.messages) {
    const event = parseInterpretationEventValue(message);
    if (event) return event;
  }
  return null;
}

function extractSlackRawSourceEventFromDispatch(
  request: HostDispatchRequest | null | undefined,
): SemanticIngressEvent | null {
  if (!request || !Array.isArray(request.messages)) return null;
  for (const message of request.messages) {
    const event = parseSlackRawSourceEventValue(message);
    if (event) return event;
  }
  return null;
}

function intakeRoutingKeyForSource(source: TaskDaemonSourceKind): string {
  if (source === "github_issues") return "github_issues:intake";
  return `${source}:intake`;
}

function classifyClickupTaskKind(task: TaskDaemonDerivedTask): IntakeDecisionKind {
  const key = task.key || "";
  const title = task.title || "";
  // This matches task-daemon's current ClickUp lifecycle task conventions.
  // When the event contract gains explicit lifecycle metadata, route on that
  // instead of inferring from human-readable keys and titles.
  if (key.startsWith("clickup-terminal:") || title.startsWith("Reconcile terminal ClickUp task:")) {
    return "cancel_or_close_work";
  }
  if (key.startsWith("clickup-removed:") || title.startsWith("Reconcile missing ClickUp task:")) {
    return "cancel_or_close_work";
  }
  return "execute_existing_work";
}

function deriveDecision(event: SemanticIngressEvent): IntakeDecision {
  if (event.derived_tasks.length === 0) {
    return {
      kind: "noop",
      reason: event.no_work_reason || "The event produced no derived work items.",
      requiredCapabilities: [],
    };
  }

  if (event.source.source === "slack") {
    return {
      kind: "create_pm_work",
      reason:
        event.ingress_kind === "slack_raw_source_records"
          ? "Slack raw ingress was interpreted into project-management follow-up work."
          : "Slack-origin interpretations should become project-management work items.",
      requiredCapabilities: ["clickup:create-task"],
    };
  }

  if (event.source.source === "clickup") {
    const kinds = new Set(event.derived_tasks.map((task) => classifyClickupTaskKind(task)));
    if (kinds.has("cancel_or_close_work")) {
      return {
        kind: "cancel_or_close_work",
        reason: "This ClickUp event indicates work that needs reconciliation or closure handling.",
        requiredCapabilities: ["coordination:routing"],
      };
    }
    return {
      kind: "execute_existing_work",
      reason: "This ClickUp event represents existing work that should be routed for execution.",
      requiredCapabilities: ["coordination:routing"],
    };
  }

  if (event.source.source === "github_issues") {
    return {
      kind: "execute_existing_work",
      reason:
        "GitHub issue events currently route through the generic execution path until a dedicated intake policy exists.",
      requiredCapabilities: ["coordination:routing"],
    };
  }

  return {
    kind: "execute_existing_work",
    reason: "Unknown sources fall back to the generic execution path.",
    requiredCapabilities: ["coordination:routing"],
  };
}

function preferredPackageTiebreakerForDecision(decision: IntakeDecision): string | null {
  if (decision.kind === "create_pm_work") return "clickup-agent";
  if (
    decision.kind === "execute_existing_work" ||
    decision.kind === "cancel_or_close_work"
  ) {
    return "coordinator-agent";
  }
  return null;
}

function compareDiscoveredAgents(left: DiscoveredAgent, right: DiscoveredAgent): number {
  const packageCompare = left.agent_package.localeCompare(right.agent_package);
  if (packageCompare !== 0) return packageCompare;
  const instanceCompare = left.agent_instance_id.localeCompare(right.agent_instance_id);
  if (instanceCompare !== 0) return instanceCompare;
  return left.name.localeCompare(right.name);
}

function formatAgentRoute(agent: DiscoveredAgent): string {
  return `${agent.agent_package}/${agent.agent_instance_id}`;
}

function selectDownstreamAgent(
  agents: DiscoveredAgent[],
  decision: IntakeDecision,
): DownstreamSelection {
  const eligible = agents
    // Prevent routing the event back into this intake agent. This guard is
    // coupled to the manifest package name.
    .filter((agent) => agent.agent_package !== "workflow-intake-agent")
    // Route only to default instances for now; multi-instance selection needs
    // explicit policy instead of silently picking an arbitrary deployment.
    .filter((agent) => agent.agent_instance_id === "default")
    .slice()
    .sort(compareDiscoveredAgents);

  if (eligible.length === 0) return { kind: "none" };

  // Capability matching decides eligibility; package names are only a stable
  // tiebreaker when multiple equivalent agents advertise the same capability.
  const preferredPackage = preferredPackageTiebreakerForDecision(decision);
  if (preferredPackage) {
    const preferred = eligible.find((agent) => agent.agent_package === preferredPackage);
    if (preferred) return { kind: "matched", agent: preferred };
  }

  if (eligible.length === 1) {
    return { kind: "matched", agent: eligible[0] };
  }

  // Fail closed when multiple downstream agents are equally eligible and no
  // explicit tiebreaker applies. Silent lexicographic routing would make
  // topology changes alter behavior without configuration changes.
  return {
    kind: "ambiguous",
    candidates: eligible.map(formatAgentRoute),
  };
}

function renderDerivedTasks(tasks: TaskDaemonDerivedTask[]): string[] {
  if (tasks.length === 0) {
    return ["- No derived tasks were attached to this event."];
  }

  return tasks.map((task, index) => {
    const title = task.title || task.key || `Task ${index + 1}`;
    const description = task.description ? ` | ${task.description}` : "";
    const priority = task.priority ? ` | priority: ${task.priority}` : "";
    const key = task.key ? ` | key: ${task.key}` : "";
    return `${index + 1}. ${title}${description}${priority}${key}`;
  });
}

function appendNumberedSection(lines: string[], title: string, entries: string[]): void {
  if (entries.length === 0) return;
  lines.push("");
  lines.push(`${title}:`);
  for (const [index, entry] of entries.entries()) {
    lines.push(`${index + 1}. ${entry}`);
  }
}

function renderWorkflowInvestigationNodes(seed: unknown): string[] {
  if (!isObject(seed)) return [];
  return parseObjectArray(seed.investigation_nodes)
    .map((node) => {
      const title = normalizeOptionalString(node.title);
      const key = normalizeOptionalString(node.key);
      const prompt = normalizeOptionalString(node.prompt);
      const goal = normalizeOptionalString(node.goal);
      const whenToRun = normalizeOptionalString(node.when_to_run);
      const dependsOn = parseStringArray(node.depends_on)
        .map((entry) => normalizeText(entry))
        .filter((entry) => entry.length > 0);

      const parts = [title || key];
      if (goal) parts.push(`goal: ${truncateText(goal, 240)}`);
      if (whenToRun) parts.push(`when: ${whenToRun}`);
      if (dependsOn.length > 0) parts.push(`depends_on: ${dependsOn.join(", ")}`);
      if (prompt) parts.push(`prompt: ${truncateText(prompt, 420)}`);
      return parts.filter((entry): entry is string => entry != null).join(" | ");
    })
    .filter((entry) => entry.length > 0);
}

function renderWorkflowClarificationNodes(seed: unknown): string[] {
  if (!isObject(seed)) return [];
  return parseObjectArray(seed.clarification_nodes)
    .map((node) => {
      const question = normalizeOptionalString(node.question);
      if (!question) return null;

      const key = normalizeOptionalString(node.key);
      const owner = normalizeOptionalString(node.suggested_owner);
      const blocking = typeof node.blocking === "boolean" ? node.blocking : false;
      const dependsOn = parseStringArray(node.depends_on)
        .map((entry) => normalizeText(entry))
        .filter((entry) => entry.length > 0);

      const parts = [truncateText(question, 420)];
      if (key) parts.push(`key: ${key}`);
      if (blocking) parts.push("blocking");
      if (owner) parts.push(`owner: ${owner}`);
      if (dependsOn.length > 0) parts.push(`depends_on: ${dependsOn.join(", ")}`);
      return parts.join(" | ");
    })
    .filter((entry): entry is string => entry != null);
}

function renderWorkflowFollowUpNodes(seed: unknown): string[] {
  if (!isObject(seed)) return [];
  return parseObjectArray(seed.follow_up_nodes)
    .map((node) => {
      const prompt = normalizeOptionalString(node.prompt);
      if (!prompt) return null;

      const kind = normalizeOptionalString(node.kind);
      const urgency = normalizeOptionalString(node.urgency);
      const parts = [truncateText(prompt, 420)];
      if (kind) parts.push(`kind: ${kind}`);
      if (urgency) parts.push(`urgency: ${urgency}`);
      return parts.join(" | ");
    })
    .filter((entry): entry is string => entry != null);
}

function describeIngressKind(ingressKind: SemanticIngressKind): string {
  if (ingressKind === "task_daemon_interpretation") {
    return "task-daemon interpretation";
  }
  return "Slack semantic ingress from raw source records";
}

function renderDecisionHeader(event: SemanticIngressEvent, decision: IntakeDecision): string {
  if (decision.kind === "create_pm_work") {
    if (event.ingress_kind === "task_daemon_interpretation") {
      return "Create project-management work items from this task-daemon event.";
    }
    return "Create project-management work items from this semantic-ingress event.";
  }
  if (decision.kind === "execute_existing_work") {
    if (event.ingress_kind === "task_daemon_interpretation") {
      return "Execute or route the existing work item described by this task-daemon event.";
    }
    return "Execute or route the existing work item described by this semantic-ingress event.";
  }
  if (decision.kind === "cancel_or_close_work") {
    return "Reconcile terminal or missing work and stop duplicate execution.";
  }
  return "No downstream action is required.";
}

function renderDownstreamPrompt(
  event: SemanticIngressEvent,
  decision: IntakeDecision,
): string {
  const lines: string[] = [];
  lines.push(renderDecisionHeader(event, decision));
  lines.push("");
  lines.push(`Ingress kind: ${describeIngressKind(event.ingress_kind)}`);
  lines.push(`Schema version: ${event.schema_version}`);
  lines.push(`Source: ${event.source.source} (${event.source.source_label})`);
  lines.push(`Source key: ${event.source.source_key}`);
  lines.push(`Project: ${event.project.project_key || "unscoped-project"}`);
  lines.push(`Messages scanned: ${event.messages_scanned ?? 0}`);
  lines.push(`Routing reason: ${decision.reason}`);

  if (typeof event.project.repo_available === "boolean") {
    lines.push(`Repo available: ${event.project.repo_available ? "yes" : "no"}`);
  }
  if (event.project.repo_path) {
    lines.push(`Repo path: ${event.project.repo_path}`);
  }

  const summary = event.interpretation.executive_summary;
  if (summary) {
    lines.push("");
    lines.push("Executive summary:");
    lines.push(truncateText(summary, MAX_SUMMARY_CHARS));
  }

  const objectives = event.interpretation.current_objectives || [];
  if (objectives.length > 0) {
    appendNumberedSection(
      lines,
      "Current objectives",
      objectives
        .map((objective) => normalizeText(objective))
        .filter((objective) => objective.length > 0),
    );
  }

  const workflowSeed = event.interpretation.workflow_seed;
  if (isObject(workflowSeed)) {
    const workflowGoal = normalizeOptionalString(workflowSeed.goal);
    if (workflowGoal) {
      lines.push("");
      lines.push("Workflow goal:");
      lines.push(truncateText(workflowGoal, MAX_SUMMARY_CHARS));
    }

    appendNumberedSection(
      lines,
      "Workflow investigation nodes",
      renderWorkflowInvestigationNodes(workflowSeed),
    );
    appendNumberedSection(
      lines,
      "Workflow clarification nodes",
      renderWorkflowClarificationNodes(workflowSeed),
    );
    appendNumberedSection(
      lines,
      "Workflow follow-up nodes",
      renderWorkflowFollowUpNodes(workflowSeed),
    );
  }

  if (event.transcript_lines.length > 0) {
    appendNumberedSection(lines, "Source transcript", event.transcript_lines);
  }

  lines.push("");
  lines.push(`Derived tasks (${event.derived_tasks.length} total):`);
  lines.push(...renderDerivedTasks(event.derived_tasks));
  return lines.join("\n");
}

function renderRouteSummary(
  decision: IntakeDecision,
  target: DiscoveredAgent,
  downstreamTexts: string[],
): string {
  const lines = [
    `Routed ${decision.kind} to ${target.agent_package}/${target.agent_instance_id}.`,
  ];
  if (downstreamTexts.length > 0) {
    lines.push("");
    lines.push("Downstream response:");
    for (const text of downstreamTexts.slice(0, 6)) {
      lines.push(`- ${truncateText(text, 600)}`);
    }
  }
  return lines.join("\n");
}

async function handleSemanticIngressEvent(event: SemanticIngressEvent): Promise<SessionResult> {
  try {
    const materializedEvent = await materializeSlackSemanticIngressEvent(event);
    const decision = deriveDecision(materializedEvent);
    if (decision.kind === "noop") {
      return { message: textReply(decision.reason) };
    }

    const agents = await discoverAgentsByCapabilities(decision.requiredCapabilities);
    const selection = selectDownstreamAgent(agents, decision);
    if (selection.kind === "none") {
      return {
        error: `No downstream agent matched required capabilities: ${decision.requiredCapabilities.join(", ")}`,
      };
    }
    if (selection.kind === "ambiguous") {
      return {
        error:
          `Multiple downstream agents matched required capabilities ` +
          `${decision.requiredCapabilities.join(", ")}: ${selection.candidates.join(", ")}`,
      };
    }

    const target = selection.agent;
    const prompt = renderDownstreamPrompt(materializedEvent, decision);
    const downstreamTexts = await delegateToAgent(target, prompt);
    return {
      message: textReply(renderRouteSummary(decision, target, downstreamTexts)),
    };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return { error: `workflow-intake-agent failed: ${reason}` };
  }
}

async function run(ctx: RunContext): Promise<SessionResult> {
  const event = extractSemanticIngressEvent(ctx.message);
  if (!event) {
    return {
      error:
        "workflow-intake-agent expects a task-daemon.interpretation.v1 or Slack host.source-records.v1 payload in message.parts[].data.",
    };
  }

  return handleSemanticIngressEvent(event);
}

function parseDispatchSemanticIngress(
  request: HostDispatchRequest,
): DispatchSemanticIngress | { error: string } {
  const messageType = normalizeOptionalString(request.message_type);
  if (messageType === TASK_DAEMON_INTERPRETATION_SCHEMA_VERSION) {
    const event = extractTaskDaemonInterpretationEventFromDispatch(request);
    if (!event) {
      return {
        error:
          "workflow-intake-agent expects task-daemon.interpretation.v1 payloads in dispatch.messages[].",
      };
    }
    return {
      event,
      expectedRoutingKey: intakeRoutingKeyForSource(event.source.source),
    };
  }

  if (messageType === RAW_SOURCE_SCHEMA_VERSION) {
    const event = extractSlackRawSourceEventFromDispatch(request);
    if (!event) {
      return {
        error:
          "workflow-intake-agent currently supports Slack host.source-records.v1 payloads in dispatch.messages[].",
      };
    }
    return {
      event,
      expectedRoutingKey: RAW_SOURCE_ROUTING_KEY,
    };
  }

  return {
    error:
      `workflow-intake-agent expected message_type ` +
      `${TASK_DAEMON_INTERPRETATION_SCHEMA_VERSION} or ${RAW_SOURCE_SCHEMA_VERSION}, ` +
      `got ${messageType ?? "missing"}.`,
  };
}

async function onDispatch(request: HostDispatchRequest): Promise<HostDispatchAck> {
  const parsed = parseDispatchSemanticIngress(request);
  if ("error" in parsed) {
    return {
      accepted: false,
      detail: parsed.error,
    };
  }

  const { event, expectedRoutingKey } = parsed;
  const routingKey = normalizeOptionalString(request.routing_key);
  if (routingKey !== expectedRoutingKey) {
    return {
      accepted: false,
      detail:
        `workflow-intake-agent expected routing_key ${expectedRoutingKey}, ` +
        `got ${routingKey ?? "missing"}.`,
    };
  }

  const result = await handleSemanticIngressEvent(event);
  if ("error" in result) {
    return {
      accepted: false,
      detail: result.error,
    };
  }

  return {
    accepted: true,
    detail:
      plainTextFromSessionMessage(result.message) ??
      `workflow-intake-agent accepted ${expectedRoutingKey}.`,
  };
}

const dispatchGlobal = globalThis as typeof globalThis & {
  onDispatch?: (request: HostDispatchRequest) => Promise<HostDispatchAck>;
};
dispatchGlobal.onDispatch = onDispatch;

__chat_register({ run });

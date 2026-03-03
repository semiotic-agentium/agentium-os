/// <reference path="./baml-runtime.d.ts" />

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

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

const MAX_FANOUT_CONCURRENCY = 3;
const MAX_TRANSCRIPT_CHARS = 12_000;
const MAX_CONVERSATION_CONTEXT_CHARS = 4_000;
const MAX_SINGLE_SEND_CONTINUE_STEPS = 16;
const MAX_DELEGATION_CONTINUE_STEPS = 128;

const INTERNAL_A2A_TOOL_NAME = "system/internal_a2a";
const DISCOVER_AGENTS_TOOL_NAME = "system/discover_agents";

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

function buildAgentCandidates(agents: DiscoveredAgent[]): Array<{
  agent_package: string;
  agent_instance_id: string;
  name: string;
  description: string | null;
  capabilities: string[];
  tools: string[];
}> {
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

// ---------------------------------------------------------------------------
// Conversation context
// ---------------------------------------------------------------------------

function getConversationSummary(ctx: RunContext): string | null {
  const tags = (ctx as Record<string, unknown>).tags;
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
  conversationSummary: string | null,
): Promise<string> {
  let synthesizedRaw: unknown;
  try {
    synthesizedRaw = await SynthesizeCoordinatorResponse({
      user_message: userText,
      delegated_transcript: transcript,
      conversation_context: conversationSummary || null,
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
// LLM-routed coordinator (Phases 1-5)
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
  return runLlmCoordinator(ctx);
}

__chat_register({ run: runCoordinator });

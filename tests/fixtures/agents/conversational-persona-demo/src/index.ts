/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: conversational-persona-demo (agent-discovery demo)
 * ----------------------------------------------------------
 * Purpose:
 * - Demonstrate intent-first orchestration where intent/plan/execution state is
 *   emitted as provenance events and the user-facing reply stays persona-scoped.
 * - Keep session FSM authority in host/runtime, not in TypeScript policy logic.
 *
 * Design model:
 * 1) Infer discovery intent from raw user text.
 * 2) Execute discovery session (GetDiscoverAgentsPlan) to enumerate candidate agents.
 * 3) Build a committed structured plan (MakeStructuredPlan) over discovered capabilities.
 * 4) Execute each committed plan step through ExecutePlanStepWithDelegate iteratively.
 * 5) Summarize delegated outputs in persona voice.
 *
 * Runtime contract:
 * - MakeStructuredPlan returns `plan_steps` (non-executable plan artifact).
 * - GetDiscoverAgentsPlan / ExecutePlanStepWithDelegate return execution hops.
 * - TS orchestrates phases and evidence; host runtime enforces transition legality.
 *
 * Provenance contract:
 * - openA2aExecutionSession submitIntent/submitPlan/startStep/completeStep/finish
 *   emit Intent/Plan/PlanStep lifecycle events for graph reconstruction.
 * - IDs here are semantic labels for lineage readability, while scope ownership
 *   and lifecycle validity are enforced host-side.
 */
import type {
  AgentCardDto,
  InternalA2aNextOutput,
  SessionResult,
} from "./baml-runtime";

type StructuredPlan = {
  objective: string;
  plan_steps: PlanStep[];
  intent_description?: string;
};
type PlanStep = {
  agent_package: string;
  agent_instance_id: string;
  sub_message?: string;
};
const PERSONA_MESSAGE_CHAR_LIMIT = 1800;
declare function InferDiscoveryIntent(args: { user_message: string }): Promise<string>;
/**
 * Host-provided execution-session lifecycle API.
 * Token value is only a caller label; session identity and scope binding are host-managed.
 */
declare function openA2aExecutionSession(token: string): Promise<{
  submitIntent: (intent: {
    intentId: string;
    description: string;
    derivedFromMessageIds: string[];
  }) => Promise<{
    submitPlan: (plan: {
      intentId: string;
      planId: string;
      steps: Array<{
        stepId: string;
        description: string;
        order: number;
        dependsOn: string[];
      }>;
    }) => Promise<{
      startStep: (stepId: string, evidenceText: string) => Promise<unknown>;
      completeStep: (stepId: string, evidenceText: string) => Promise<unknown>;
      finish: () => Promise<unknown>;
    }>;
  }>;
}>;
function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

/**
 * Normalize varied A2A result envelopes into a single output shape.
 *
 * Why needed:
 * - Different tool/session hops can return chunk arrays, single objects, or null.
 * - Downstream summarization expects one consistent InternalA2aNextOutput contract.
 */
function mergeA2aOutput(value: unknown): InternalA2aNextOutput {
  if (value == null) return { chunks: [], completion: null, historyContext: null };
  if (Array.isArray(value)) {
    const chunks: InternalA2aNextOutput["chunks"] = [];
    for (const item of value) {
      const v = item as { chunks?: InternalA2aNextOutput["chunks"] };
      if (Array.isArray(v.chunks)) chunks.push(...v.chunks);
    }
    return { chunks, completion: null, historyContext: null };
  }
  const obj = value as InternalA2aNextOutput;
  return Array.isArray(obj.chunks) ? obj : { chunks: [], completion: null, historyContext: null };
}

function readStrictStatus(value: unknown): string | null {
  if (!value || typeof value !== "object") return null;
  const status = (value as { status?: unknown }).status;
  return typeof status === "string" ? status : null;
}

function readStrictOutput(value: unknown): unknown {
  if (!value || typeof value !== "object") return null;
  return (value as { output?: unknown }).output ?? null;
}

/**
 * Extract discovery payload from either direct result or nested `output`.
 *
 * This keeps orchestration resilient to envelope variations while still requiring
 * explicit structured fields (`agents`, `done`) before trusting a candidate.
 */
function parseDiscoverAgentsOutput(raw: unknown): { agents?: AgentCardDto[]; done?: boolean } {
  if (raw && typeof raw === "object") {
    const obj = raw as { agents?: unknown; done?: unknown; output?: unknown };
    if (Array.isArray(obj.agents) || typeof obj.done === "boolean") {
      return {
        agents: Array.isArray(obj.agents) ? (obj.agents as AgentCardDto[]) : [],
        done: typeof obj.done === "boolean" ? obj.done : undefined,
      };
    }
    if (obj.output && typeof obj.output === "object") {
      const out = obj.output as { agents?: unknown; done?: unknown };
      return {
        agents: Array.isArray(out.agents) ? (out.agents as AgentCardDto[]) : [],
        done: typeof out.done === "boolean" ? out.done : undefined,
      };
    }
  }
  return { agents: [], done: true };
}

function messageIdForExecution(message: unknown): string {
  if (isObject(message)) {
    const msgId = message.messageId;
    if (typeof msgId === "string" && msgId.trim().length > 0) return msgId;
    const id = message.id;
    if (typeof id === "string" && id.trim().length > 0) return id;
  }
  return "msg-persona-fallback";
}

/**
 * Slugify human-readable intent/plan text into stable, short IDs.
 *
 * These IDs are readable lineage handles in provenance (intent/plan artifact labels),
 * not security/uniqueness boundaries.
 */
function slugToken(value: string, fallback: string): string {
  const normalized = value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  return normalized.length > 0 ? normalized : fallback;
}

function intentFromPlan(plan: StructuredPlan, userMessage: string): { intentId: string; description: string } {
  const intentDescriptionRaw = typeof (plan as { intent_description?: unknown }).intent_description === "string"
    ? String((plan as { intent_description?: string }).intent_description).trim()
    : "";
  const objectiveRaw = typeof plan.objective === "string" ? plan.objective.trim() : "";
  const fallbackDescription = "Handle user request: " + userMessage.slice(0, 140);
  const description = intentDescriptionRaw || objectiveRaw || fallbackDescription;
  return {
    intentId: "intent-" + slugToken(description, "delegate"),
    description,
  };
}

function delegatedTarget(step: PlanStep): string {
  const pkg = typeof step.agent_package === "string" ? step.agent_package.trim() : "";
  const inst = typeof step.agent_instance_id === "string" ? step.agent_instance_id.trim() : "";
  if (pkg && inst) return pkg + "/" + inst;
  if (pkg) return pkg;
  return "unknown-agent";
}

function clampPersonaMessage(message: string): string {
  const trimmed = message.trim();
  if (trimmed.length <= PERSONA_MESSAGE_CHAR_LIMIT) return trimmed;
  return trimmed.slice(0, PERSONA_MESSAGE_CHAR_LIMIT) + "…";
}

/**
 * Execute one committed delegation step by managing the A2A session directly.
 *
 * Open/Read/Finish are handled mechanically. The LLM (DecideDelegationAction)
 * only decides WHAT to say and WHEN to stop — it never sees FSM op choices.
 */
async function runDelegationStepwise(
  plan: StructuredPlan,
  step: PlanStep,
): Promise<InternalA2aNextOutput> {
  const goal = (step as { sub_message?: string }).sub_message
    || plan.objective
    || "Complete the delegated task";
  const agent = delegatedTarget(step);

  const allChunks: InternalA2aNextOutput["chunks"] = [];
  let priorResponse: string | null = null;
  const MAX_TURNS = 3;

  let sessionId: string | null = null;

  try {
    // Open the A2A session: __tool_session_open(toolName, openInputJson) returns sessionId
    const openInput = JSON.stringify({
      target: {
        agent_package: step.agent_package,
        agent_instance_id: step.agent_instance_id,
      },
    });
    const opened = await (globalThis as { __tool_session_open?: (toolName: string, args: string) => Promise<unknown> })
      .__tool_session_open!("system/internal_a2a", openInput);
    sessionId = String(opened);

    for (let turn = 0; turn < MAX_TURNS; turn++) {
      // Ask the LLM: what to say (or finish)?
      const decision = await DecideDelegationAction({
        goal,
        agent,
        prior_response: priorResponse,
      });

      // null/empty = goal achieved, finish
      const messageText = typeof decision === "string" ? decision.trim() : null;
      if (!messageText) {
        break;
      }

      // Send the message
      const sendInput = JSON.stringify({ parts: [{ text: messageText }] });
      await (globalThis as { __tool_session_send?: (id: string, args: string) => Promise<unknown> })
        .__tool_session_send!(sessionId, sendInput);

      // Read until we get a done response
      let readDone = false;
      for (let readHop = 0; readHop < 20 && !readDone; readHop++) {
        const readResultRaw = await (globalThis as { __tool_session_read?: (id: string, args: string) => Promise<unknown> })
          .__tool_session_read!(sessionId, "{}");
        const readResult = readResultRaw as Record<string, unknown>;
        const status = readResult.status as string | undefined;

        if (status === "done") {
          readDone = true;
          const output = readResult.output as Record<string, unknown> | null;
          const chunks = (output?.chunks ?? []) as InternalA2aNextOutput["chunks"];
          allChunks.push(...chunks);
          const texts = chunks
            .map((c) => c.message?.parts?.map((p: { text?: string | null }) => p.text).filter(Boolean).join(" ") ?? "")
            .filter(Boolean);
          if (texts.length > 0) priorResponse = texts.join("\n").slice(0, 2000);
        } else if (status === "streaming") {
          const output = readResult.output as Record<string, unknown> | null;
          const chunks = (output?.chunks ?? []) as InternalA2aNextOutput["chunks"];
          allChunks.push(...chunks);
        } else if (status === "aborted" || status === "error") {
          readDone = true;
        }
      }

      if (!priorResponse) break;
    }

    if (sessionId) {
      await (globalThis as { __tool_session_finish?: (id: string) => Promise<unknown> })
        .__tool_session_finish!(sessionId);
    }
  } catch (e) {
    try {
      if (sessionId) {
        await (globalThis as { __tool_session_abort?: (id: string, reason: string | null) => Promise<unknown> })
          .__tool_session_abort?.(sessionId, null);
      }
    } catch (_) {/* best-effort */}
    throw e;
  }

  return { chunks: allChunks, completion: null, historyContext: null };
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = (ctx.text ?? "").trim() || "unknown";
    try {
      const messageId = messageIdForExecution(ctx.message);
      const discoveryExecutionSession = typeof openA2aExecutionSession === "function"
        ? await openA2aExecutionSession("persona-discovery-" + Date.now().toString())
        : null;
      // Intent extraction is deliberately single-shot and goal-level to keep
      // downstream discovery prompts focused and cache-stable.
      const discoveryIntentTextRaw = await InferDiscoveryIntent({ user_message: text });
      const discoveryIntentText = (typeof discoveryIntentTextRaw === "string" && discoveryIntentTextRaw.trim().length > 0)
        ? discoveryIntentTextRaw.trim()
        : "Identify agents relevant to the user intent.";
      const discoveryIntentId = "intent-discover-" + slugToken(discoveryIntentText, "discover");
      const discoveryPlanId = "plan-discover-" + slugToken(discoveryIntentText, "discover");
      const discoveryIntentPhase = discoveryExecutionSession
        ? await discoveryExecutionSession.submitIntent({
            intentId: discoveryIntentId,
            description: discoveryIntentText,
            derivedFromMessageIds: [messageId],
          })
        : null;
      const discoveryExecutable = discoveryIntentPhase != null
        ? await discoveryIntentPhase.submitPlan({
            intentId: discoveryIntentId,
            planId: discoveryPlanId,
            steps: [
              {
                stepId: "step-discover-agents",
                description: "Find agent capabilities that can satisfy inferred intent: " + discoveryIntentText,
                order: 0,
                dependsOn: [],
              },
            ],
          })
        : null;
      if (discoveryExecutable != null) {
        // startStep/completeStep are provenance evidence markers, not tool execution.
        await discoveryExecutable.startStep(
          "step-discover-agents",
          "Searching for agent capabilities that satisfy inferred intent: " + discoveryIntentText,
        );
      }
      // Run strict session fragments until terminal so discover_agents yields concrete payload.
      const discoverRun = await runGeneratedStepExecutor(
        "GetDiscoverAgentsPlan",
        { inferred_intent: discoveryIntentText },
        { max_steps: 8 },
      );
      const discoverCandidates: unknown[] = [discoverRun.last, ...discoverRun.steps.slice().reverse()];
      let agentsResult: { agents?: AgentCardDto[]; done?: boolean } = { agents: [], done: true };
      for (const candidate of discoverCandidates) {
        const parsed = parseDiscoverAgentsOutput(candidate);
        const hasExplicitPayload = isObject(candidate)
          && (
            "agents" in candidate
            || (isObject((candidate as { output?: unknown }).output)
              && "agents" in ((candidate as { output?: Record<string, unknown> }).output as Record<string, unknown>))
          );
        if (hasExplicitPayload || (Array.isArray(parsed.agents) && parsed.agents.length > 0)) {
          agentsResult = parsed;
          break;
        }
      }
      const agents: AgentCardDto[] = Array.isArray(agentsResult?.agents) ? agentsResult.agents : [];
      if (discoveryExecutable != null) {
        await discoveryExecutable.completeStep(
          "step-discover-agents",
          "Capability-matched candidate agents identified for inferred intent.",
        );
        await discoveryExecutable.finish();
      }

      const executionSession = typeof openA2aExecutionSession === "function"
        ? await openA2aExecutionSession("persona-" + Date.now().toString())
        : null;
      // Second-phase output is a committed Plan Artifact (plan_steps), not execution hops.
      const plan = await MakeStructuredPlan({ user_message: text, agents });
      const steps = Array.isArray(plan?.plan_steps) ? plan.plan_steps : [];
      const synthesizedIntent = intentFromPlan(plan, text);
      const planId = "plan-" + slugToken(plan.objective || synthesizedIntent.description, "delegate");
      const intentPhase = executionSession
        ? await executionSession.submitIntent({
            intentId: synthesizedIntent.intentId,
            description: synthesizedIntent.description,
            derivedFromMessageIds: [messageId],
          })
        : null;
      if (steps.length === 0) {
        // Capability/meta branch: no delegation, but still emit plan/step evidence for auditability.
        const executable = intentPhase != null
          ? await intentPhase.submitPlan({
              intentId: synthesizedIntent.intentId,
              planId: planId + "-capabilities",
              steps: [
                {
                  stepId: "step-format-capabilities",
                  description: "Transform discovered agent capabilities into a concise user-facing summary.",
                  order: 0,
                  dependsOn: [],
                },
              ],
            })
          : null;
        if (executable != null) {
          await executable.startStep(
            "step-format-capabilities",
            "Preparing capability summary from discovered agents.",
          );
        }
        const message = clampPersonaMessage(String(await FormatCapabilities({ user_message: text, agents })));
        if (executable != null) {
          await executable.completeStep(
            "step-format-capabilities",
            "Capability summary generated for operator reply.",
          );
          await executable.finish();
        }
        return { message: String(message) };
      }

      const executable = intentPhase != null
        ? await intentPhase.submitPlan({
            intentId: synthesizedIntent.intentId,
            planId,
            steps: steps.map((step: PlanStep, idx: number) => ({
              stepId: "step-delegate-" + idx.toString(),
              description: "Delegate to " + delegatedTarget(step) + ": "
                + ((typeof step.sub_message === "string" && step.sub_message.trim().length > 0)
                  ? step.sub_message.trim()
                  : "Delegated plan step " + (idx + 1).toString()),
              order: idx,
              dependsOn: idx === 0 ? [] : ["step-delegate-" + (idx - 1).toString()],
            })),
          })
        : null;

      const allChunks: InternalA2aNextOutput["chunks"] = [];
      // Iterative solver over committed steps: execute in order, preserve dependencies.
      for (let idx = 0; idx < steps.length; idx++) {
        const step = steps[idx] as PlanStep;
        const stepId = "step-delegate-" + idx.toString();
        if (executable != null) {
          await executable.startStep(
            stepId,
            "Delegation started for " + delegatedTarget(step) + ".",
          );
        }
        if (
          typeof step.agent_package !== "string" ||
          typeof step.agent_instance_id !== "string" ||
          !step.agent_package ||
          !step.agent_instance_id
        ) {
          if (executable != null) {
            await executable.completeStep(
              stepId,
              "Delegation skipped due to missing target metadata.",
            );
          }
          continue;
        }
        const stepOutput = await runDelegationStepwise(plan, step);
        const stepMerged = mergeA2aOutput(stepOutput);
        if (Array.isArray(stepMerged.chunks)) allChunks.push(...stepMerged.chunks);
        if (executable != null) {
          await executable.completeStep(
            stepId,
            "Delegation completed for " + delegatedTarget(step) + ".",
          );
        }
      }
      // Merge delegated step outputs and let the persona react to the full context.
      const merged = { chunks: allChunks, completion: null, historyContext: null };
      const message = clampPersonaMessage(String(await PersonaReact({
        user_message: text,
        plan_objective: plan.objective || text,
        a2a_output: merged,
      })));
      if (executable != null) {
        await executable.finish();
      }
      return { message };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

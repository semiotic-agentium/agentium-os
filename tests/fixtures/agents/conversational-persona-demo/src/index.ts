/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: conversational-persona-demo (agent-discovery demo)
 * ----------------------------------------------------------
 * Orkemedies persona that routes via agent discovery: lists capabilities or
 * delegates via a structured plan (one or multiple agent chats), then replies in persona.
 *
 * Flow: RouteIntent → GetDiscoverAgentsPlan → FormatCapabilities or
 *       MakeStructuredPlan → for each step BuildDelegatePlan (BAML) → merge chunks → SummarizeDelegatedResult.
 * MakeStructuredPlan returns StructuredPlan with plan_steps (not "steps") so the runtime does not
 * treat it as a ToolSessionPlan; only BuildDelegatePlan/GetDiscoverAgentsPlan return executable plans.
 * This file never constructs or parses session plan JSON (prompt + session tool is LLM-driven; TS just calls BAML and gets the execution result).
 */
import type { AgentCardDto, InternalA2aNextOutput, SessionResult, StructuredPlan } from "./baml-runtime";

/** Duplicate of BAML PlanStep: generator does not emit nested types (see baml-rt-builder README). Keep in sync with persona_prompt.baml PlanStep. */
declare global {
  interface PlanStep {
    agent_package: string;
    agent_instance_id: string;
    sub_message: string;
  }
}

function mergeA2aOutput(value: unknown): InternalA2aNextOutput {
  if (value == null) return { chunks: [], completion: null };
  if (Array.isArray(value)) {
    const chunks: InternalA2aNextOutput["chunks"] = [];
    for (const item of value) {
      const v = item as { chunks?: InternalA2aNextOutput["chunks"] };
      if (Array.isArray(v.chunks)) chunks.push(...v.chunks);
    }
    return { chunks, completion: null };
  }
  const obj = value as InternalA2aNextOutput;
  return Array.isArray(obj.chunks) ? obj : { chunks: [], completion: null };
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = (ctx.text ?? "").trim() || "unknown";
    try {
      const path = await RouteIntent({ user_message: text });
      // Runtime executes the plan and returns the tool output { agents, done }, not the plan
      const agentsResult = (await GetDiscoverAgentsPlan({ user_message: text })) as {
        agents?: AgentCardDto[];
        done?: boolean;
      };
      const agents: AgentCardDto[] = Array.isArray(agentsResult?.agents)
        ? agentsResult.agents
        : [];

      if (path === "List_capabilities") {
        const message = await FormatCapabilities({ user_message: text, agents });
        return { message: String(message) };
      }

      const plan = await MakeStructuredPlan({ user_message: text, agents });
      const steps = Array.isArray(plan?.plan_steps) ? plan.plan_steps : [];
      if (steps.length === 0) {
        const message = await FormatCapabilities({ user_message: text, agents });
        return { message: String(message) };
      }

      const allChunks: InternalA2aNextOutput["chunks"] = [];
      for (const step of steps as PlanStep[]) {
        if (
          typeof step.agent_package !== "string" ||
          typeof step.agent_instance_id !== "string" ||
          !step.agent_package ||
          !step.agent_instance_id
        )
          continue;
        const stepOutput = await BuildDelegatePlan({ plan, step });
        const stepMerged = mergeA2aOutput(stepOutput);
        if (Array.isArray(stepMerged.chunks)) allChunks.push(...stepMerged.chunks);
      }
      const merged: InternalA2aNextOutput = { chunks: allChunks, completion: null };
      const message = await SummarizeDelegatedResult({ a2a_output: merged });
      return { message: String(message) };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

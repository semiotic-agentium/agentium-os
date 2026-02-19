/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: conversational-persona-demo (agent-discovery demo)
 * ----------------------------------------------------------
 * Orkemedies persona that routes via agent discovery: lists capabilities or
 * delegates to a suitable agent via system/internal_a2a, then replies in persona.
 *
 * Flow: RouteIntent → GetDiscoverAgentsPlan → FormatCapabilities or
 *       SelectAgentForMessage → BuildDelegatePlan → SummarizeDelegatedResult.
 */
import type { AgentCardDto, InternalA2aNextOutput, SessionResult } from "./baml-runtime";

function mergeA2aOutput(value: unknown): InternalA2aNextOutput {
  if (value == null) return { chunks: [] };
  if (Array.isArray(value)) {
    const chunks: InternalA2aNextOutput["chunks"] = [];
    for (const item of value) {
      const v = item as { chunks?: InternalA2aNextOutput["chunks"] };
      if (Array.isArray(v.chunks)) chunks.push(...v.chunks);
    }
    return { chunks };
  }
  const obj = value as InternalA2aNextOutput;
  return Array.isArray(obj.chunks) ? obj : { chunks: [] };
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

      const selected = await SelectAgentForMessage({ user_message: text, agents });
      if (!selected?.agent_package || !selected?.agent_instance_id) {
        const message = await FormatCapabilities({ user_message: text, agents });
        return { message: String(message) };
      }

      const a2aOutput = await BuildDelegatePlan({
        agent_package: selected.agent_package,
        agent_instance_id: selected.agent_instance_id,
        user_message: text,
      });
      const merged = mergeA2aOutput(a2aOutput);
      const message = await SummarizeDelegatedResult({ a2a_output: merged });
      return { message: String(message) };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

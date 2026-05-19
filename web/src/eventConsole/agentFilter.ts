import type { AgentDiscoveryEntry } from "../types/a2a";

/**
 * An agent is event-capable for the MVP Event Console when its agent card
 * advertises at least one dispatch subscription. The runner pre-fills
 * agent_card.subscriptions from the manifest discovery block.
 */
export function isEventCapableAgent(agent: AgentDiscoveryEntry): boolean {
  const subs = agent.agent_card?.subscriptions;
  return Array.isArray(subs) && subs.length > 0;
}

export function filterEventCapableAgents(
  agents: ReadonlyArray<AgentDiscoveryEntry>,
): AgentDiscoveryEntry[] {
  return agents.filter(isEventCapableAgent);
}

/** Short subscription summary line for an agent card (e.g. "slack, clickup → task-daemon.interpretation.v1"). */
export function summarizeSubscriptions(agent: AgentDiscoveryEntry): string {
  const subs = agent.agent_card?.subscriptions ?? [];
  if (subs.length === 0) return "no dispatch subscriptions";
  const parts: string[] = [];
  for (const sub of subs) {
    const left = sub.source_kinds.length > 0 ? sub.source_kinds.join(", ") : "*";
    const right = sub.schema_versions.length > 0 ? sub.schema_versions.join(", ") : "*";
    parts.push(`${left} → ${right}`);
  }
  return parts.join(" · ");
}

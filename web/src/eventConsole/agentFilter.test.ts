import { describe, expect, it } from "vitest";
import { filterEventCapableAgents, isEventCapableAgent } from "./agentFilter";
import type { AgentDiscoveryEntry } from "../types/a2a";

function agent(overrides: Partial<AgentDiscoveryEntry>): AgentDiscoveryEntry {
  return {
    agent_package: overrides.agent_package ?? "pkg",
    agent_instance_id: overrides.agent_instance_id ?? "inst",
    name: overrides.name ?? "agent",
    version: overrides.version ?? "0.0.0",
    agent_card: overrides.agent_card ?? {
      name: "agent",
      version: "0.0.0",
      agent_package: overrides.agent_package ?? "pkg",
      agent_instance_id: overrides.agent_instance_id ?? "inst",
      tools: [],
      capabilities: [],
    },
  };
}

describe("agentFilter", () => {
  it("treats agents with no subscriptions as not event-capable", () => {
    const a = agent({});
    expect(isEventCapableAgent(a)).toBe(false);
  });

  it("treats agents with at least one subscription as event-capable", () => {
    const a = agent({
      agent_card: {
        name: "a",
        version: "0.0.0",
        agent_package: "pkg",
        agent_instance_id: "inst",
        tools: [],
        capabilities: [],
        subscriptions: [
          {
            schema_versions: ["task-daemon.interpretation.v1"],
            source_kinds: ["slack"],
            source_keys: [],
            source_key_prefixes: [],
          },
        ],
      },
    });
    expect(isEventCapableAgent(a)).toBe(true);
  });

  it("filterEventCapableAgents only keeps agents whose card lists subscriptions", () => {
    const eventy = agent({
      agent_package: "dispatch-echo",
      agent_card: {
        name: "dispatch-echo",
        version: "1.0.0",
        agent_package: "dispatch-echo",
        agent_instance_id: "default",
        tools: [],
        capabilities: [],
        subscriptions: [
          {
            schema_versions: ["host.source-records.v1"],
            source_kinds: ["slack"],
            source_keys: [],
            source_key_prefixes: [],
          },
        ],
      },
    });
    const chatty = agent({ agent_package: "chat-only" });
    expect(filterEventCapableAgents([chatty, eventy])).toEqual([eventy]);
  });
});

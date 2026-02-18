# What do we currently have?

A BAML / ts runtime that can:
    Build and package and agent with a tool manifest
    Execute that agent
    Affordances for writing agents:
        Generated BAML for tool interfaces
        Tool BAML is session based, for a uniform tool interface to prompting
        Runtime host automatically handles the tool session loop, passing result back to agent ts - this may need to be evented
        Generated ts for tool interfaces - function signature based not session based
    Tool interfaces (will likely change), Tool registry that's written to the provenance store
    a2a, ish - we only support async messaging
    Provenance graph for agent lifecycle, LLM and tool calls - this is likely janky and should be considered unstable, but it's not too far off


# Backlog


## Core runtime
    * Give it a better name
    * Tools that can move messages from host -> agent directly, typed mailboxes, backpressure etc
    * System tools:
      * Tool discovery
      * Agent Discovery
      * Agent Communication
      * Agent Storage (A tenanted/acl controlled? provenance graph?, raw cypher / natural language interface)
      * Agent Memory (a subgraph linked to prov, with temporal logic, narrower interface?)
      * Sidecar execution
      * TUI automation??
      * Introspection / extrospection of agents via their prov and memory
    * Feature tools
      * Notion
      * Clickup
      * Gitea?
      * Consider MVP adapters that restrict them to enumerable behaviour (they are dynamic by design, this it at odds with our architecture).
      * Etc - and we need a good feedback loop for dev process / prompting of tool tool creation

## Agent build tooling
Even if we do not go for self authoring, this is useful:
* Prompting, flows for dev agents, docs
* Inputs to prompts for agent dev via system tool discovery output
* Agent dev can choose tools that it needs, author manifests
* Agentic test tooling - simulation of tools and a2a conversation using LLM - full isolation from host interaction

## Headless Host
If we have self authoring or any kind of playpen dev tool sidecars Podman orchestration is needed, a fully distributable desktop app would probably need libcontainer or similar + networking. It will need to be able to run devcontainers, and handle secret orchestration. Research good ways of hardening this if remotely hosted. Openclaw using tailscale was nice, and that should probably be the ONLY way.

We should probably start off supporting multiple tenant graphs in the host from the start, as it's a hard assumption to retrofit

Acting as a distributed system containing the same graph over multiple hosts? or do you treat an instance as it's own 'Agent' via a gateway agent (that's quite a stretch goal)

## UI Head
If we are planning on doing host OS interaction, then tauri, as it gives us per-os bindings and toolchains already. It runs web user interfaces, with a few constraints, so early versions can be built with normal web frameworks only.

UI can read directly from the provenance graph as this is a provenance-first arch - all application current and historical state will be queryable. API will mostly be needed only for interaction.

## Others
Performance metrics / scaling estimates: This approach to agentic execution *should* be bar more efficient than anything dependent on virtualisation - a baml agent quite possibly fits in the overhead of running an agent in other platforms, never mind the agent itself.

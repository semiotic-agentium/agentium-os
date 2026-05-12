# Agentium OS Architectural White Paper

## Executive Thesis

**Agentium OS** is an **agent runtime**: it executes declarative agent artefacts, owns execution and
planning surfaces, orchestrates **host-mediated** tool sessions, and captures provenance for what
occurred inside the boundary. Because effects are **host-mediated**, the runtime exhibits perfect
observability of agent behavior within that boundary.

External systems remain outside the boundary. What the runtime records is what crosses host-visible
interfaces: every LLM call, tool hop, A2A interaction, task transition, message, artefact, and
provenance event passes through runtime machinery and joins the semantic transcript.

Agentium agents are ReAct-capable task actors with enforced planning and execution semantics:
behavior is organized around tasks, plans, steps, and structured outcomes rather than open-ended
chat transcripts with ad hoc tool calls. Plans, citations, lineage, step execution, tool-session
phases, and structured replies are runtime concepts rather than informal prompt conventions.

Agentium uses **BAML**, a **prompt runtime language** that combines typed prompt programs with a
runtime that executes them against models. In plain language, it is the language and runtime used
to define structured prompting, schemas, and typed outputs so LLM calls are expressed as explicit
programs rather than unstructured strings.

The agent artefact is materially different from a conventional software package. The canonical
shareable unit is a hashed, content-addressable source bundle: manifest, TypeScript agent source,
and prompt-program definitions. It is declarative content executed by the trusted runtime as an
immutable shareable unit without an external dependency graph of its own. Runtime components,
external tools, container images, and operator environments still carry their own supply-chain
inventories, but the agent itself is a higher-confidence artefact for sharing within and between
organisations.

For clustered deployments, Agentium OS commits to **router-held stateful conversations**. The router
holds client conversation continuity at the edge and translates it into message-oriented runtime
communication for agent LLM, tool, and A2A work. The result is rebalancing and rolling agent
updates without dropping conversations. Placement, shared stores, cross-pod A2A forwarding,
deployment lifecycle APIs, and channel-oriented runtime design are the operational substrate that
makes this continuity model coherent.

## Why This Is Not a Framework

Application **frameworks** remain the right answer for many deployments. Teams that embed agents
inside their own services, already own the surrounding application architecture, and want library
first composition will continue to choose frameworks—and should.

**Agentium OS** occupies a different category: a deployable **agent runtime** product that owns the
execution boundary end-to-end—planning primitives, **host-mediated** effects, provenance as
transcript of record, repository-backed agent identity, and cluster-aware conversation
continuity—so operators run agents as managed runtime tenants rather than as bespoke application
internals.

The distinction is categorical, not pejorative. When the goal is a shared runtime with enforced
semantics and audit-grade visibility across agents, an agent-runtime deployment model fits. When
the goal is maximal in-process control inside one application, a **framework** often fits better.

## Design Goals And Architectural Drivers

### 1. Declarative Agents As Runtime-Bound Artefacts

Agentium agents should be authorable as compact, inspectable artefacts bound to the runtime
through declared surfaces and packaging contracts. Agent source is built from a manifest, TypeScript
entrypoints, prompt-program schemas and functions (BAML), generated runtime types, and
session-plan metadata.
The runtime interprets and executes this artefact through known boundaries.

This goal is the root of the architecture. The runtime can reason about an agent because
behavior is expressed through declared surfaces with explicit contracts. The agent declares
capabilities, plans work, asks the host to execute tool sessions, and emits operator-visible
structured replies.

The authoring surface is intentionally small and composable: manifest, TypeScript
orchestration, prompt-program definitions, generated typing for prompts and tools, and
packaged deployment artefacts. The build pipeline compiles TypeScript, generates
prompt-runtime types from compiled prompt-program definitions, emits runtime typings, and
packages the agent for operator deployment. The runner loads packaged agents, executes them on the
agent runtime, and exposes operator and conversational surfaces through declared orchestration and
prompt-program entry surfaces.

### 2. Perfect Observability Of Agent Behavior

Within the Agentium runtime boundary, every agent action is mediated by the host. That
is the basis for perfect observability of agent behavior. The runtime sees the agent's
planning lifecycle, LLM calls, tool-session transitions, A2A interactions, messages,
artifacts, task lifecycle, and provenance emissions because those actions are mediated
by the host.

Agent-mediated contact with external systems is recorded where it crosses host-visible surfaces:
when the agent requests an LLM completion, calls a host tool, delegates through A2A, emits an
artifact, or advances a task, that action passes through runtime machinery and becomes part of the
host-visible semantic transcript.

Provenance records typed runtime events into a graph-normalized model persisted in SurrealDB.
Conversation projection reconstructs agent-visible history from that graph and keeps
canonical history separate from phase overlays used for prompting. OpenTelemetry spans and
metrics provide operational visibility; provenance remains the semantic transcript of agent
behavior inside the runtime boundary.

### 3. Enforced Planning And ReAct-Capable Task Execution

Agentium agents should reason, plan, act, observe, and revise under runtime supervision.
Planning is expressed through explicit runtime primitives such as intent submission, plan
submission, step start, step completion, finish, supersession, and citation-bound lineage.

This lets Agentium agents operate as task actors. The agent can classify a turn, derive
intent, produce a structured plan, execute steps through host-mediated actions, revise
when observations invalidate the plan, and produce a structured final reply. The runtime
can observe and validate this flow because plan and step mutations are explicit.

Planning primitives are explicit runtime operations with protocol semantics. Intent submission,
plan submission, step lifecycle, supersession, and citation-bound lineage are host-visible
protocol surfaces. Session prompt construction treats plan anchoring and iterative solving as
first-class engineering concerns rather than ad hoc context stuffing. Citations and lineage are
host-bound and validated against stable runtime-derived references. Generated step execution
is authoritative in the Rust runtime loop, keeping transitions enforceable even when JavaScript
orchestrates the outer task flow.

### 4. Host-Owned Tool Sessions And External Effects

External effects are mediated by the host through declared tool sessions. Prompt programs emit
tool-session fragments, and Rust executes the finite-state machine. The legal lifecycle is
Open, Send or Read, then Finish or Abort, with phase-specific generated types preventing
invalid transitions from becoming normal runtime behavior.

This design gives the runtime a durable boundary around tool use. Tool access can be
allowlisted in the manifest, represented in generated prompt-runtime types, executed by Rust,
and
captured in provenance.

Tool implementations are defined by contracts, metadata, schemas, and session-plan bindings.
JavaScript orchestration never executes effects directly; the host interprets each hop.

Partners can author **standalone external tools** outside the Agentium OS tree: a tool directory
carries metadata and an implementation that satisfies the host tool protocol. Operators register
those packages with the runner at deploy time. The host still owns the session finite-state
machine, phase validation, allowlists, and provenance regardless of where the implementation was
built.

Selectable **tool backends** keep that boundary stable while widening how implementations ship:

- **Static tools** are compiled into the platform binary and invoked in-process for tightly
coupled integrations.
- **External process tools** run as host subprocesses using the same framed protocol over
standard I/O.
- **Sandbox tools** run inside a **microVM-isolated guest** (microsandbox-backed): the host
launches a guest **adapter** that speaks the protocol from inside the sandbox while the runner
applies network posture, secrets binding, and lifecycle limits at the isolation boundary.

All paths converge on one agent-runtime rule: the agent receives typed observations only after host
mediation. Coordinator-level planning artifacts use distinct shapes from executable tool-session
fragments so strategic orchestration stays explicitly layered above tactical tool execution.

### 5. Source-Bundle Identity And No External SBOM For Agents

Agentium agents should be shareable as content-addressed declarative source bundles with
inspectable structure and lineage. Canonical identity is a validated SHA-256 content hash over the
declarative source bundle (manifest, TypeScript source, and prompt-program source) after
repository-assigned version stamping in the manifest. Publish flows compute identity from
authoritative source inputs, build deployable archives server-side, enforce hash integrity at
publish time, and store lineage as immutable entries. Deploy-by-hash ties runtime activation to
that identity so operators promote agents as immutable artefacts rather than floating package
versions. The
deployable archive contains generated and runtime support artefacts, but the shareable agent
identity remains the declarative source bundle.

This is the basis for the no-external-SBOM claim. The agent itself has no external software
dependency tree of its own. It is executed by the trusted runtime and may use runtime-provided
tools while remaining a declarative package rather than a general-purpose installable dependency
graph.

### 6. Cluster-Aware Conversation Continuity

Agentium OS should preserve conversations while allowing clustered runtimes to rebalance
work and roll agent versions. The router holds client conversation continuity at the edge
and turns conversational connectivity into message-oriented runtime work.
That enables a runner or agent instance to change without forcing the client conversation
to drop.

Cluster deployments use multiple runners backed by shared SurrealDB state, cluster runner
registration, agent placement, heartbeat-based liveness, cross-pod A2A forwarding driven by
placement resolution, validated forwarding paths, migration controls, and channel-oriented
runtime design. Together these operational mechanics realize clustered continuity without tying
client conversations to a single execution pod. Kubernetes installs ship as a Helm chart as
the supported operator surface. Forwarding, migration controls, heartbeat-driven runner
liveness, and multi-agent routing are first-class cluster behaviors. Continuity is layered: the
router preserves client-attached streams at the edge, while shared stores, placement, migration
controls, and provenance-backed task state provide durable coordination behind that edge.

## System Context

Agentium OS is easiest to understand from the outside in: first the actors and external
dependencies, then the runtime boundary, then the deployable services inside that boundary.

```mermaid
flowchart TD
    AgentAuthor["Agent Author"] --> AgentSource["Declarative Agent Source"]
    Operator["Operator"] --> RunnerApi["Runner API"]
    EndUser["End User"] --> A2aIngress["A2A Ingress"]
    AgentSource --> Builder["Agent Builder"]
    Builder --> Repository["Content-Addressed Repository"]
    Repository --> Runner["Agentium Runtime Runner"]
    A2aIngress --> Runner
    Runner --> BamlRuntime["Prompt Runtime (BAML)"]
    Runner --> QuickJs["QuickJS Agent Host"]
    Runner --> ToolHost["Host Tool Sessions"]
    Runner --> Provenance["Provenance Graph"]
    Runner --> Observability["OTLP Metrics And Traces"]
    ToolHost --> ExternalSystems["External Systems"]
    BamlRuntime --> LlmProviders["LLM Providers"]
    Runner --> OtherAgents["A2A Agents"]
```



Boundary definition: external systems sit outside Agentium OS; agent-mediated interactions pass
through host-visible runtime interfaces.

## Solution Strategy

Agentium OS uses a typed Rust host to turn declarative agent artefacts into observable
ReAct executions. The host coordinates the prompt runtime (BAML), QuickJS entrypoints,
generated runtime types,
tool-session FSMs, A2A transport, repository deployment, provenance graph writes, and
operator APIs.

The solution strategy has seven pillars:

- **Unified agent runtime:** agent behavior executes through explicit host surfaces with
protocol-visible transitions.
- **Planning as protocol:** intent, plan, step, finish, citation, and supersession are
runtime concepts.
- **Host-owned external effects:** LLM, tool, A2A, task, message, and artifact actions
cross host-visible boundaries.
- **Graph-first provenance:** conversation history and citations reconstruct from recorded graph
edges anchored to runtime-derived identities.
- **Content-addressed distribution:** agent source bundles are hashed and deployed by
identity.
- **Operational cluster layer:** placement, forwarding, shared state, and router-held
conversations together define uninterrupted clustered operation.
- **Operator separation:** public A2A routes and token-authenticated operator routes have
different trust boundaries.

## Static Architecture

Static architecture groups collaborating services by responsibility. The decomposition below names
the major services that recur throughout runtime, deployment, and operations narratives.

```mermaid
flowchart TD
    subgraph authoring["Authoring And Packaging"]
        AgentManifest["manifest.json"]
        AgentTs["TypeScript Agent Source"]
        AgentBaml["Prompt Program Source"]
        BuilderCrate["Agent Builder"]
        HashCrate["Content Identity Hasher"]
        RepositoryCrate["Agent Repository"]
    end

    subgraph runtime["Runtime Execution"]
        RunnerCrate["Agent Runner"]
        QuickJsCrate["JavaScript Host"]
        ToolsCrate["Host Tool Runtime"]
        InterceptorCrate["Interceptor Pipeline"]
        A2aCrate["A2A Runtime"]
    end

    subgraph operationalLayer["Operational Layer Services"]
        ProvenanceCrate["Provenance Store"]
        ConversationCrate["Conversation Projection"]
        ObservabilityCrate["Observability Layer"]
        ApiCrate["HTTP API Surface"]
        RouterCrate["Cluster Routing Client"]
    end

    AgentManifest --> BuilderCrate
    AgentTs --> BuilderCrate
    AgentBaml --> BuilderCrate
    BuilderCrate --> HashCrate
    HashCrate --> RepositoryCrate
    RepositoryCrate --> RunnerCrate
    RunnerCrate --> QuickJsCrate
    RunnerCrate --> ToolsCrate
    RunnerCrate --> A2aCrate
    RunnerCrate --> ProvenanceCrate
    RunnerCrate --> ObservabilityCrate
    ApiCrate --> RunnerCrate
    RouterCrate --> ApiCrate
    ProvenanceCrate --> ConversationCrate
```



Authoring and packaging services establish the agent as a content-addressed source bundle.
Runtime execution services host JavaScript orchestration, prompt-runtime execution, A2A, and
tools. Operational-layer services provide provenance, conversation projection, observability, HTTP
API boundaries, and cluster routing helpers.

## Runtime Flows

### Enforced Planning And ReAct Execution

```mermaid
sequenceDiagram
    participant User
    participant A2A as A2A Runtime
    participant JS as QuickJS Agent
    participant Plan as Planning Runtime
    participant PromptRuntime as Prompt Runtime
    participant Tool as Host Tool FSM
    participant Prov as Provenance Graph

    User->>A2A: message.sendStream
    A2A->>JS: run(ctx)
    JS->>Plan: submitIntent
    Plan->>Prov: record intent lineage
    JS->>PromptRuntime: produce structured plan
    JS->>Plan: submitPlan
    Plan->>Prov: record plan and steps
    loop each step
        JS->>Plan: startStep
        JS->>PromptRuntime: select or act
        PromptRuntime->>Tool: emit session fragment
        Tool->>Prov: record host-mediated action
        Tool-->>JS: observation
        JS->>Plan: completeStep
    end
    JS->>Plan: finish
    JS-->>A2A: StructuredReply
    A2A-->>User: streamed response
```



### Tool Session Execution

Tool effects are owned by the host runtime. The prompt runtime returns a declarative fragment;
Rust extracts it, the tool registry validates phase and policy, the host executes the session hop
(in-process, via an external tool process, or by driving a sandbox guest adapter), and provenance
records the mediated action.

```mermaid
sequenceDiagram
    participant JS as QuickJS Agent
    participant PromptFn as Prompt Function
    participant Extract as Tool Extraction
    participant Registry as Tool Registry
    participant Tool as Rust Tool Handler
    participant Prov as Provenance

    JS->>PromptFn: call session planning function
    PromptFn-->>Extract: JSON fragment with op
    Extract->>Registry: validate tool and phase
    Registry->>Tool: execute Open Send Read Finish Abort
    Tool->>Prov: emit effect event
    Tool-->>JS: typed observation
```



### Cluster Forwarding

```mermaid
sequenceDiagram
    participant Client
    participant RunnerA as Ingress Runner
    participant Store as Shared SurrealDB
    participant RunnerB as Placement Runner
    participant Agent as Agent Runtime

    Client->>RunnerA: A2A request
    RunnerA->>Store: resolve agent placement
    Store-->>RunnerA: RunnerB endpoint
    RunnerA->>RunnerB: validated forwarded request
    RunnerB->>Agent: local execution
    Agent-->>RunnerB: JSON-RPC stream chunks
    RunnerB-->>RunnerA: response chunks
    RunnerA-->>Client: A2A response
```



### Router-Held Stateful Conversations

```mermaid
sequenceDiagram
    participant Client
    participant Router as Stateful Conversation Router
    participant RunnerA as Runner A
    participant RunnerB as Runner B
    participant Store as Shared Runtime State

    Client->>Router: conversation stream
    Router->>RunnerA: message-oriented work item
    RunnerA->>Store: persist state and provenance
    RunnerA-->>Router: response event
    Router-->>Client: stream response
    Router->>RunnerB: continue conversation after rebalance
    RunnerB->>Store: resume from shared state
    RunnerB-->>Router: response event
    Router-->>Client: uninterrupted conversation
```



## Deployment And Operations Model

Kubernetes installs ship through a published Helm chart for Agentium OS. A representative
cluster topology runs multiple runner replicas backed by shared SurrealDB. Runner identity and
deployment environment are exported through OpenTelemetry resource attributes. Operator routes
are token-authenticated in cluster mode. Conversational endpoints (`/chat`, `/dispatch`, discovery,
provenance reads) remain part of the public surface and are reachable from any pod on the cluster
network; the SurrealDB ingress policy is the only per-component NetworkPolicy fence. Cross-runner
forwarding therefore depends on the cluster fabric perimeter plus operator-route token gating.
Repository storage and runner-local deployment state are intentionally separated so artefacts
remain immutable while runtime activation evolves. Deployments are hash-centric with restore
semantics tied to recorded deployment state.

The router is the durable conversation edge. Runtime work behind that edge is message-oriented.
Rebalancing and rolling agent updates preserve client-attached conversations. Operational practice
pairs edge continuity with cluster placement, shared persistence, and controlled migration so
work relocates cleanly while clients keep a single conversational attachment point.

## Cross-Cutting Concepts

### Threat Model

This subsection states trust boundaries, protected assets, and the architectural controls that keep
the **agent runtime** posture predictable for operators.

**Trust boundaries.**

- **Agent code versus host.** JavaScript orchestration proposes work; the Rust host interprets
  planning mutations, executes tool-session finite-state transitions, and performs **host-mediated**
  LLM, tool, A2A, task, message, and artefact actions.
- **Public conversational surfaces versus operator APIs.** End-user A2A ingress (`/chat`,
  `/dispatch`) and provenance reads are public on the cluster network; operator routes (deployment
  lifecycle, configuration, repository mutations) are token-authenticated in cluster mode. The
  runner NetworkPolicy does not fence the public surface — the cluster fabric perimeter is the trust
  boundary for cross-pod A2A. SurrealDB ingress is the one component-level NetworkPolicy fence.
- **Runner cluster versus external networks.** Cross-runner forwarding is validated and intended
  for isolated cluster fabrics; control-plane targets carry SSRF protections.
- **Guest tool adapters versus host policy.** External tools may run as subprocesses or inside
  microVM guests; either way the host retains session ownership, manifest allowlists, and—for
  sandbox mode—network posture and lifecycle limits at the isolation boundary.

**Protected assets.**

- Operator secrets and configuration surfaced only through declared resolver paths.
- Repository and deployment integrity tied to content-addressed agent identity.
- Provenance graph as audit-grade semantic transcript; conversation projection derived from graph
  structure rather than ad hoc joins.
- End-user conversational continuity at the router edge during cluster churn.

**Controls (architecture-level).**

- **Host-mediated effects** as the default: no alternate path for tools or LLM calls that bypasses the
  runtime boundary relevant to observability.
- **Manifest allowlists** and generated typing that bind prompt programs to named tool capabilities.
- **Selectable tool backends** (in-process, external process, microVM sandbox) without weakening the
  session FSM or provenance capture at the host.
- **Token-authenticated operator routes** and **validated cluster forwarding** so lateral movement
  does not rely on implicit network trust alone.

### Provenance

Provenance is the semantic transcript of record for agent behavior inside the runtime boundary.
Runtime events are normalized into graph form, conversation history is projected from that graph,
and citations refer back to stable runtime-derived references.

### Observability

OpenTelemetry spans and metrics provide operational visibility into ingress, routing,
effects, tools, LLM calls, provenance I/O, and runner identity. Provenance carries the semantic
history of agent behavior; OpenTelemetry carries the operational measurement surface for running
production clusters.

### Extensibility

Extensibility comes through declarative agent packages, prompt-program schemas and
functions (BAML), generated
runtime types, platform-built tools, independently packaged external tools (subprocess or
microVM sandbox adapters), event dispatch, A2A, and repository
lineage. Every extension point remains runtime-visible: agents extend capability through
declarations and host-mediated execution surfaces with explicit contracts.

## Position In The Landscape

The market vocabulary for **agent runtimes** is stabilizing. **Amazon Bedrock AgentCore**
deserves explicit mention: it is a prominent managed offering that helps validate the category—teams
want execution boundaries, lifecycle tooling, and operational packaging around agents rather than
only ad hoc orchestration code.

**Agentium OS** draws a sharp line from that validation without implying superiority of one shape
over another:

- **Deployment stance.** Agentium OS targets operators running the runtime under their own
  Kubernetes and tenancy controls—repository-backed promotion, deploy-by-hash semantics, and Helm
  as the supported install surface—rather than a proprietary hyperscaler control plane.
- **Semantic transcript.** Graph-first **provenance** is the architectural transcript of record for
  host-mediated behavior; conversation reconstruction and citations trace runtime-derived anchors,
  not informal logs assembled outside the graph.
- **Agent artefact model.** Content-addressed declarative **source bundles** define canonical agent
  identity separately from container and runtime supply-chain inventories.
- **Cluster continuity.** Router-held **stateful conversations** pair edge attachment with shared
  placement and persistence so client streams survive rebalance and rolling updates.
- **Prompt runtime language.** **BAML** is embedded as the typed prompt-program surface agents and
  operators reason about inside this runtime.
- **Independent tools.** Standalone external tools—subprocess or microVM sandbox adapters—extend the
  runtime without collapsing effect mediation back into agent JavaScript.

Taken together, AgentCore and Agentium OS **co-validate** the **agent runtime** category while
occupying different operational and packaging commitments; partners choose based on control-plane
ownership, transcript semantics, and tenancy—not on which approach is more advanced in the abstract.

## Architectural Commitments

The architecture is summarized as a compact set of definitional commitments:

- Declarative agents as compact, inspectable runtime-bound packages.
- A unified agent runtime for planning, tools, A2A, and provenance.
- Enforced planning with explicit protocol primitives and host-visible lineage.
- ReAct-capable task actors producing structured, operator-visible outcomes.
- Rust host ownership of tool sessions, effect mediation, and selectable backends (in-process,
external process, microVM sandbox).
- QuickJS as the JavaScript orchestration boundary.
- A2A as the agent interoperability protocol surface.
- Content-addressed source-bundle identity for sharing and promotion.
- Graph-first provenance as the semantic transcript of record.
- Router-held stateful conversations as the clustered continuity model.
- Helm chart as the supported Kubernetes install surface.

These commitments align incentives for partners: agents stay small and declarative, while the
agent runtime supplies enforcement, observability, auditability, and operational control at scale.

## Partner Outcomes

Agentium OS is designed so partners get predictable multi-tenant agent operations without turning
every agent into bespoke infrastructure code.

- **Continuous conversations in clusters:** router-held streams preserve client attachment while
runners rebalance and agent versions roll forward.
- **Legible agent behavior:** provenance-backed histories make execution inspectable as a
first-class operational narrative.
- **Governed external effects:** host tool sessions turn integrations into enforceable,
allowlisted capabilities with explicit lifecycle semantics—including independently authored tools
that run as subprocesses or inside microVM sandboxes without weakening host mediation.
- **Shareable agent artefacts:** content-addressed source bundles make agents easy to publish,
promote, and reason about across teams and organizations.
- **Operational clarity:** Kubernetes installs and observability defaults align runtime identity,
routing, and telemetry into a coordinated agent-runtime operations model.

## Summary

**Agentium OS is a deployable agent runtime that executes declarative agents with host-mediated
effects, graph-first provenance, and cluster-aware conversation continuity—so operators run agents
with audit-grade visibility without rebuilding agent infrastructure as bespoke application code.**

Terminology used throughout this paper:

| Term | Meaning |
|------|--------|
| **Agent runtime** | The categorical noun for the class of systems (execution boundary + semantics + operations). |
| **Agentium OS** | The product described in this paper. |
| **Host-mediated** | Describes LLM calls, tools, A2A, tasks, artefacts, and provenance crossings that pass through runtime machinery. |
| **Substrate** | The operational layer (packaging, placement, routing, day-two ops)—not a synonym for the whole product. |
| **Framework** | A library-first embedding model, contrasted here with a deployable agent runtime; frameworks remain the right choice for many teams. |

## Glossary

- **Agent runtime:** the class of execution environments that run declarative agents with
host-mediated effects and operator-visible semantics; **Agentium OS** is one product instance.
- **Agentium OS:** the deployable agent runtime product described in this paper.
- **Amazon Bedrock AgentCore (AgentCore):** AWS managed agent-runtime offering cited here as a
category co-validator for **agent runtime** alongside Agentium OS.
- **Declarative agent artefact:** the manifest, TypeScript orchestration, prompt-program
definitions, generated typing, and packaged outputs treated as one deployable unit.
- **BAML:** the prompt runtime language embedded in Agentium OS for typed prompting programs and
generated runtime interfaces.
- **Host tool session:** the finite-state execution of an external effect mediated by the Rust
host, driven by declarative fragments emitted from prompt programs.
- **External tool:** a standalone tool package (metadata plus implementation) authored and versioned
outside Agentium OS, registered with the runner and executed through a host-selected backend.
- **Sandbox tool adapter:** the guest-side program inside a microVM that implements the framed tool
protocol; the host spawns and constrains it while retaining session ownership and provenance.
- **Provenance graph:** the normalized record of host-mediated actions used for audit,
replay-oriented understanding, and conversation reconstruction.
- **A2A:** the agent-to-agent protocol surface used for interoperability and delegation across
agents.
- **Router-held conversation:** client-attached conversational continuity owned at the edge and
translated into message-oriented runtime work behind that edge.
- **Content-addressed identity:** the canonical hash binding an agent's declarative source
bundle to an immutable artefact identity.

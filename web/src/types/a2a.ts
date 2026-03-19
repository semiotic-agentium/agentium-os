/** Agent card nested in discovery response */
export interface AgentCardInfo {
  name: string;
  version: string;
  agent_package: string;
  agent_instance_id: string;
  tools: string[];
  baml_functions?: string[];
  description?: string | null;
  capabilities: string[];
}

/** Agent discovery entry from GET /agents */
export interface AgentDiscoveryEntry {
  agent_package: string;
  agent_instance_id: string;
  name: string;
  version: string;
  agent_card: AgentCardInfo;
}

/** Token usage counts from metrics API */
export interface TokenUsage {
  in: number;
  out: number;
  total: number;
}

/** Per-turn metrics from GET /contexts/{context_id}/metrics */
export interface TurnMetrics {
  message_id: string;
  user_prompt_count: number;
  llm_call_count: number;
  llm_duration_ms_total: number;
  tokens: TokenUsage;
}

/** Session aggregate metrics */
export interface SessionMetrics {
  turns_total: number;
  user_prompts_total: number;
  llm_calls_total: number;
  llm_duration_ms_total: number;
  tokens_total: TokenUsage;
}

/** Response from GET /contexts/{context_id}/metrics */
export interface ContextMetricsResponse {
  context_id: string;
  turns: TurnMetrics[];
  session: SessionMetrics;
}

/** A2A message part */
export interface Part {
  text?: string;
  kind?: string;
  data?: unknown;
}

/** A2A message */
export interface A2aMessage {
  messageId: string;
  role: string;
  parts: Part[];
  contextId?: string;
  taskId?: string;
  referenceTaskIds?: string[];
  extensions?: string[];
  metadata?: Record<string, unknown>;
}

/** SSE response: JSON-RPC success envelope */
export interface JSONRPCResponse {
  jsonrpc: "2.0";
  id: string | null;
  result?: StreamChunkResult;
  error?: { code: number; message: string; data?: unknown };
}

export interface StreamChunkResult {
  stream: boolean;
  index: number;
  final: boolean;
  chunk: ChunkPayload;
  /** Present and true when this chunk was relayed from the tool path (async). Non-standard; use to distinguish from normal collect-path chunks. */
  toolStreamChunk?: boolean;
}

export interface ChunkPayload {
  message?: A2aMessage;
  task?: TaskPayload;
  statusUpdate?: StatusUpdatePayload;
  /** When result.toolStreamChunk is true, chunk may have tool stream fields. */
  toolName?: string;
  events?: ToolEvent[];
  completion?: ToolCompletion;
  chunk?: unknown;
}

export interface StatusUpdatePayload {
  taskId?: string;
  status?: TaskStatus;
  /** Nested event body (relay sends statusUpdate/status_update) */
  statusUpdate?: {
    message?: A2aMessage;
    metadata?: Record<string, unknown>;
    contextId?: string;
    taskId?: string;
    status?: TaskStatus;
  };
  status_update?: {
    message?: A2aMessage;
    metadata?: Record<string, unknown>;
    contextId?: string;
    taskId?: string;
    status?: TaskStatus;
  };
}

export interface TaskPayload {
  id?: string;
  contextId?: string;
  status?: TaskStatus;
  /** Tool stream payload: when result.toolStreamChunk is true, task may carry tool events. */
  toolName?: string;
  events?: ToolEvent[];
  completion?: ToolCompletion;
  artifacts?: unknown[];
  history?: unknown[];
}

export interface TaskStatus {
  state?: string;
  message?: A2aMessage;
  metadata?: Record<string, unknown>;
}

/** One event from a tool stream (e.g. claude/dev). */
export interface ToolEvent {
  kind: string;
  thinking?: string;
  text?: string;
  name?: string;
  input?: unknown;
  subtype?: string;
  result?: string;
  [key: string]: unknown;
}

export type ToolCompletion = "DONE" | "INPUT_REQUIRED" | "INTERRUPTED";

/** Block inside an agent message (text or tool notification). */
export type ContentBlock = TextContentBlock | ToolNotificationBlock;

export interface TextContentBlock {
  type: "text";
  text: string;
}

export interface ToolNotificationBlock {
  type: "tool";
  toolName: string;
  status: string;
  events: ToolEvent[];
  completion?: ToolCompletion;
}

/** Internal chat message for the UI */
export interface ChatMessage {
  id: string;
  role: "user" | "agent";
  text: string;
  timestamp: Date;
  isStreaming?: boolean;
  /** When set, UI renders blocks instead of single text (agent messages only). */
  contentBlocks?: ContentBlock[];
  /** When true, stream ended with TASK_STATE_INPUT_REQUIRED; agent is waiting for user reply */
  awaitingInput?: boolean;
  /** Optional prompt from the agent (e.g. from awaitInput(prompt)); show as hint/placeholder */
  inputRequiredPrompt?: string;
  /** A2A message metadata (if present in the inbound message) */
  metadata?: Record<string, unknown>;
  /** Task state transitions recorded during SSE streaming */
  stateTransitions?: StateTransition[];
}

/** Workflow phase tracker — parsed from coordinator SSE progress messages */
export type WorkflowPhaseName = "discovery" | "planning" | "execution" | "synthesis" | "idle";

export interface WorkflowNodeStatus {
  name: string;
  status: "pending" | "running" | "completed" | "failed";
}

export interface WorkflowProgressState {
  phase: WorkflowPhaseName;
  iteration?: number;
  nodes: WorkflowNodeStatus[];
  completedNodes: string[];
  /** True once the coordinator pipeline has meaningfully started (discovery or planning seen). */
  pipelineActive?: boolean;
}

/** State transition for task timeline */
export interface StateTransition {
  state: string;
  timestamp: Date;
}

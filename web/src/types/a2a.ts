/** Agent discovery entry from GET /agents */
export interface AgentDiscoveryEntry {
  agent_package: string;
  agent_instance_id: string;
  name: string;
  version: string;
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
}

export interface ChunkPayload {
  message?: A2aMessage;
  task?: TaskPayload;
  statusUpdate?: { status?: TaskStatus };
}

export interface TaskPayload {
  id?: string;
  contextId?: string;
  status?: TaskStatus;
}

export interface TaskStatus {
  state?: string;
  message?: A2aMessage;
}

/** Internal chat message for the UI */
export interface ChatMessage {
  id: string;
  role: "user" | "agent";
  text: string;
  timestamp: Date;
  isStreaming?: boolean;
}

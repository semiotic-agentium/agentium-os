/**
 * A2A (Agent-to-Agent) types for BAML agent packages.
 * Only message.sendStream is supported; all message handling is async/stream.
 */

export interface Part {
  text?: string;
  data?: unknown;
  raw?: string;
  url?: string;
  filename?: string;
  mediaType?: string;
  metadata?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface Message {
  messageId: string;
  role: string;
  parts: Part[];
  contextId?: string;
  taskId?: string;
  referenceTaskIds?: string[];
  extensions?: string[];
  metadata?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface SendMessageRequest {
  message: Message;
  configuration?: {
    acceptedOutputModes?: string[];
    blocking?: boolean;
    historyLength?: number | string;
    [key: string]: unknown;
  };
  metadata?: Record<string, unknown>;
  tenant?: string;
  [key: string]: unknown;
}

export interface Artifact {
  artifactId?: string;
  name?: string;
  description?: string;
  parts: Part[];
  metadata?: Record<string, unknown>;
  extensions?: string[];
  [key: string]: unknown;
}

export interface TaskStatus {
  state?: string;
  message?: Message;
  timestamp?: string;
  [key: string]: unknown;
}

export interface Task {
  id?: string;
  contextId?: string;
  artifacts?: Artifact[];
  history?: Message[];
  status?: TaskStatus;
  metadata?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TaskStatusUpdateEvent {
  contextId?: string;
  taskId?: string;
  status?: TaskStatus;
  metadata?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TaskArtifactUpdateEvent {
  contextId?: string;
  taskId?: string;
  lastChunk?: boolean;
  append?: boolean;
  artifact?: Artifact;
  metadata?: Record<string, unknown>;
  [key: string]: unknown;
}

/** One chunk in a message.sendStream response. */
export type A2aStreamChunk =
  | { message: Message; task?: never; statusUpdate?: never; artifactUpdate?: never }
  | { message?: never; task: Task; statusUpdate?: never; artifactUpdate?: never }
  | { message?: never; task?: never; statusUpdate: TaskStatusUpdateEvent; artifactUpdate?: never }
  | { message?: never; task?: never; statusUpdate?: never; artifactUpdate: TaskArtifactUpdateEvent }
  | { message?: Message; task?: Task; statusUpdate?: TaskStatusUpdateEvent; artifactUpdate?: TaskArtifactUpdateEvent; [key: string]: unknown };

export interface A2aJsonRpcRequest {
  jsonrpc: string;
  method: "message.sendStream";
  params?: SendMessageRequest;
  id?: string | number | null;
}

export interface BamlAgent {
  handle_a2a_request(request: A2aJsonRpcRequest): Promise<A2aStreamChunk[]>;
  handle_a2a_cancel?(args: { id: string; tenant?: string }): Promise<void>;
}

declare global {
  function __baml_a2a_register(
    agent: BamlAgent & { tools?: Record<string, (args: unknown) => Promise<unknown>> }
  ): void;
}

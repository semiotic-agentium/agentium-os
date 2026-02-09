/**
 * A2A (Agent-to-Agent) types for BAML agent packages.
 * Only message.sendStream is supported; all message handling is async/stream.
 *
 * Conversation routing:
 * - The host invokes the handler once per incoming chat message and binds each invocation
 *   to the correct conversation (context). Multiple conversations may be in progress in parallel;
 *   each message is delivered with the correct conversation context, and yielded chunks are
 *   attributed to that conversation by the host.
 *
 * Stream contract:
 * - For message.sendStream the host sets globalThis.__baml_chat_yield_buffer and
 *   globalThis.__baml_chat_yield before invoking onChatMessage.
 * - The agent MUST call __baml_chat_yield(chunk) for each ChatStreamChunk it produces.
 *   The host reads the buffer after the promise resolves and uses it as the stream.
 * - Return value of onChatMessage is ignored. No fallback.
 * - IDs, history, tenant/config, and other protocol metadata are handled by the host.
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

/** Incoming or outgoing message content (parts only; IDs/role/etc. are host-managed). */
export interface Message {
  parts: Part[];
}

/** Incoming chat message passed to the JS handler (parts only). */
export type ChatMessage = Message;

export interface Artifact {
  name?: string;
  description?: string;
  parts: Part[];
}

export interface TaskStatus {
  state?: string;
  message?: Message;
  timestamp?: string;
  [key: string]: unknown;
}

export interface Task {
  artifacts?: Artifact[];
  status?: TaskStatus;
}

export interface TaskStatusUpdateEvent {
  status?: TaskStatus;
}

export interface TaskArtifactUpdateEvent {
  lastChunk?: boolean;
  append?: boolean;
  artifact?: Artifact;
}

/** One chunk in a message.sendStream response. */
export type ChatStreamChunk =
  | { message: Message; task?: never; statusUpdate?: never; artifactUpdate?: never }
  | { message?: never; task: Task; statusUpdate?: never; artifactUpdate?: never }
  | { message?: never; task?: never; statusUpdate: TaskStatusUpdateEvent; artifactUpdate?: never }
  | { message?: never; task?: never; statusUpdate?: never; artifactUpdate: TaskArtifactUpdateEvent }
  | { message?: Message; task?: Task; statusUpdate?: TaskStatusUpdateEvent; artifactUpdate?: TaskArtifactUpdateEvent; [key: string]: unknown };

export interface BamlAgent {
  /** Receives the incoming chat message (parts only). Yield each chunk via __baml_chat_yield(chunk); return value is ignored. */
  onChatMessage(message: ChatMessage): Promise<void>;
}

declare global {
  function __baml_chat_register(
    agent: BamlAgent & { tools?: Record<string, (args: unknown) => Promise<unknown>> }
  ): void;
  /** Set by host before stream requests. Agent must call once per chunk. */
  function __baml_chat_yield(chunk: ChatStreamChunk): void;
}

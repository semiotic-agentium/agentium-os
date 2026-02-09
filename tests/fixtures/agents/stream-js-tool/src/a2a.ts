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
  onChatMessage(message: ChatMessage): Promise<void>;
}

declare global {
  function __baml_chat_register(
    agent: BamlAgent & { tools?: Record<string, (args: unknown) => Promise<unknown>> }
  ): void;
  /** Set by host before stream requests. Agent must call once per chunk. */
  function __baml_chat_yield(chunk: ChatStreamChunk): void;
}

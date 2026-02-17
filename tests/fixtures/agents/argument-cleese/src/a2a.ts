/**
 * A2A (Agent-to-Agent) types for BAML agent packages.
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

export type ChatStreamChunk =
  | { message: Message; task?: never; statusUpdate?: never; artifactUpdate?: never }
  | { message?: Message; task?: Task; statusUpdate?: unknown; artifactUpdate?: unknown; [key: string]: unknown };

export interface BamlAgent {
  onChatMessage(message: ChatMessage): Promise<void>;
}

declare global {
  function __baml_chat_register(
    agent: BamlAgent & { tools?: Record<string, (args: unknown) => Promise<unknown>> }
  ): void;
  function __baml_chat_yield(chunk: ChatStreamChunk): void;
}

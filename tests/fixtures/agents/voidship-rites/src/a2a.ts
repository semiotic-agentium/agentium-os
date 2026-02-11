export interface ChatMessage {
  role?: string;
  parts?: { text?: string }[];
}
export interface ChatStreamChunk {
  message?: { parts: { text: string }[] };
  task?: Task;
}
export interface Task {
  status: { state: string; message?: { parts: { text: string }[] } };
}

import type { ToolCompletion } from "../types/a2a";

export function statusFromFsmPhase(fsmPhase: string): string {
  const phase = fsmPhase.toLowerCase();
  if (phase.includes("complete") || phase.includes("done") || phase.includes("finish")) return "Done";
  if (phase.includes("error") || phase.includes("fail") || phase.includes("abort")) return "Interrupted";
  return "Running";
}

export function completionFromStatus(status: string): ToolCompletion | undefined {
  if (status === "Done") return "DONE";
  if (status === "Interrupted") return "INTERRUPTED";
  return undefined;
}

import type {
  ChatMessage,
  ContentBlock,
  ToolCompletion,
  ToolEvent,
  ToolNotificationBlock,
} from "../types/a2a";

/** Format status/phase text for display: surface phase or tool name, avoid "Calling model: unknown (X)". */
export function formatStatusPhaseText(raw: string): string {
  const phaseMatch = raw.match(/Calling model: unknown \((.+)\)/);
  const toolMatch = raw.match(/Invoking tool: (.+)/);
  return phaseMatch ? phaseMatch[1]! : toolMatch ? `Tool: ${toolMatch[1]}` : raw;
}

export function deriveToolStatus(block: ToolNotificationBlock): string {
  if (block.completion === "DONE") return "Done";
  if (block.completion === "INPUT_REQUIRED") return "Input required";
  if (block.completion === "INTERRUPTED") return "Interrupted";
  const last = block.events[block.events.length - 1];
  if (!last) return "Running";
  switch (last.kind) {
    case "assistant_thinking":
      return "Thinking…";
    case "assistant_tool_use":
      return "Using tool";
    case "assistant_text":
      return "Writing…";
    case "terminal_result":
      return "Complete";
    case "system_notice": {
      const phase = last.subtype ? formatStatusPhaseText(last.subtype) : "System";
      const model = typeof last.model === "string" ? last.model : undefined;
      return model ? `${phase} · ${model}` : phase;
    }
    default:
      return "Running";
  }
}

/** Blocks for a given tool (baseName or "baseName 2", "baseName 3", …). */
export function isToolBlockForBase(b: ContentBlock, baseName: string): b is ToolNotificationBlock {
  return (
    b.type === "tool" &&
    (b.toolName === baseName ||
      b.toolName === `${baseName} 2` ||
      b.toolName.startsWith(`${baseName} `))
  );
}

export function findOrCreateToolBlock(msg: ChatMessage, toolName: string): ToolNotificationBlock {
  const blocks = msg.contentBlocks!;
  const existing = blocks.find(
    (b): b is ToolNotificationBlock => b.type === "tool" && b.toolName === toolName,
  );
  if (existing) return existing;
  const block: ToolNotificationBlock = {
    type: "tool",
    toolName,
    status: "Running",
    events: [],
  };
  blocks.push(block);
  return block;
}

export type ToolAppendMode = "start" | "progress" | "end";

/** When a tool invocation is complete, only a "start" mode may create a new numbered section. */
export function getOrCreateToolBlockForAppend(
  msg: ChatMessage,
  baseName: string,
  mode: ToolAppendMode,
): ToolNotificationBlock {
  const blocks = msg.contentBlocks!;
  const toolBlocks = blocks.filter((b): b is ToolNotificationBlock =>
    isToolBlockForBase(b, baseName),
  );
  const lastTool = toolBlocks[toolBlocks.length - 1];
  if (!lastTool) return findOrCreateToolBlock(msg, baseName);
  const lastComplete = lastTool.completion === "DONE" || lastTool.completion === "INTERRUPTED";
  const needNewBlock = lastComplete && mode === "start";
  if (needNewBlock) {
    const name =
      toolBlocks.length === 1 ? `${baseName} 2` : `${baseName} ${toolBlocks.length + 1}`;
    return findOrCreateToolBlock(msg, name);
  }
  return lastTool;
}

export function detectToolAppendMode(
  events: ToolEvent[],
  completion: ToolCompletion | undefined,
): ToolAppendMode {
  const hasStartEvent = events.some((ev) => {
    if (ev.kind === "assistant_tool_use") return true;
    if (ev.kind !== "system_notice") return false;
    const marker = `${ev.subtype ?? ""}${ev.text ?? ""}`.toLowerCase();
    return marker.includes("session step: open");
  });
  if (hasStartEvent) return "start";
  if (completion === "DONE" || completion === "INTERRUPTED") return "end";
  return "progress";
}

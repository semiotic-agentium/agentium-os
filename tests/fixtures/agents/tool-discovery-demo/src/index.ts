/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: tool-discovery-demo
 * Uses system/discover_tools to find tools by query and respond.
 * Flow: ChooseDiscoverToolsQuery → Open → Act (Send | Read) → Continue (Send | Read | Finish).
 * Blocking Send returns `result: { tools:[...], done:bool }`; a Continue hop must emit Finish once the list is in the archive.
 */

function formatToolsList(tools: Array<{ name: string; bundle: string; description: string }>): string {
  if (tools.length === 0) return "No tools found for that query.";
  return tools
    .map((t) => `- **${t.name}** (${t.bundle}): ${t.description}`)
    .join("\n");
}

function extractTools(step: unknown): Array<{ name: string; bundle: string; description: string }> | null {
  if (!step || typeof step !== "object") return null;
  const s = step as Record<string, unknown>;

  // New path: blocking Send puts raw discover_tools JSON in `result`
  const rawResult = s.result as Record<string, unknown> | undefined;
  if (rawResult && Array.isArray(rawResult.tools)) {
    return rawResult.tools as Array<{ name: string; bundle: string; description: string }>;
  }

  // Legacy path: tools directly on the step or in output
  if (Array.isArray(s.tools)) {
    return s.tools as Array<{ name: string; bundle: string; description: string }>;
  }
  const out = s.output as Record<string, unknown> | undefined;
  if (out && Array.isArray(out.tools)) {
    return out.tools as Array<{ name: string; bundle: string; description: string }>;
  }
  return null;
}

__chat_register({
  run: async (ctx) => {
    const text = ctx.text?.trim() || "";
    if (!text) return { message: "Send a message like: what tools do you have for Notion? or tell me about ClickUp." };

    const run = await runGeneratedStepExecutor("ChooseDiscoverToolsQuery", { user_message: text }, { max_steps: 6 });

    // Check run.last first (blocking Send result is the primary source)
    const last = run.last as unknown;
    const lastTools = extractTools(last);
    if (lastTools) {
      return { message: `Here are the tools I found:\n\n${formatToolsList(lastTools)}` };
    }

    // Fallback: scan steps in reverse
    for (const step of [...run.steps].reverse()) {
      const tools = extractTools(step);
      if (tools) {
        return { message: `Here are the tools I found:\n\n${formatToolsList(tools)}` };
      }
    }

    return { message: "No tools found for that query." };
  },
});

/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: tool-discovery-demo
 * Uses system/discover_tools to find tools by query and respond.
 * When the user asks about "Notion", "ClickUp", "calculate", etc., we call
 * ChooseDiscoverToolsQuery which returns a session plan; the runtime executes it
 * and returns the discover_tools output (tools list). We format that as the reply.
 */

function formatToolsList(tools: Array<{ name: string; bundle: string; description: string }>): string {
  if (tools.length === 0) return "No tools found for that query.";
  return tools
    .map((t) => `- **${t.name}** (${t.bundle}): ${t.description}`)
    .join("\n");
}

__chat_register({
  run: async (ctx) => {
    const text = ctx.text?.trim() || "";
    if (!text) return { message: "Send a message like: what tools do you have for Notion? or tell me about ClickUp." };

    // Use the step-executor loop: Open → Send (with query) → Read (get tools) → Finish
    const run = await runGeneratedStepExecutor("ChooseDiscoverToolsQuery", { user_message: text }, { max_steps: 6 });

    // Scan steps in reverse for the Read result that contains tools
    let tools: Array<{ name: string; bundle: string; description: string }> | undefined;
    let done = true;
    for (const step of [...run.steps].reverse()) {
      const s = step as { tools?: Array<{ name: string; bundle: string; description: string }>; done?: boolean };
      if (Array.isArray(s.tools)) {
        tools = s.tools;
        done = typeof s.done === "boolean" ? s.done : true;
        break;
      }
    }

    if (Array.isArray(tools)) {
      const message = done !== false
        ? `Here are the tools I found:\n\n${formatToolsList(tools)}`
        : `Tools (partial):\n\n${formatToolsList(tools)}`;
      return { message };
    }
    return { message: "No tools found for that query." };
  },
});

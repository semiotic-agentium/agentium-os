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

    const result = await ChooseDiscoverToolsQuery({ user_message: text });
    if (result == null || typeof result !== "object") {
      return { error: "Tool discovery returned no result." };
    }
    // Runtime executes the plan and returns the tool output: { tools, done }
    const tools = (result as { tools?: Array<{ name: string; bundle: string; description: string }> }).tools;
    const done = (result as { done?: boolean }).done;
    if (Array.isArray(tools)) {
      const message = done !== false
        ? `Here are the tools I found:\n\n${formatToolsList(tools)}`
        : `Tools (partial):\n\n${formatToolsList(tools)}`;
      return { message };
    }
    return { message: "I used tool discovery but got no tool list back. Try asking e.g. 'what can you do with Notion?' or 'clickup'." };
  },
});

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

const server = new McpServer({ name: "ts-echo", version: "0.1.0" });
server.registerTool(
  "ts-echo",
  {
    description: "Echo the input message (e2e fixture).",
    inputSchema: { message: z.string() },
    outputSchema: z.object({ echoed: z.string() }),
    annotations: { readOnlyHint: true, idempotentHint: true },
  },
  async (input) => {
    const output = { echoed: input.message };
    return {
      content: [{ type: "text", text: JSON.stringify(output) }],
      structuredContent: output,
    };
  },
);

await server.connect(new StdioServerTransport());

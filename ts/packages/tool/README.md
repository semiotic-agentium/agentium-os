# @karakara/tool

`@karakara/tool` is optional TypeScript sugar for the official MCP SDK. It
creates an `McpServer`, registers one typed tool, and connects it to MCP stdio.
The process still speaks ordinary MCP. You can skip this package and use
`@modelcontextprotocol/sdk` directly.

```ts
import { defineTool, serve, z } from "@karakara/tool";

const server = defineTool(
  {
    name: "weather",
    version: "1.0.0",
    description: "Current temperature for a coordinate.",
    sideEffect: "read",
    inputSchema: {
      latitude: z.number(),
      longitude: z.number(),
    },
    outputSchema: z.object({ temperatureC: z.number() }),
  },
  async (input) => {
    const response = await fetch(`https://example.test/weather?lat=${input.latitude}&lon=${input.longitude}`);
    return { temperatureC: (await response.json()).temperatureC };
  },
);

await serve(server);
```

`defineTool` maps `sideEffect` and `idempotent` to MCP annotations. It puts
Karakara's descriptor fields in the MCP annotation namespace. `karak tool
build` starts the server, calls `tools/list`, and writes the resulting
`tools.json`.

Put toolset-wide network and secret requests in the project's
`karakara.toml`. They apply to the server process, not one MCP tool.

## Build

```bash
npm install
npm run build
```

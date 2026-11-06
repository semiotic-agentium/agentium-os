# @karakara/tool

Write one typed function. TypeBox supplies both its TypeScript types and the JSON
Schema returned by MCP `tools/list`. `karak tool build` uses that response to
generate `tools.json`, descriptor digests, and `tool-manifest.json`.

```ts
import { defineTool, Type } from "@karakara/tool";

const Input = Type.Object({
  latitude: Type.Number({ minimum: -90, maximum: 90 }),
  longitude: Type.Number({ minimum: -180, maximum: 180 }),
});

const Output = Type.Object({
  temperatureC: Type.Number(),
});

export default defineTool(
  {
    name: "weather/meteo",
    version: "1.0.0",
    description: "Current temperature for a coordinate.",
    sideEffect: "read",
    input: Input,
    output: Output,
    net: ["api.open-meteo.com:443"],
  },
  async (input, ctx) => {
    const url = new URL("https://api.open-meteo.com/v1/forecast");
    url.searchParams.set("latitude", String(input.latitude));
    url.searchParams.set("longitude", String(input.longitude));
    url.searchParams.set("current", "temperature_2m");

    const response = await ctx.fetch(url);
    if (!response.ok) {
      throw ctx.error(
        "transient",
        `Open-Meteo returned HTTP ${response.status}`,
      );
    }
    const body = (await response.json()) as {
      current: { temperature_2m: number };
    };
    return { temperatureC: body.current.temperature_2m };
  },
);
```

`defineTool` owns runtime startup and transport. It does not expose an MCP
server, connection lifecycle, or transport objects. Authors define the contract
and handler; Karakara handles the rest. Plain MCP servers remain compatible as
a separate, lower-level authoring path.

The SDK validates input before calling the handler and validates output before
returning it. Use `ctx.error` to classify failures and `ctx.signal` for
cancellable work.

## Network access

Omit `net` for tools that do not need the network. Their Sandbox workers receive
no external egress.

`net` requests outbound TCP destinations as `host:port`. The Sandbox host owns
and enforces those grants. `ctx.fetch` is the HTTP and HTTPS helper: it checks
the initial and final destination against `net` for fast feedback, while the
host remains the security boundary for redirects and every connection.

The host grant is not limited to HTTP. Future capability-aware helpers can use
the same destination model for protocols such as gRPC. Long-lived consumers,
such as Kafka subscriptions, belong in the Service tier rather than a
request/response Sandbox tool.

## Build

```bash
npm install
npm run build
karak tool build
karak tool test --input '{"latitude":52.52,"longitude":13.41}'
```

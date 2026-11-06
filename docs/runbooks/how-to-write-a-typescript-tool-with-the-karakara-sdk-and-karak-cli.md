# How to write a TypeScript tool with the Karakara SDK and `karak` CLI

This guide covers the complete TypeScript tool workflow: scaffold, implement,
build, test through the DevBackend, publish, and deploy an agent that uses the
tool.

## 1. Create a TypeScript tool

```bash
karak tool new --lang ts meteo --bundle weather
cd meteo
npm install
```

This creates:

```text
meteo/
├── package.json
├── tsconfig.json
├── src/tool.ts
├── README.md
└── .gitignore
```

The fully qualified tool name is `weather/meteo`.

The scaffold depends on `@karakara/tool@^0.2.0`. Until that package is
published to npm, install it from this checkout:

```bash
npm install --save /path/to/agentium-os/ts/packages/tool
```

## 2. Write the tool

Replace `src/tool.ts` with:

```ts
import { defineTool, Type } from "@karakara/tool";

const Input = Type.Object({
  latitude: Type.Number({
    minimum: -90,
    maximum: 90,
  }),
  longitude: Type.Number({
    minimum: -180,
    maximum: 180,
  }),
});

const Output = Type.Object({
  temperatureC: Type.Number(),
  observedAt: Type.String(),
});

export default defineTool(
  {
    name: "weather/meteo",
    version: "0.1.0",
    description: "Get the current temperature for a coordinate.",
    sideEffect: "read",
    input: Input,
    output: Output,

    // These become capability requests in the generated manifest.
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
      current: {
        time: string;
        temperature_2m: number;
      };
    };

    return {
      temperatureC: body.current.temperature_2m,
      observedAt: body.current.time,
    };
  },
);
```

TypeScript infers `input` and the handler return type from the TypeBox schemas.
You do not write a separate JSON Schema, MCP registration, or manifest.

`defineTool` starts the MCP stdio server automatically.

## 3. Build it

```bash
karak tool build .
```

The command compiles the TypeScript server, calls MCP `tools/list`, and
generates:

```text
dist/tool.js
tools.json
tool-manifest.json
tool.lock.json
```

`tools.json` contains the TypeBox-derived input and output schemas.
`tool-manifest.json` contains the toolset name, version, descriptor digest,
command, and requested network access.

For example:

```json
{
  "name": "weather",
  "version": "0.1.0",
  "command": ["node", "dist/tool.js"],
  "requested": {
    "net": [
      {
        "host": "api.open-meteo.com",
        "port": 443
      }
    ]
  }
}
```

## 4. Test through the DevBackend

```bash
karak tool test . \
  --input '{"latitude":52.52,"longitude":13.41}'
```

This does not call the handler directly. It:

1. Builds the `.ktool`.
2. Starts `baml-tool-supervisor` in development mode.
3. Loads the bundle over `protocol_v2`.
4. Starts the MCP worker.
5. Invokes `weather/meteo` through MCP.
6. Validates the returned value against `Output`.

Expected output:

```text
✓ ok
  output: {
    "temperatureC": 8.4,
    "observedAt": "2026-11-06T12:00"
  }
```

Bad input fails before the handler runs:

```bash
karak tool test . --input '{"latitude":"Berlin"}'
```

```text
✗ error
  class: "invalid_argument"
```

## 5. Use classified errors

The available classes are:

```ts
throw ctx.error("invalid_argument", "latitude is required");
throw ctx.error("transient", "upstream service is unavailable");
throw ctx.error("permission", "the account cannot read this resource");
throw ctx.error("configuration", "the API key is missing");
throw ctx.error("execution", "the response could not be processed");
```

These classifications cross MCP and the supervisor, so the runtime can
distinguish correctable input errors from retryable or fatal failures.

## 6. Declare and use secrets

Declare secret names in the tool specification:

```ts
export default defineTool(
  {
    name: "support/tickets",
    input: Input,
    output: Output,
    net: ["api.example.com:443"],
    secrets: {
      API_TOKEN: "egress-bound",
    },
  },
  async (input, ctx) => {
    const response = await ctx.fetch("https://api.example.com/tickets", {
      headers: {
        Authorization: `Bearer ${ctx.secrets.API_TOKEN}`,
      },
    });

    return await response.json();
  },
);
```

For an egress-bound secret, `ctx.secrets.API_TOKEN` is only a placeholder inside
the Sandbox. The host replaces it on an approved TLS connection to the bound
destination.

Secret values do not belong in the tool source or manifest.

## 7. Publish the tool

Start the local platform:

```bash
just runner
```

Then publish:

```bash
karak tool publish . \
  --repository-url http://127.0.0.1:18080/repository \
  --approved-by "$USER"
```

The CLI displays the requested capabilities before asking for approval:

```text
Toolset: weather
  tool:        weather/meteo
  side effect: Read
  network:
    - api.open-meteo.com:443
  secrets:     none
```

For non-interactive use:

```bash
karak tool publish . \
  --repository-url http://127.0.0.1:18080/repository \
  --approved-by ci \
  --yes
```

You can also use the top-level auto-detecting command:

```bash
karak publish \
  --path . \
  --repository-url http://127.0.0.1:18080/repository \
  --approved-by "$USER" \
  --yes
```

Publishing creates `dist/weather.ktool`, audits it, stores it
content-addressed, and records both the descriptor digest and executable tool
digest.

## 8. Add it to an agent

Add the tool name to the agent's `manifest.json`:

```json
{
  "version": "1.0.0",
  "name": "weather-agent",
  "entry_point": "src/index.ts",
  "signature": "weather-agent@1.0.0",
  "tools": ["weather/meteo@^0.1"]
}
```

The version range is optional:

```json
"tools": ["weather/meteo"]
```

The agent build resolves the approved tool and writes the exact descriptor and
tool digests into its lockfile.

## 9. Publish and deploy the agent

```bash
karak install agent \
  --path ./weather-agent \
  --repository-url http://127.0.0.1:18080/repository \
  --url http://127.0.0.1:18080 \
  --rationale "Deploy weather agent"
```

`karak install agent` publishes the agent and deploys it. Deployment fails if
`weather/meteo` is missing, unapproved, or does not match the pinned descriptor.

## 10. Complete command sequence

```bash
# Create
karak tool new --lang ts meteo --bundle weather
cd meteo
npm install

# Edit src/tool.ts

# Build and test
karak tool build .
karak tool test . \
  --input '{"latitude":52.52,"longitude":13.41}'

# Publish
karak tool publish . \
  --repository-url http://127.0.0.1:18080/repository \
  --approved-by "$USER" \
  --yes

# Add "weather/meteo@^0.1" to the agent manifest

# Publish and deploy the agent
karak install agent \
  --path ../weather-agent \
  --repository-url http://127.0.0.1:18080/repository \
  --url http://127.0.0.1:18080 \
  --rationale "Deploy weather agent"
```

The converted example is at
[`examples/external-tools/meteo-tool/src/tool.ts`](../../examples/external-tools/meteo-tool/src/tool.ts).

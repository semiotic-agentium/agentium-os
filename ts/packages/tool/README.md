# @karakara/tool

The blessed authoring SDK for Karakara external tools (v2). Write **one typed
async function**; the SDK derives the descriptor from your types and runs the
wire. No framing, no manifest, no schema file, no Dockerfile.

```ts
import { defineTool, Type, type Static } from "@karakara/tool";

const Input = Type.Object({ latitude: Type.Number(), longitude: Type.Number() });
const Output = Type.Object({ temperatureC: Type.Number() });

export default defineTool(
  {
    name: "weather/meteo",
    version: "1.0.0",
    description: "Current temperature for a coordinate.",
    sideEffect: "read",
    input: Input,
    output: Output,
    net: ["api.open-meteo.com:443"],
    secrets: { METEO_KEY: "egress_bound" },
  },
  async (input: Static<typeof Input>, ctx): Promise<Static<typeof Output>> => {
    const res = await ctx.fetch(
      `https://api.open-meteo.com/v1/forecast?latitude=${input.latitude}` +
        `&longitude=${input.longitude}&current=temperature_2m`,
    );
    if (!res.ok) throw ctx.error("transient", `upstream ${res.status}`);
    const body = (await res.json()) as any;
    return { temperatureC: body.current.temperature_2m };
  },
);
```

## What the SDK does for you

- **Descriptor** — `defineTool(...).descriptor()` emits the unified
  `tool-descriptor.json` (`-p1-spec.md` §P1.1), field-for-field identical to what
  static (Rust) and MCP tools produce, so parity is structural.
- **Input/output validation** — bad input is rejected as `invalid_argument`
  (LLM-correctable) before your handler runs; bad output is caught before it
  reaches the LLM.
- **Capability binding** — `ctx.fetch` only reaches the hosts you declared in
  `net`; `ctx.secrets.X` is a placeholder injected by the host on egress (you
  never see the plaintext).
- **Errors** — `ctx.error(class, msg)` maps to the host `ErrorClass` so the
  disposition (retry / tell-the-LLM / fatal) is right.

## Design

See `plans/external-tools-v2-ts-sdk.md`. Types use **TypeBox** (JSON-Schema
native, so the schema *is* the descriptor schema — no codegen, no drift).

## Build

```
npm install
npm run build
```

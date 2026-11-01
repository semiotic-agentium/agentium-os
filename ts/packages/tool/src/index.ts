// SPDX-License-Identifier: Apache-2.0
//
// @karakara/tool — blessed authoring SDK for Karakara external tools (v2).
//
// A tool is ONE typed async function. The SDK derives the unified
// ToolDescriptor from the spec + TypeBox types (so it cannot drift from the
// host's descriptor), restricts network/secret access to what was declared,
// and runs the §7 wire. Authors never write framing, a manifest, or a schema.
//
// See plans/external-tools-v2-ts-sdk.md and -p1-spec.md §P1.1.

import { Type, type Static, type TSchema } from "@sinclair/typebox";

export { Type, type Static, type TSchema };
export { serve, type Transport } from "./runtime.js";
export { stdioTransport } from "./stdio.js";

/** Mirrors the host `ErrorClass` (snake_case on the wire). */
export type ErrorClass =
  | "configuration"
  | "invalid_argument"
  | "transient"
  | "permission"
  | "execution";

/** Mirrors the host `SideEffect` (replaces the old read/write/delete access). */
export type SideEffect = "read" | "write" | "irreversible";

/** Mirrors the host `SecretBinding`. Default is egress-bound. */
export type SecretBinding = "egress_bound" | "reveal";

/** What the author declares. Everything else is derived. */
export interface ToolSpec<I extends TSchema, O extends TSchema> {
  /** Fully-qualified `bundle/local_name`. */
  name: string;
  version: string;
  description: string;
  sideEffect: SideEffect;
  input: I;
  output: O;
  /** Hosts the tool may reach, as `host:port`. Resolved to a CIDR grant. */
  net?: string[];
  /** Secret names → binding. Default egress-bound; value is never visible here. */
  secrets?: Record<string, SecretBinding>;
  /** Defaults to `sideEffect === "read"`. */
  idempotent?: boolean;
  /** Error classes the handler may raise. Defaults to the full set. */
  errors?: ErrorClass[];
}

/** Context handed to the handler. Capabilities are bound to the host grant. */
export interface ToolContext {
  /** `fetch`, but the host only permits the declared `net` hosts. */
  fetch: typeof fetch;
  /** Secret placeholders keyed by name; the real value is injected by the
   *  host on egress to the approved host — never readable here. */
  secrets: Record<string, string>;
  /** Produce a classified error mapped to the host disposition. */
  error: (cls: ErrorClass, message: string) => ToolError;
  /** Structured log → surfaces as an `Event`, never stdout framing. */
  log: (...args: unknown[]) => void;
  /** The host-enforced deadline for this invocation. */
  readonly deadlineMs: number;
  /** Aborts when the deadline passes; wired into `ctx.fetch`. */
  readonly signal: AbortSignal;
}

export type Handler<I extends TSchema, O extends TSchema> = (
  input: Static<I>,
  ctx: ToolContext,
) => Promise<Static<O>>;

/** A classified tool error. `throw ctx.error("transient", ...)`. */
export class ToolError extends Error {
  constructor(
    public readonly errorClass: ErrorClass,
    message: string,
  ) {
    super(message);
    this.name = "ToolError";
  }
}

/** One requested host:port in the descriptor. */
export interface HostPort {
  host: string;
  port: number;
}

/** The unified descriptor — field names match the host Rust struct exactly so
 *  `descriptor()` output is a valid `tool-descriptor.json`. */
export interface ToolDescriptor {
  descriptor_abi: number;
  name: string;
  version: string;
  description: string;
  input_schema: unknown;
  output_schema: unknown;
  error_classes: ErrorClass[];
  side_effect: SideEffect;
  idempotent: boolean;
  output_trust: "untrusted"; // external tool output is always untrusted
  requested: {
    net: HostPort[];
    secrets: { name: string; binding: SecretBinding }[];
  };
}

export const DESCRIPTOR_ABI = 1;

const ALL_ERROR_CLASSES: ErrorClass[] = [
  "configuration",
  "invalid_argument",
  "transient",
  "permission",
  "execution",
];

/** A built tool: carries the spec+handler, derives its descriptor, and serves
 *  the wire. The build step reads `.descriptor()`; the runtime calls `.serve()`. */
export interface Tool {
  readonly name: string;
  descriptor(): ToolDescriptor;
  /** Validate `input`, run the handler with a bound ctx, validate output. */
  run(input: unknown, ctx: ToolContext): Promise<unknown>;
  /** The declared spec, for the runtime to bind caps from. */
  readonly spec: ToolSpec<TSchema, TSchema>;
  readonly handler: Handler<TSchema, TSchema>;
}

function parseHostPort(entry: string): HostPort {
  const idx = entry.lastIndexOf(":");
  if (idx <= 0 || idx === entry.length - 1) {
    throw new Error(`net entry must be "host:port", got "${entry}"`);
  }
  const host = entry.slice(0, idx);
  const port = Number(entry.slice(idx + 1));
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`net entry has invalid port: "${entry}"`);
  }
  return { host, port };
}

/** Author entry point: declare a tool as one typed function. */
export function defineTool<I extends TSchema, O extends TSchema>(
  spec: ToolSpec<I, O>,
  handler: Handler<I, O>,
): Tool {
  const descriptor = (): ToolDescriptor => ({
    descriptor_abi: DESCRIPTOR_ABI,
    name: spec.name,
    version: spec.version,
    description: spec.description,
    input_schema: spec.input,
    output_schema: spec.output,
    error_classes: spec.errors ?? ALL_ERROR_CLASSES,
    side_effect: spec.sideEffect,
    idempotent: spec.idempotent ?? spec.sideEffect === "read",
    output_trust: "untrusted",
    requested: {
      net: (spec.net ?? []).map(parseHostPort),
      secrets: Object.entries(spec.secrets ?? {}).map(([name, binding]) => ({
        name,
        binding,
      })),
    },
  });

  return {
    name: spec.name,
    descriptor,
    spec: spec as unknown as ToolSpec<TSchema, TSchema>,
    handler: handler as unknown as Handler<TSchema, TSchema>,
    run: (input, ctx) => handler(input as Static<I>, ctx),
  };
}

// SPDX-License-Identifier: Apache-2.0

import { readFileSync } from "node:fs";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ErrorCode,
  ListToolsRequestSchema,
  McpError,
} from "@modelcontextprotocol/sdk/types.js";
import { Type, type Static, type TSchema } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";

export { Type };
export type { Static, TSchema };

export type SideEffect = "read" | "write" | "irreversible";
export type ToolErrorClass =
  | "invalid_argument"
  | "transient"
  | "permission"
  | "configuration"
  | "execution";
export type SecretBinding = "egress-bound" | "reveal";
export type SecretSpec = Readonly<Record<string, SecretBinding>>;

const ALL_ERROR_CLASSES: readonly ToolErrorClass[] = [
  "invalid_argument",
  "transient",
  "permission",
  "configuration",
  "execution",
];

export interface ToolSpec<
  I extends TSchema,
  O extends TSchema,
  S extends SecretSpec = Record<never, never>,
> {
  /** Fully qualified `toolset/tool` name. */
  name: string;
  /** Toolset version. Defaults to `0.1.0`. */
  version?: string;
  description?: string;
  input: I;
  output: O;
  sideEffect?: SideEffect;
  idempotent?: boolean;
  /** Outbound TCP destinations requested by this toolset, written as `host:port`. */
  net?: readonly string[];
  /** Named secret requests. Secret values never belong in source code. */
  secrets?: S;
}

export interface ToolContext<S extends SecretSpec = Record<never, never>> {
  /** HTTP(S) fetch with declaration checks. The host owns network enforcement. */
  fetch(input: string | URL | Request, init?: RequestInit): Promise<Response>;
  /** Create a classified error for `throw ctx.error(...)`. */
  error(errorClass: ToolErrorClass, message: string): Error;
  /** Emit a structured tool log event. */
  log(
    level: "debug" | "info" | "notice" | "warning" | "error",
    message: string,
  ): Promise<void>;
  /** Aborts when the host cancels the tool invocation. */
  readonly signal: AbortSignal;
  /** Egress placeholders or reveal values provided by the supervisor. */
  readonly secrets: Readonly<{ [K in keyof S]: string }>;
}

export type ToolHandler<
  I extends TSchema,
  O extends TSchema,
  S extends SecretSpec,
> = (input: Static<I>, ctx: ToolContext<S>) => Promise<Static<O>> | Static<O>;

class ToolExecutionError extends Error {
  readonly errorClass: ToolErrorClass;

  constructor(errorClass: ToolErrorClass, message: string) {
    super(message);
    this.name = "ToolExecutionError";
    this.errorClass = errorClass;
  }
}

interface ParsedTarget {
  host: string;
  port: number;
}

/**
 * Define one TypeBox-backed Karakara tool.
 *
 * The SDK owns runtime startup and transport. Authors provide only the tool
 * contract and business-logic handler.
 */
export function defineTool<
  I extends TSchema,
  O extends TSchema,
  S extends SecretSpec = Record<never, never>,
>(spec: ToolSpec<I, O, S>, handler: ToolHandler<I, O, S>): void {
  const { toolsetName, localName } = parseToolName(spec.name);
  const version = spec.version ?? "0.1.0";
  const description = spec.description ?? localName;
  const sideEffect = spec.sideEffect ?? "write";
  const idempotent = spec.idempotent ?? sideEffect === "read";
  const targets = (spec.net ?? []).map(parseTarget);
  const requested = {
    net: targets.map(({ host, port }) => ({ host, port })),
    secrets: Object.entries(spec.secrets ?? {}).map(([name, binding]) => ({
      name,
      binding: binding === "egress-bound" ? "egress_bound" : binding,
    })),
  };
  const errorClasses = [...ALL_ERROR_CLASSES];
  const server = new Server(
    { name: localName, version },
    { capabilities: { logging: {}, tools: {} } },
  );

  server.setRequestHandler(ListToolsRequestSchema, async () => ({
    tools: [
      {
        name: localName,
        description,
        inputSchema: spec.input as Record<string, unknown>,
        outputSchema: spec.output as Record<string, unknown>,
        annotations: {
          readOnlyHint: sideEffect === "read",
          destructiveHint: sideEffect === "irreversible",
          idempotentHint: idempotent,
          karakara: {
            descriptorAbi: 1,
            version,
            outputSchema: spec.output,
            errorClasses,
            sideEffect,
            idempotent,
            outputTrust: "untrusted",
          },
        },
        _meta: {
          "karakara/toolset": {
            name: toolsetName,
            version,
            capabilities: requested,
          },
        },
      },
    ],
  }));

  server.setRequestHandler(CallToolRequestSchema, async (request, extra) => {
    if (request.params.name !== localName) {
      throw new McpError(
        ErrorCode.MethodNotFound,
        `unknown tool '${request.params.name}'`,
      );
    }
    const input = request.params.arguments ?? {};
    if (!Value.Check(spec.input, input)) {
      throw new McpError(
        ErrorCode.InvalidParams,
        `input failed schema validation: ${validationMessage(spec.input, input)}`,
      );
    }

    const ctx = createContext(server, targets, spec.secrets, extra.signal);
    try {
      const output = await handler(input as Static<I>, ctx);
      if (!Value.Check(spec.output, output)) {
        throw new ToolExecutionError(
          "execution",
          `handler output failed schema validation: ${validationMessage(spec.output, output)}`,
        );
      }
      return {
        content: [{ type: "text" as const, text: JSON.stringify(output) }],
        ...(isRecord(output) ? { structuredContent: output } : {}),
      };
    } catch (error) {
      const classified = extra.signal.aborted
        ? new ToolExecutionError(
            "transient",
            errorMessage(extra.signal.reason ?? error),
          )
        : classifyError(error);
      return {
        content: [{ type: "text" as const, text: classified.message }],
        isError: true,
        _meta: { "karakara/errorClass": classified.errorClass },
      };
    }
  });

  server.connect(new StdioServerTransport()).catch((error: unknown) => {
    console.error(`@karakara/tool failed to start: ${errorMessage(error)}`);
    process.exitCode = 1;
  });
}

function createContext<S extends SecretSpec>(
  server: Server,
  targets: readonly ParsedTarget[],
  secrets: S | undefined,
  signal: AbortSignal,
): ToolContext<S> {
  const allowed = new Set(targets.map(({ host, port }) => `${host}:${port}`));
  const revealFds = revealFdTable();
  const secretValues = Object.fromEntries(
    Object.keys(secrets ?? {}).map((name) => {
      const fd = revealFds[name];
      const value =
        fd === undefined
          ? (process.env[name] ?? `{{${name}}}`)
          : readFileSync(`/proc/self/fd/${fd}`, "utf8");
      return [name, value];
    }),
  ) as { [K in keyof S]: string };

  return {
    signal,
    secrets: secretValues,
    error: (errorClass, message) => new ToolExecutionError(errorClass, message),
    log: async (level, message) => {
      await server.sendLoggingMessage({ level, data: message });
    },
    fetch: async (input, init = {}) => {
      assertFetchDestination(requestUrl(input), allowed);
      const requestSignal = combineSignals(signal, init.signal);
      const response = await globalThis.fetch(input, {
        ...init,
        signal: requestSignal,
      });
      if (response.url) {
        assertFetchDestination(new URL(response.url), allowed);
      }
      return response;
    },
  };
}

function combineSignals(
  primary: AbortSignal,
  secondary: AbortSignal | null | undefined,
): AbortSignal {
  if (!secondary) return primary;
  if (primary.aborted) return primary;
  if (secondary.aborted) return secondary;
  return AbortSignal.any([primary, secondary]);
}

function parseToolName(name: string): {
  toolsetName: string;
  localName: string;
} {
  const parts = name.split("/");
  if (parts.length !== 2 || parts.some((part) => part.length === 0)) {
    throw new TypeError(`tool name '${name}' must use the form 'toolset/tool'`);
  }
  return { toolsetName: parts[0]!, localName: parts[1]! };
}

function parseTarget(value: string): ParsedTarget {
  const separator = value.lastIndexOf(":");
  const host = value.slice(0, separator).trim().toLowerCase();
  const port = Number(value.slice(separator + 1));
  if (
    separator <= 0 ||
    !host ||
    !Number.isInteger(port) ||
    port < 1 ||
    port > 65535
  ) {
    throw new TypeError(
      `network destination '${value}' must use the form 'host:port'`,
    );
  }
  return { host, port };
}

function revealFdTable(): Record<string, number> {
  const raw = process.env.KARAKARA_REVEAL_FDS;
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!isRecord(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        (entry): entry is [string, number] =>
          Number.isInteger(entry[1]) && (entry[1] as number) >= 0,
      ),
    );
  } catch {
    return {};
  }
}

function requestUrl(input: string | URL | Request): URL {
  if (input instanceof URL) return input;
  if (typeof input === "string") return new URL(input);
  return new URL(input.url);
}

function assertFetchDestination(url: URL, allowed: ReadonlySet<string>): void {
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new ToolExecutionError(
      "permission",
      `ctx.fetch supports HTTP and HTTPS, not '${url.protocol}'`,
    );
  }
  const port = url.port
    ? Number(url.port)
    : url.protocol === "https:"
      ? 443
      : 80;
  const destination = `${url.hostname.toLowerCase()}:${port}`;
  if (!allowed.has(destination)) {
    throw new ToolExecutionError(
      "permission",
      `network destination '${destination}' is not declared in this tool's net list`,
    );
  }
}

function validationMessage(schema: TSchema, value: unknown): string {
  return [...Value.Errors(schema, value)]
    .slice(0, 3)
    .map((error) => `${error.path || "/"} ${error.message}`)
    .join("; ");
}

function classifyError(error: unknown): ToolExecutionError {
  if (error instanceof ToolExecutionError) return error;
  if (error instanceof Error && error.name === "AbortError") {
    return new ToolExecutionError("transient", error.message);
  }
  return new ToolExecutionError("execution", errorMessage(error));
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

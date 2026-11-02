// SPDX-License-Identifier: Apache-2.0

/**
 * Small conveniences over the official MCP TypeScript SDK.
 *
 * `defineTool` creates an `McpServer` and registers one tool. It does not add a
 * Karakara transport or a second worker protocol. The resulting server speaks
 * ordinary MCP stdio and can run anywhere an MCP server can run.
 */

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { zodToJsonSchema } from "zod-to-json-schema";

export { McpServer, StdioServerTransport, z };

export type SideEffect = "read" | "write" | "irreversible";

type InputShape = z.ZodRawShape;
type OutputSchema = z.ZodTypeAny;

export interface ToolSpec<I extends InputShape, O extends OutputSchema> {
  /** MCP-local tool name. `karak tool build` prefixes it with the toolset name. */
  name: string;
  /** Included in Karakara's MCP annotation and descriptor digest. */
  version: string;
  description: string;
  inputSchema: I;
  outputSchema: O;
  sideEffect?: SideEffect;
  idempotent?: boolean;
}

export type ToolHandler<I extends InputShape, O extends OutputSchema> = (
  input: z.infer<z.ZodObject<I>>,
) => Promise<z.infer<O>> | z.infer<O>;

/**
 * Register one typed tool on a fresh official MCP server.
 *
 * The standard annotations carry the MCP-native read-only, destructive, and
 * idempotent hints. Karakara-specific descriptor fields live in the permitted
 * annotations namespace, so an ordinary MCP client can ignore them.
 */
export function defineTool<I extends InputShape, O extends OutputSchema>(
  spec: ToolSpec<I, O>,
  handler: ToolHandler<I, O>,
): McpServer {
  const sideEffect = spec.sideEffect ?? "write";
  const idempotent = spec.idempotent ?? sideEffect === "read";
  const server = new McpServer({ name: spec.name, version: spec.version });

  const callback = async (input: unknown) => {
    try {
      const output = await handler(input as z.infer<z.ZodObject<I>>);
      return {
        content: [{ type: "text", text: JSON.stringify(output) }],
        structuredContent: output as Record<string, unknown>,
      };
    } catch (error) {
      return {
        content: [{ type: "text", text: errorMessage(error) }],
        isError: true,
      };
    }
  };

  server.registerTool(
    spec.name,
    {
      description: spec.description,
      inputSchema: spec.inputSchema,
      outputSchema: spec.outputSchema,
      annotations: {
        readOnlyHint: sideEffect === "read",
        destructiveHint: sideEffect === "irreversible",
        idempotentHint: idempotent,
        karakara: {
          descriptorAbi: 1,
          version: spec.version,
          outputSchema: zodSchema(spec.outputSchema),
          errorClasses: ["execution"],
          sideEffect,
          idempotent,
          outputTrust: "untrusted",
        },
      } as never,
    },
    callback as never,
  );

  return server;
}

/** Connect an MCP server to the official stdio transport. */
export async function serve(server: McpServer): Promise<void> {
  await server.connect(new StdioServerTransport());
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function zodSchema(schema: z.ZodTypeAny): unknown {
  return zodToJsonSchema(schema);
}

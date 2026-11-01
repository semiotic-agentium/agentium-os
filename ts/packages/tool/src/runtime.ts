// SPDX-License-Identifier: Apache-2.0
//
// Runtime: drive the §7 wire for one tool worker. Runs inside the tool process
// (node runtime image in prod; the DevBackend child in `karak tool test`). The
// guest supervisor handles hello/load_tool + bundle fetch; this loop handles
// the per-invocation messages for the worker: validate input against the tool's
// schema, run the handler with a capability-bound ctx, validate output, frame
// the result. Transport is injected so prod (vsock) and dev (stdio) share this
// exact code path.

import { Value } from "@sinclair/typebox/value";

import {
  ToolError,
  type ErrorClass,
  type Tool,
  type ToolContext,
} from "./index.js";

/** Host→worker messages this loop handles (subset of the full wire). */
type HostMessage =
  | {
      t: "invoke";
      request_id: number;
      tool_digest: string;
      input: unknown;
      deadline_ms: number;
    }
  | { t: "cancel"; request_id: number }
  | { t: "secret"; request_id: number; name: string; value: string };

/** Worker→host messages this loop emits. */
type GuestMessage =
  | {
      t: "result";
      request_id: number;
      ok: boolean;
      output?: unknown;
      error?: string;
      error_class: ErrorClass;
      truncated: boolean;
    }
  | { t: "event"; request_id: number; kind: string; payload: unknown };

/** Bidirectional framed-message transport. The DevBackend and the prod vsock
 *  backend each provide one; the loop is transport-agnostic. */
export interface Transport {
  /** Next host message, or null at clean EOF. */
  recv(): Promise<HostMessage | null>;
  send(msg: GuestMessage): Promise<void>;
  /** Build the capability-bound ctx for one invocation (fetch restricted to
   *  declared hosts; secret placeholders). Supplied by the backend because the
   *  enforcement lives outside the tool. */
  contextFor(request_id: number, deadline_ms: number): ToolContext;
}

const classOf = (e: unknown): ErrorClass =>
  e instanceof ToolError ? e.errorClass : "execution";

const messageOf = (e: unknown): string =>
  e instanceof Error ? e.message : String(e);

/** Serve one tool over a transport until the host closes the channel. */
export async function serve(tool: Tool, transport: Transport): Promise<void> {
  for (;;) {
    const msg = await transport.recv();
    if (msg === null) return;
    if (msg.t !== "invoke") continue; // cancel/secret handled by the backend/ctx

    const { request_id, input, deadline_ms } = msg;

    // Input validation is the SDK's job, so a bad arg is invalid_argument
    // (LLM-correctable) without any author code.
    if (!Value.Check(tool.spec.input, input)) {
      const first = [...Value.Errors(tool.spec.input, input)][0];
      await transport.send({
        t: "result",
        request_id,
        ok: false,
        error: first ? `${first.path} ${first.message}` : "input failed schema",
        error_class: "invalid_argument",
        truncated: false,
      });
      continue;
    }

    const ctx = transport.contextFor(request_id, deadline_ms);
    try {
      const output = await tool.run(input, ctx);
      // Output validation catches tool bugs before bad data reaches the LLM.
      if (!Value.Check(tool.spec.output, output)) {
        throw new ToolError("execution", "tool output failed its own schema");
      }
      await transport.send({
        t: "result",
        request_id,
        ok: true,
        output,
        error_class: "execution",
        truncated: false,
      });
    } catch (e) {
      await transport.send({
        t: "result",
        request_id,
        ok: false,
        error: messageOf(e),
        error_class: classOf(e),
        truncated: false,
      });
    }
  }
}

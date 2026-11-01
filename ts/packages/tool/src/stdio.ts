// SPDX-License-Identifier: Apache-2.0
//
// Stdio transport for the DevBackend (`karak tool test`): length-prefixed JSON
// frames over process stdin/stdout — [u32 BE len][utf-8 JSON]. Same framing the
// host uses, so the dev path exercises the real serve() loop. In prod the vsock
// backend provides an equivalent Transport; this one is for local testing.

import { ToolError, type ErrorClass, type ToolContext } from "./index.js";
import type { Transport } from "./runtime.js";

const HEADER = 4;

// Minimal host messages this transport decodes (see protocol_v2).
type HostMessage =
  | { t: "invoke"; request_id: number; tool_digest: string; input: unknown; deadline_ms: number }
  | { t: "cancel"; request_id: number }
  | { t: "secret"; request_id: number; name: string; value: string };

type GuestMessage = Parameters<Transport["send"]>[0];

export function stdioTransport(): Transport {
  const inStream = process.stdin;
  const outStream = process.stdout;

  let buffer = Buffer.alloc(0);
  let ended = false;
  const waiters: ((m: HostMessage | null) => void)[] = [];

  const tryDeliver = () => {
    while (waiters.length > 0) {
      if (buffer.length >= HEADER) {
        const len = buffer.readUInt32BE(0);
        if (buffer.length >= HEADER + len) {
          const json = buffer.subarray(HEADER, HEADER + len).toString("utf8");
          buffer = buffer.subarray(HEADER + len);
          const resolve = waiters.shift()!;
          resolve(JSON.parse(json) as HostMessage);
          continue;
        }
      }
      if (ended) {
        const resolve = waiters.shift()!;
        resolve(null);
        continue;
      }
      break;
    }
  };

  inStream.on("data", (chunk: Buffer) => {
    buffer = Buffer.concat([buffer, chunk]);
    tryDeliver();
  });
  inStream.on("end", () => {
    ended = true;
    tryDeliver();
  });

  const recv = (): Promise<HostMessage | null> =>
    new Promise((resolve) => {
      waiters.push(resolve);
      tryDeliver();
    });

  const send = (msg: GuestMessage): Promise<void> =>
    new Promise((resolve) => {
      const body = Buffer.from(JSON.stringify(msg), "utf8");
      const frame = Buffer.alloc(HEADER + body.length);
      frame.writeUInt32BE(body.length, 0);
      body.copy(frame, HEADER);
      outStream.write(frame, () => resolve());
    });

  const contextFor = (_request_id: number, deadline_ms: number): ToolContext => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), deadline_ms);
    if (typeof timer === "object" && timer && "unref" in timer) {
      (timer as { unref: () => void }).unref();
    }
    return {
      // Dev: unrestricted fetch. In prod the host restricts egress to the
      // declared hosts and injects secrets; the restriction lives outside
      // the tool, so the handler code is identical.
      fetch: (globalThis.fetch as typeof fetch),
      secrets: {},
      error: (cls: ErrorClass, message: string) => new ToolError(cls, message),
      log: (...args: unknown[]) =>
        process.stderr.write(args.map(String).join(" ") + "\n"),
      deadlineMs: deadline_ms,
      signal: controller.signal,
    };
  };

  return { recv, send, contextFor };
}

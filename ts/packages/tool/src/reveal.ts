// SPDX-License-Identifier: Apache-2.0
//
// Reveal-tier secret transport (external tools v2, P4.7).
//
// Egress-bound secrets (the default) never enter the tool process. A reveal
// secret is the explicitly approved exception: the guest supervisor writes the
// value into a sealed memfd and hands the worker a routing table of
// `name -> fd number` in KARAKARA_REVEAL_FDS. The value is never in argv, env
// values, or the invoke payload. Read it once, then zero the buffer.

import fs from "node:fs";

import type { ToolContext } from "./index.js";
import type { Transport } from "./runtime.js";
import { stdioTransport } from "./stdio.js";

const ENV_TABLE = "KARAKARA_REVEAL_FDS";
const MAX_REVEAL_BYTES = 64 * 1024;

function revealTable(): Record<string, number> {
  const raw = process.env[ENV_TABLE];
  if (!raw) return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return {};
    }
    const table: Record<string, number> = {};
    for (const [name, value] of Object.entries(parsed)) {
      if (Number.isInteger(value) && (value as number) >= 0) {
        table[name] = value as number;
      }
    }
    return table;
  } catch {
    return {};
  }
}

function readSecret(fd: number): string {
  const buffer = Buffer.alloc(MAX_REVEAL_BYTES);
  let offset = 0;
  try {
    while (offset < buffer.length) {
      const read = fs.readSync(fd, buffer, offset, buffer.length - offset, offset);
      if (read === 0) break;
      offset += read;
    }
    return buffer.subarray(0, offset).toString("utf8");
  } finally {
    // Do not leave plaintext in a reusable buffer.
    buffer.fill(0);
  }
}

/** Wrap a transport so `ctx.secrets` resolves reveal secrets from inherited
 *  fds on first access. Defaults to the stdio DevBackend transport. */
export function revealTransport(inner: Transport = stdioTransport()): Transport {
  const table = revealTable();
  const cache = new Map<string, string>();

  const secret = (name: string): string => {
    const cached = cache.get(name);
    if (cached !== undefined) return cached;
    const fd = table[name];
    if (fd === undefined) {
      throw new Error(`reveal secret '${name}' was not delivered to this worker`);
    }
    const value = readSecret(fd);
    cache.set(name, value);
    return value;
  };

  return {
    recv: () => inner.recv(),
    send: (message) => inner.send(message),
    contextFor: (requestId: number, deadlineMs: number): ToolContext => {
      const context = inner.contextFor(requestId, deadlineMs);
      for (const name of Object.keys(table)) {
        Object.defineProperty(context.secrets, name, {
          enumerable: true,
          configurable: false,
          get: () => secret(name),
        });
      }
      return context;
    },
  };
}

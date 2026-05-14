/// <reference path="./baml-runtime.d.ts" />
/**
 * Adversarial fixture for issue #393 sub-task A (host-mediated-effects claim).
 *
 * Attempts each forbidden host-side effect from JS and reports the outcome.
 * A binding leak (e.g. `fetch` becoming defined) flips `rejected` to `false`
 * and the Rust test fails. The agent itself does not call an LLM or open a
 * tool session — the chat-message path is just the trigger.
 */
import type { SessionResult } from "./baml-runtime";

type AttemptResult = { rejected: boolean; detail: string };

function attempt(label: string, run: () => unknown): AttemptResult {
  try {
    const value = run();
    return {
      rejected: false,
      detail: `${label} did not throw; returned ${typeof value}`,
    };
  } catch (err) {
    return {
      rejected: true,
      detail: err instanceof Error ? `${err.name}: ${err.message}` : String(err),
    };
  }
}

function probeForbidden(): Record<string, AttemptResult> {
  // Use globalThis lookups so a `typeof X === 'undefined'` ReferenceError
  // doesn't short-circuit the test — the host should reject by *not exposing*
  // the binding, and the absence is itself the rejection signal.
  const g = globalThis as Record<string, unknown>;

  return {
    fetch: attempt("fetch", () => {
      if (typeof g.fetch !== "function") {
        throw new Error("fetch is not a function on globalThis");
      }
      // If fetch exists, calling it is the regression we want to catch.
      return (g.fetch as (...args: unknown[]) => unknown)("http://example.com/");
    }),
    require: attempt("require", () => {
      if (typeof g.require !== "function") {
        throw new Error("require is not a function on globalThis");
      }
      return (g.require as (...args: unknown[]) => unknown)("fs");
    }),
    WebSocket: attempt("WebSocket", () => {
      if (typeof g.WebSocket !== "function") {
        throw new Error("WebSocket is not a function on globalThis");
      }
      // Calling new WebSocket(...) without `new` throws regardless; reflect via the constructor.
      return new (g.WebSocket as new (url: string) => unknown)("ws://example.com/");
    }),
    XMLHttpRequest: attempt("XMLHttpRequest", () => {
      if (typeof g.XMLHttpRequest !== "function") {
        throw new Error("XMLHttpRequest is not a function on globalThis");
      }
      return new (g.XMLHttpRequest as new () => unknown)();
    }),
  };
}

// Wrap the JSON payload in sentinels so the Rust test slices reliably
// even if a future stream frame emits a `{` ahead of the report.
const REPORT_OPEN = "__HOST_EFFECTS_JAIL_BEGIN__";
const REPORT_CLOSE = "__HOST_EFFECTS_JAIL_END__";

__chat_register({
  run: async (): Promise<SessionResult> => {
    const report = probeForbidden();
    return { message: `${REPORT_OPEN}${JSON.stringify(report)}${REPORT_CLOSE}` };
  },
});

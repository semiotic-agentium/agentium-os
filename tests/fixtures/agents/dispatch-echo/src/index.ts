/// <reference path="./baml-runtime.d.ts" />
import type {
  DispatchRunContext,
  HostDispatchAck,
  HostDispatchRequest,
  SessionResult,
} from "./baml-runtime";

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(readInput?: Record<string, unknown>): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

type CallbackCommand = {
  continuation: "detached" | "resume_current_task";
  token: string;
};

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function parseCallbackCommand(text: string | undefined): CallbackCommand | null {
  const trimmed = (text || "").trim();
  const match = /^schedule-callback\s+(detached|resume_current_task)\s+([A-Za-z0-9_-]+)$/.exec(
    trimmed,
  );
  if (!match) return null;
  return {
    continuation: match[1] as CallbackCommand["continuation"],
    token: match[2],
  };
}

async function runSingleSendSession(
  toolName: string,
  openInput: Record<string, unknown>,
  sendInput: Record<string, unknown>,
): Promise<unknown> {
  let toolSession: ToolSessionHandle | null = null;
  try {
    toolSession = await openToolSession(toolName, openInput);
    await toolSession.send(sendInput);
    for (let step = 0; step < 8; step += 1) {
      const next = await toolSession.continue();
      const nextObj = isObject(next) ? next : null;
      const status =
        nextObj && typeof nextObj.status === "string" ? nextObj.status.toLowerCase() : null;
      if (status === "streaming") {
        continue;
      }
      if (status === "error") {
        const errorMessage =
          nextObj &&
          isObject(nextObj.error) &&
          typeof nextObj.error.message === "string"
            ? nextObj.error.message
            : "tool session returned error status";
        throw new Error(errorMessage);
      }
      await toolSession.finish();
      toolSession = null;
      return nextObj && "output" in nextObj ? nextObj.output : next;
    }
    throw new Error(`tool session ${toolName} exceeded continue step budget`);
  } catch (error) {
    if (toolSession) {
      try {
        await toolSession.abort(error instanceof Error ? error.message : String(error));
      } catch {
        // Ignore abort failures while already handling the upstream error.
      }
    }
    throw error;
  }
}

async function scheduleCallback(command: CallbackCommand): Promise<string> {
  const sourceKey = `dispatch-echo:callback:${command.token}`;
  const input: Record<string, unknown> = {
    op: "schedule",
    afterMs: 0,
    sourceKey,
    payload: {
      token: command.token,
    },
  };
  if (command.continuation === "resume_current_task") {
    input.continuation = "resume_current_task";
    input.dedupeKey = command.token;
  }
  const raw = await runSingleSendSession("system/callback", {}, input);
  let dispatchContextId = "";
  let dispatchTaskId = "";
  if (isObject(raw)) {
    if (typeof raw.dispatchContextId === "string") {
      dispatchContextId = raw.dispatchContextId;
    }
    if (typeof raw.dispatchTaskId === "string") {
      dispatchTaskId = raw.dispatchTaskId;
    }
  }
  return `scheduled callback ${command.continuation} ${command.token} dispatchContextId=${dispatchContextId} dispatchTaskId=${dispatchTaskId}`;
}

function callbackTokenFromRequest(request: HostDispatchRequest): string | null {
  const messages = extractDispatchMessages<Record<string, unknown>>(request);
  const first = messages[0];
  if (!isObject(first)) return null;
  const payload = isObject(first.payload) ? first.payload : null;
  return payload && typeof payload.token === "string" ? payload.token : null;
}

async function handleCallbackDispatch(request: HostDispatchRequest): Promise<HostDispatchAck> {
  const messages = extractDispatchMessages(request);
  const token = callbackTokenFromRequest(request);
  if (!token) {
    return {
      accepted: true,
      detail: `routing_key=${request.routing_key} messages=${messages.length} missing_token=true`,
    };
  }

  try {
    await runSingleSendSession(
      "system/discover_tools",
      { reason: "callback provenance probe" },
      { query: token, limit: 1 },
    );
    return {
      accepted: true,
      detail: `routing_key=${request.routing_key} messages=${messages.length} token=${token}`,
    };
  } catch (error) {
    return {
      accepted: true,
      detail: `routing_key=${request.routing_key} messages=${messages.length} callback_probe_error=${
        error instanceof Error ? error.message : String(error)
      }`,
    };
  }
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const callbackCommand = parseCallbackCommand(ctx.text);
    if (callbackCommand) {
      const detail = await scheduleCallback(callbackCommand);
      return { message: detail };
    }
    return { message: "dispatch-echo does not handle A2A messages" };
  },

  onDispatch: async (ctx: DispatchRunContext): Promise<HostDispatchAck> => {
    const request = ctx.request;
    if (request.routing_key === "system:callback") {
      return handleCallbackDispatch(request);
    }
    const messages = extractDispatchMessages(request);
    return {
      accepted: true,
      detail: `routing_key=${request.routing_key} messages=${messages.length}`,
    };
  },
});

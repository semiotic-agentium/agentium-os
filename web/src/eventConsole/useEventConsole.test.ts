import { afterEach, describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import type { AgentDiscoveryEntry } from "../types/a2a";
import { useEventConsole } from "./useEventConsole";
import {
  EVENT_CONSOLE_ORIGIN,
  SCOPE_EXISTING_CONTEXT,
} from "./dispatchRequest";

function eventCapableAgent(): AgentDiscoveryEntry {
  return {
    agent_package: "dispatch-echo",
    agent_instance_id: "default",
    name: "dispatch-echo",
    version: "1.0.0",
    agent_card: {
      name: "dispatch-echo",
      version: "1.0.0",
      agent_package: "dispatch-echo",
      agent_instance_id: "default",
      tools: [],
      capabilities: [],
      subscriptions: [
        {
          schema_versions: ["task-daemon.interpretation.v1"],
          source_kinds: ["slack"],
          source_keys: [],
          source_key_prefixes: [],
        },
      ],
    },
  };
}

function mountConsole() {
  const agents = ref<ReadonlyArray<AgentDiscoveryEntry>>([eventCapableAgent()]);
  const ec = useEventConsole({ agents });
  ec.selectAgent(ec.agentKey(agents.value[0]!));
  return { ec, agents };
}

function mockFetchOnce(response: Response | Error): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(() =>
    response instanceof Error ? Promise.reject(response) : Promise.resolve(response),
  );
  globalThis.fetch = fetchMock as unknown as typeof fetch;
  return fetchMock;
}

const originalFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = originalFetch;
  vi.restoreAllMocks();
});

describe("useEventConsole.dispatch()", () => {
  it("records an accepted outcome when the runner returns accepted=true", async () => {
    const { ec } = mountConsole();
    mockFetchOnce(
      new Response(JSON.stringify({ accepted: true, detail: "messages=1" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    const result = await ec.dispatch();
    expect(result?.status).toBe("accepted");
    expect(result?.httpStatus).toBe(200);
    expect(result?.detail).toBe("messages=1");
    expect(result?.targetPackage).toBe("dispatch-echo");
    expect(result?.routingKey).toBe("slack:intake");
    expect(result?.contextId).toMatch(/^ctx-\d+-\d+$/);
    expect(ec.outcome.value).toEqual(result);
  });

  it("records a rejected outcome when the runner returns accepted=false", async () => {
    const { ec } = mountConsole();
    mockFetchOnce(
      new Response(JSON.stringify({ accepted: false, detail: "subscription mismatch" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    const result = await ec.dispatch();
    expect(result?.status).toBe("rejected");
    expect(result?.detail).toBe("subscription mismatch");
  });

  it("records an error outcome on 4xx with the server body as detail", async () => {
    const { ec } = mountConsole();
    mockFetchOnce(
      new Response("routing_key must be non-empty", { status: 400 }),
    );
    const result = await ec.dispatch();
    expect(result?.status).toBe("error");
    expect(result?.httpStatus).toBe(400);
    expect(result?.detail).toBe("routing_key must be non-empty");
  });

  it("records an error outcome on a network throw with the error message as detail", async () => {
    const { ec } = mountConsole();
    mockFetchOnce(new TypeError("fetch failed: ECONNREFUSED"));
    const result = await ec.dispatch();
    expect(result?.status).toBe("error");
    expect(result?.httpStatus).toBeUndefined();
    expect(result?.detail).toBe("fetch failed: ECONNREFUSED");
  });

  it("falls back to rejected when the ack body is not valid JSON", async () => {
    const { ec } = mountConsole();
    mockFetchOnce(
      new Response("oops not json", {
        status: 200,
      }),
    );
    const result = await ec.dispatch();
    expect(result?.status).toBe("rejected");
    expect(result?.httpStatus).toBe(200);
  });

  it("posts to /agents/{pkg}/{inst}/dispatch with the operator-eval-console origin metadata", async () => {
    const { ec } = mountConsole();
    const fetchMock = mockFetchOnce(
      new Response(JSON.stringify({ accepted: true }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    await ec.dispatch();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/agents/dispatch-echo/default/dispatch");
    expect((init as RequestInit).method).toBe("POST");
    const body = JSON.parse((init as RequestInit).body as string);
    expect(body.metadata.origin).toBe(EVENT_CONSOLE_ORIGIN);
    expect(body.routing_key).toBe("slack:intake");
    expect(body.context_id).toMatch(/^ctx-\d+-\d+$/);
  });

  it("carries the operator-supplied existing context id under existing_context scope", async () => {
    const { ec } = mountConsole();
    ec.scope.kind = SCOPE_EXISTING_CONTEXT;
    ec.continueContextId.value = "ctx-444-7";
    const fetchMock = mockFetchOnce(
      new Response(JSON.stringify({ accepted: true }), { status: 200 }),
    );
    const result = await ec.dispatch();
    const body = JSON.parse((fetchMock.mock.calls[0]![1] as RequestInit).body as string);
    expect(body.context_id).toBe("ctx-444-7");
    expect(result?.contextId).toBe("ctx-444-7");
  });

  it("returns null and does not fetch when JSON is invalid", async () => {
    const { ec } = mountConsole();
    ec.messagesJsonText.value = "{ not valid";
    const fetchMock = mockFetchOnce(
      new Response(JSON.stringify({ accepted: true }), { status: 200 }),
    );
    const result = await ec.dispatch();
    expect(result).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("returns null and does not fetch when no agent is selected", async () => {
    const agents = ref<ReadonlyArray<AgentDiscoveryEntry>>([eventCapableAgent()]);
    const ec = useEventConsole({ agents });
    const fetchMock = mockFetchOnce(
      new Response(JSON.stringify({ accepted: true }), { status: 200 }),
    );
    const result = await ec.dispatch();
    expect(result).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("useEventConsole.selectSample()", () => {
  it("changes the selected sample without touching the editor body", () => {
    const { ec } = mountConsole();
    ec.messagesJsonText.value = '[{"operator":"edited"}]';
    const originalText = ec.messagesJsonText.value;
    ec.selectSample("ford-incident-raised");
    expect(ec.selectedSampleId.value).toBe("ford-incident-raised");
    expect(ec.messagesJsonText.value).toBe(originalText);
  });
});

describe("useEventConsole.previewPlaceholder", () => {
  it("reports 'Fix the JSON above…' when the editor JSON is invalid", () => {
    const { ec } = mountConsole();
    ec.messagesJsonText.value = "{ broken";
    expect(ec.previewPlaceholder.value).toBe("Fix the JSON above to see the preview.");
  });

  it("returns null when a sample is loaded and JSON parses", () => {
    const { ec } = mountConsole();
    expect(ec.previewPlaceholder.value).toBeNull();
  });
});

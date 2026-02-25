/// <reference path="./baml-runtime.d.ts" />

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function extractToolOutput<T>(value: unknown): T {
  if (isObject(value) && "output" in value) {
    return (value as { output: T }).output;
  }
  return value as T;
}

async function callMemoryTool<T>(
  toolName: string,
  sendInput: Record<string, unknown>,
): Promise<T> {
  let session: ToolSessionHandle | null = null;
  try {
    session = await openToolSession(toolName, {});
    await session.send(sendInput);
    const next = await session.continue();
    await session.finish();
    session = null;
    return extractToolOutput<T>(next);
  } catch (err) {
    if (session) {
      try {
        await session.abort(err instanceof Error ? err.message : String(err));
      } catch {
        // Ignore abort failure on error path.
      }
    }
    throw err;
  }
}

__chat_register({
  run: async (ctx) => {
    const text = (ctx.text || "").trim();
    if (!text.includes("memory-smoke")) {
      return {
        message: `Unknown or no trigger: ${text || "(empty)"} (expected 'memory-smoke')`,
      };
    }

    const add = await callMemoryTool<{
      nodeIds: number[];
      edgeCount: number;
      done: boolean;
    }>("memory/add", {
      events: [
        {
          eventType: "fact",
          content: "The sky is blue on clear days",
          sessionId: 7,
          confidence: 0.95,
        },
        {
          eventType: "decision",
          content: "Use sky-blue accent in onboarding mock",
          sessionId: 7,
          confidence: 0.9,
        },
      ],
    });

    const factId = add.nodeIds[0];
    const decisionId = add.nodeIds[1];

    const link = await callMemoryTool<{ edgesCreated: number; done: boolean }>(
      "memory/link",
      {
        edges: [
          {
            source: decisionId,
            target: factId,
            edgeType: "caused_by",
            weight: 1.0,
          },
        ],
      },
    );

    const search = await callMemoryTool<{
      matches: Array<{ id: number; eventType: string; content: string }>;
      done: boolean;
    }>("memory/search", {
      query: "sky blue",
      types: ["fact"],
      max: 5,
    });

    const traverse = await callMemoryTool<{
      nodes: Array<{ id: number; eventType: string }>;
      edges: Array<{ source: number; target: number; edgeType: string }>;
      done: boolean;
    }>("memory/traverse", {
      startId: decisionId,
      direction: "forward",
      depth: 2,
    });

    const stats = await callMemoryTool<{ status: string; nodeCount: number; edgeCount: number }>(
      "memory/stats",
      {},
    );

    const firstMatchType = search.matches[0]?.eventType || "none";
    const firstTraverseEdgeType = traverse.edges[0]?.edgeType || "none";
    const message =
      `MEMORY_SMOKE_OK addNodes=${add.nodeIds.length} ` +
      `linkEdges=${link.edgesCreated} ` +
      `searchMatches=${search.matches.length} ` +
      `searchType=${firstMatchType} ` +
      `traverseNodes=${traverse.nodes.length} ` +
      `traverseEdges=${traverse.edges.length} ` +
      `traverseEdgeType=${firstTraverseEdgeType} ` +
      `statsStatus=${stats.status} statsNodes=${stats.nodeCount} statsEdges=${stats.edgeCount}`;

    ctx.emit.message(message);
    return { message };
  },
});

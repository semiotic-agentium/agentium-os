import { describe, expect, it } from "vitest";
import type { JSONRPCResponse } from "../types/a2a";
import { readA2aSseJsonRpcStream } from "./a2aSse";

function streamFromChunks(chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(encoder.encode(chunk));
      }
      controller.close();
    },
  });
}

describe("a2a SSE parsing", () => {
  it("streams JSON-RPC events incrementally across chunk boundaries", async () => {
    const streamed: JSONRPCResponse[] = [];
    const body = streamFromChunks([
      'event: message\r\ndata: {"jsonrpc":"2.0","result":{"chunk":{"message":{"parts":[{"text":"tool start"}]}}}}\r\n\r\n',
      'event: message\r\ndata: {"jsonrpc":"2.0","result":{"chunk":{"message":{"parts":[{"text":"tool done"}]}}}}\r\n\r\n',
    ]);

    const count = await readA2aSseJsonRpcStream(body, (event) => {
      streamed.push(event);
    });

    expect(count).toBe(2);
    expect(streamed).toHaveLength(2);
    expect(streamed[0]?.result?.chunk?.message?.parts?.[0]?.text).toBe("tool start");
    expect(streamed[1]?.result?.chunk?.message?.parts?.[0]?.text).toBe("tool done");
  });

  it("handles multi-line data payload assembly while streaming", async () => {
    const streamed: JSONRPCResponse[] = [];
    const body = streamFromChunks([
      ["event: message", 'data: {"jsonrpc":"2.0",', 'data: "result":{"chunk":{"message":{"parts":[{"text":"joined"}]}}}}', "", ""].join(
        "\n",
      ),
    ]);

    const count = await readA2aSseJsonRpcStream(body, (event) => {
      streamed.push(event);
    });

    expect(count).toBe(1);
    expect(streamed).toHaveLength(1);
    expect(streamed[0]?.result?.chunk?.message?.parts?.[0]?.text).toBe("joined");
  });
});

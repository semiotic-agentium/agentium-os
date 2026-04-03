#!/usr/bin/env python3
"""DEPRECATED: Runner A2A streaming now uses SSE, not WebSocket.
Use: curl -s -N -X POST http://HOST/agents/PKG/INST/a2a/sse -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}'
Or run ./scripts/verify-runner-http.sh (SSE section).
Legacy WebSocket client kept for reference.
Usage: ./scripts/verify-a2a-ws-client.py [URL]
Requires: pip install websockets
"""
import asyncio
import json
import sys

try:
    import websockets
except ImportError:
    print("Requires: pip install websockets", file=sys.stderr)
    sys.exit(1)


async def main():
    url = sys.argv[1] if len(sys.argv) > 1 else "ws://127.0.0.1:18080/agents/stream-baml-tool/default/a2a/ws"
    request = {"jsonrpc": "2.0", "method": "tasks.list", "params": {}, "id": None}
    print(f"Connecting to {url} ...")
    async with websockets.connect(url) as ws:
        print(f"Send: {json.dumps(request)}")
        await ws.send(json.dumps(request))
        while True:
            try:
                msg = await asyncio.wait_for(ws.recv(), timeout=5.0)
            except asyncio.TimeoutError:
                break
            print(f"Recv: {msg}")
            try:
                data = json.loads(msg)
                if data.get("result") is not None:
                    print("tasks.list result:", data["result"])
                if data.get("error"):
                    print("JSON-RPC error:", data["error"])
            except json.JSONDecodeError:
                pass
    print("Done.")


if __name__ == "__main__":
    asyncio.run(main())

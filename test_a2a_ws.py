#!/usr/bin/env python3
"""
WebSocket client to test the A2A interface.
Tests JSON-RPC A2A protocol over WebSocket.
"""

import asyncio
import json
import sys
from typing import Optional

try:
    import websockets
except ImportError:
    print("Error: websockets library not found. Install with: pip install websockets")
    sys.exit(1)


async def test_a2a_websocket(uri: str = "ws://127.0.0.1:8080"):
    """Test A2A WebSocket interface with a simple request."""

    print(f"Connecting to {uri}...")

    try:
        async with websockets.connect(uri) as websocket:
            print("✅ Connected to WebSocket server")

            # Test 1: Simple A2A request with a JavaScript function call
            # This matches the format from the test cases
            request = {
                "jsonrpc": "2.0",
                "id": "test-1",
                "payload": {
                    "method": "message/send",
                    "params": {
                        "message": {
                            "kind": "message",
                            "message_id": "msg-test-1",
                            "role": "user",
                            "parts": [
                                {
                                    "text": "Hello World"
                                }
                            ],
                            "context_id": None,
                            "task_id": None,
                            "reference_task_ids": [],
                            "extensions": [],
                            "metadata": {
                                "method": "greet"
                            }
                        },
                        "configuration": None,
                        "metadata": {
                            "method": "greet"
                        }
                    }
                }
            }

            print("\n📤 Sending A2A request:")
            print(json.dumps(request, indent=2))

            await websocket.send(json.dumps(request))

            # Wait for response
            print("\n⏳ Waiting for response...")
            try:
                response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
                print("\n📥 Received response:")
                response_data = json.loads(response)
                print(json.dumps(response_data, indent=2))

                # Validate response structure
                if "jsonrpc" in response_data:
                    print("\n✅ Valid JSON-RPC response received")
                    if "result" in response_data:
                        print("✅ Response contains result")
                    elif "error" in response_data:
                        print(f"⚠️  Response contains error: {response_data['error']}")
                else:
                    print("❌ Invalid response format")

            except asyncio.TimeoutError:
                print("❌ Timeout waiting for response")
                return False

            # Test 2: Invalid JSON-RPC version
            print("\n" + "="*60)
            print("Test 2: Invalid JSON-RPC version")
            print("="*60)

            invalid_request = {
                "jsonrpc": "1.0",  # Invalid version
                "id": "test-2",
                "payload": {
                    "method": "message/send",
                    "params": {
                        "message": {
                            "kind": "message",
                            "message_id": "msg-test-2",
                            "role": "user",
                            "parts": [{"text": "test"}],
                            "context_id": None,
                            "task_id": None,
                            "reference_task_ids": [],
                            "extensions": [],
                            "metadata": None
                        },
                        "configuration": None,
                        "metadata": None
                    }
                }
            }

            print("\n📤 Sending invalid request:")
            print(json.dumps(invalid_request, indent=2))

            await websocket.send(json.dumps(invalid_request))

            try:
                response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
                print("\n📥 Received response:")
                response_data = json.loads(response)
                print(json.dumps(response_data, indent=2))

                if "error" in response_data:
                    print("✅ Correctly rejected invalid request")
                else:
                    print("⚠️  Expected error response")
            except asyncio.TimeoutError:
                print("❌ Timeout waiting for response")

            # Test 3: Malformed JSON
            print("\n" + "="*60)
            print("Test 3: Malformed JSON")
            print("="*60)

            print("\n📤 Sending malformed JSON...")
            await websocket.send("{ invalid json }")

            try:
                response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
                print("\n📥 Received response:")
                response_data = json.loads(response)
                print(json.dumps(response_data, indent=2))

                if "error" in response_data:
                    error = response_data.get("error", {})
                    if isinstance(error, dict) and error.get("code") == -32700:
                        print("✅ Correctly rejected malformed JSON (Parse error)")
                    else:
                        print(f"⚠️  Got error but unexpected code: {error}")
                else:
                    print("⚠️  Expected error response for malformed JSON")
            except asyncio.TimeoutError:
                print("❌ Timeout waiting for response")

            print("\n" + "="*60)
            print("✅ All tests completed")
            print("="*60)

            return True

    except websockets.exceptions.InvalidURI:
        print(f"❌ Invalid URI: {uri}")
        return False
    except websockets.exceptions.InvalidStatusCode as e:
        print(f"❌ Connection failed with status {e.status_code}")
        return False
    except ConnectionRefusedError:
        print(f"❌ Connection refused. Is the server running on {uri}?")
        print("   Start the server with: ./target/release/baml-rt --api-server")
        return False
    except Exception as e:
        print(f"❌ Error: {e}")
        import traceback
        traceback.print_exc()
        return False


async def main():
    """Main entry point."""
    uri = sys.argv[1] if len(sys.argv) > 1 else "ws://127.0.0.1:8080"

    print("="*60)
    print("A2A WebSocket Interface Test")
    print("="*60)
    print(f"Target: {uri}")
    print()

    success = await test_a2a_websocket(uri)
    sys.exit(0 if success else 1)


if __name__ == "__main__":
    asyncio.run(main())

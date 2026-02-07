#!/usr/bin/env node
/**
 * WebSocket client to test the A2A interface.
 * Tests JSON-RPC A2A protocol over WebSocket.
 */

const WebSocket = require('ws');

const URI = process.argv[2] || 'ws://127.0.0.1:8080';

async function testA2AWebSocket(uri) {
    console.log('='.repeat(60));
    console.log('A2A WebSocket Interface Test');
    console.log('='.repeat(60));
    console.log(`Target: ${uri}\n`);

    return new Promise((resolve, reject) => {
        console.log(`Connecting to ${uri}...`);
        
        const ws = new WebSocket(uri);

        ws.on('open', () => {
            console.log('✅ Connected to WebSocket server\n');

            // Test 1: Simple A2A request
            console.log('='.repeat(60));
            console.log('Test 1: Simple A2A Request');
            console.log('='.repeat(60));

            const request = {
                jsonrpc: "2.0",
                id: "test-1",
                method: "message/send",
                params: {
                    message: {
                        kind: "message",
                        messageId: "msg-test-1",
                        role: "user",
                        parts: [
                            {
                                kind: "text",
                                text: "Hello World"
                            }
                        ],
                        contextId: null,
                        taskId: null,
                        referenceTaskIds: [],
                        extensions: [],
                        metadata: {
                            method: "greet"
                        }
                    },
                    configuration: null,
                    metadata: {
                        method: "greet"
                    }
                }
            };

            console.log('\n📤 Sending A2A request:');
            console.log(JSON.stringify(request, null, 2));

            ws.send(JSON.stringify(request));
        });

        let testCount = 0;
        const maxTests = 4;
        let pongReceived = false;

        ws.on('message', (data) => {
            testCount++;
            console.log('\n📥 Received response:');
            try {
                const response = JSON.parse(data.toString());
                console.log(JSON.stringify(response, null, 2));

                // Validate response structure
                if (response.jsonrpc) {
                    console.log('\n✅ Valid JSON-RPC response received');
                    if (response.result) {
                        console.log('✅ Response contains result');
                    } else if (response.error) {
                        console.log(`⚠️  Response contains error: ${JSON.stringify(response.error)}`);
                    }
                } else {
                    console.log('❌ Invalid response format');
                }

                // Send next test
                if (testCount === 1) {
                    // Test 2: Invalid JSON-RPC version
                    console.log('\n' + '='.repeat(60));
                    console.log('Test 2: Invalid JSON-RPC version');
                    console.log('='.repeat(60));

                    const invalidRequest = {
                        jsonrpc: "1.0", // Invalid version
                        id: "test-2",
                        method: "message/send",
                        params: {
                    message: {
                        kind: "message",
                        messageId: "msg-test-2",
                                role: "user",
                                parts: [{ kind: "text", text: "test" }],
                        contextId: null,
                        taskId: null,
                        referenceTaskIds: [],
                                extensions: [],
                                metadata: null
                            },
                            configuration: null,
                            metadata: null
                        }
                    };

                    console.log('\n📤 Sending invalid request:');
                    console.log(JSON.stringify(invalidRequest, null, 2));

                    ws.send(JSON.stringify(invalidRequest));
                } else if (testCount === 2) {
                    // Test 3: Malformed JSON
                    console.log('\n' + '='.repeat(60));
                    console.log('Test 3: Malformed JSON');
                    console.log('='.repeat(60));

                    console.log('\n📤 Sending malformed JSON...');
                    ws.send('{ invalid json }');
                } else if (testCount === 3) {
                    // Test 4: Ping/Pong keepalive for long-running chats
                    console.log('\n' + '='.repeat(60));
                    console.log('Test 4: Ping/Pong Keepalive');
                    console.log('='.repeat(60));

                    console.log('\n📤 Sending WebSocket ping...');
                    ws.ping();

                    setTimeout(() => {
                        if (pongReceived) {
                            console.log('✅ Pong received (keepalive ok)');
                        } else {
                            console.log('❌ Pong not received within timeout');
                        }
                        console.log('\n' + '='.repeat(60));
                        console.log('✅ All tests completed');
                        console.log('='.repeat(60));
                        ws.close();
                        resolve(pongReceived);
                    }, 2000);
                }
            } catch (e) {
                console.log(`\n❌ Error parsing response: ${e.message}`);
                if (testCount >= maxTests) {
                    ws.close();
                    resolve(false);
                }
            }
        });

        ws.on('error', (error) => {
            console.error(`\n❌ WebSocket error: ${error.message}`);
            if (error.code === 'ECONNREFUSED') {
                console.error(`\n   Connection refused. Is the server running on ${uri}?`);
                console.error('   Start the server with: ./target/release/baml-rt --api-server');
            }
            reject(error);
        });

        ws.on('close', () => {
            if (testCount < maxTests) {
                console.log('\n⚠️  Connection closed before all tests completed');
            }
        });

        ws.on('pong', () => {
            pongReceived = true;
        });
    });
}

// Main execution
testA2AWebSocket(URI)
    .then((success) => {
        process.exit(success ? 0 : 1);
    })
    .catch((error) => {
        console.error(`\n❌ Test failed: ${error.message}`);
        process.exit(1);
    });

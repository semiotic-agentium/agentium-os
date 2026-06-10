# Host Tool Development Guide

Host tools in Agentium OS provide agents with capabilities beyond LLM inference. This guide covers both **MCP (Model Context Protocol) servers** and **external tools** — the two primary host tool types.

## Tool Types Overview

### MCP Servers
- **Protocol**: JSON-RPC over stdio/SSE
- **Lifecycle**: Managed by Agentium runtime
- **State**: Stateless request/response
- **Use cases**: APIs, databases, file systems
- **Registry**: Cached server snapshots with schema validation

### External Tools  
- **Protocol**: Custom execution environments
- **Lifecycle**: Sandboxed process execution
- **State**: Stateful session management
- **Use cases**: Complex workflows, interactive tools
- **Registry**: Validated tool implementations with metadata

## MCP Server Integration

### Server Configuration

MCP servers are configured in `mcp-catalog.json`:

```json
{
  "servers": {
    "filesystem": {
      "command": "npx",
      "args": ["@modelcontextprotocol/server-filesystem", "/tmp"],
      "env": {}
    },
    "postgres": {
      "command": "mcp-server-postgres",
      "args": ["postgresql://user:pass@localhost/db"]
    }
  }
}
```

### Registry Snapshots

Agentium maintains registry snapshots of MCP servers for:
- **Schema validation** - Ensure tool/resource schemas match expectations
- **Offline builds** - Enable CI/test without external dependencies  
- **Version consistency** - Lock to approved server versions

Snapshot resolution:
1. Check registry for cached server snapshot
2. Fall back to live server discovery if not cached
3. Validate schema compatibility

### BAML Integration

MCP tools are exposed as BAML functions:

```typescript
// Generated from MCP server schema
function ReadFile(path: string) -> string
function WriteFile(path: string, content: string) -> string
function ListDirectory(path: string) -> string[]
```

Usage in agents:

```typescript
import { ReadFile, WriteFile } from "./baml_client"

const content = await ReadFile("/path/to/file.txt")
const result = await WriteFile("/output.txt", processedContent)
```

## External Tool Development

### Tool Manifest

Every external tool requires `tool-manifest.json`:

```json
{
  "name": "weather-api",
  "version": "1.0.0",
  "description": "Weather data retrieval tool",
  "interface": {
    "operations": {
      "get_weather": {
        "input_schema": {
          "type": "object",
          "properties": {
            "location": {"type": "string"},
            "units": {"type": "string", "enum": ["metric", "imperial"]}
          },
          "required": ["location"]
        },
        "output_schema": {
          "type": "object",
          "properties": {
            "temperature": {"type": "number"},
            "conditions": {"type": "string"}
          }
        }
      }
    }
  },
  "runtime": {
    "type": "container",
    "image": "weather-tool:latest",
    "entrypoint": ["/app/tool-adapter"]
  }
}
```

### Tool Adapter Protocol

External tools communicate via JSON-RPC over stdin/stdout:

```json
// Request
{"jsonrpc": "2.0", "id": 1, "method": "get_weather", "params": {"location": "San Francisco"}}

// Response  
{"jsonrpc": "2.0", "id": 1, "result": {"temperature": 18, "conditions": "cloudy"}}

// Error
{"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "API unavailable"}}
```

### Session State Management

External tools follow a session FSM:

```
Open → Send → SearchRead/PageRead → Send → ... → Finish/Abort
```

**States:**
- `Open` - Initialize tool session
- `Send` - Execute operation with parameters
- `SearchRead` - Query operation results
- `PageRead` - Paginate through results
- `Finish` - Clean session termination
- `Abort` - Force session cleanup

### Language Templates

The CLI generates tool adapters in multiple languages:

#### Python Template
```python
#!/usr/bin/env python3
import json
import sys
from typing import Dict, Any

def handle_get_weather(params: Dict[str, Any]) -> Dict[str, Any]:
    location = params["location"]
    # Implementation here
    return {"temperature": 20, "conditions": "sunny"}

def main():
    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        
        if method == "get_weather":
            result = handle_get_weather(request["params"])
            response = {"jsonrpc": "2.0", "id": request["id"], "result": result}
        else:
            response = {"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "Method not found"}}
        
        print(json.dumps(response))
        sys.stdout.flush()

if __name__ == "__main__":
    main()
```

#### Rust Template
```rust
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader};

fn handle_get_weather(params: &Value) -> Result<Value, String> {
    let location = params["location"].as_str().ok_or("Missing location")?;
    // Implementation here
    Ok(json!({"temperature": 20, "conditions": "sunny"}))
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let reader = BufReader::new(stdin.lock());
    
    for line in reader.lines() {
        let line = line?;
        let request: Value = serde_json::from_str(&line).unwrap();
        
        let response = match request["method"].as_str() {
            Some("get_weather") => {
                match handle_get_weather(&request["params"]) {
                    Ok(result) => json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": result
                    }),
                    Err(err) => json!({
                        "jsonrpc": "2.0", 
                        "id": request["id"],
                        "error": {"code": -32000, "message": err}
                    })
                }
            },
            _ => json!({
                "jsonrpc": "2.0",
                "id": request["id"], 
                "error": {"code": -32601, "message": "Method not found"}
            })
        };
        
        println!("{}", response);
    }
    Ok(())
}
```

## Tool Registration Workflow

### 1. Enable External Tool
```bash
builder external-tool enable ./tools/weather-api --repository-url http://localhost:18080/repository
```

This:
- Validates `tool-manifest.json`
- Generates language-specific adapter code
- Creates sandbox binding if requested
- Imports approved snapshot into registry

### 2. Refresh Tool
```bash
builder external-tool refresh weather-api --dir ./tools/weather-api
```

Updates existing tool with new implementation.

### 3. Inspect Tool
```bash
builder external-tool inspect support/weather --json
```

Shows tool metadata and interface details.

## BAML Integration

External tools are exposed as BAML classes:

```typescript
// Generated from tool manifest
class WeatherAPI {
  async get_weather(location: string, units?: string): Promise<{temperature: number, conditions: string}> {
    // Runtime handles tool session lifecycle
  }
}
```

Usage in agents:

```typescript
import { WeatherAPI } from "./baml_client"

const weather = new WeatherAPI()
const result = await weather.get_weather("San Francisco", "metric")
console.log(`Temperature: ${result.temperature}°C, Conditions: ${result.conditions}`)
```

## Registry Architecture

The Agentium registry provides:

### MCP Snapshots
- **Server metadata** - Command, args, environment
- **Schema cache** - Tool/resource definitions
- **Version tracking** - Immutable snapshot references

### External Tool Snapshots  
- **Manifest validation** - Schema compliance
- **Implementation artifacts** - Container images, binaries
- **Interface contracts** - Operation schemas

### Offline Capabilities
- **Snapshot export** - Bundle for CI/test environments
- **Cache validation** - Verify snapshot integrity
- **Fallback resolution** - Graceful degradation without registry

## Best Practices

### Tool Design
1. **Single responsibility** - One tool per logical capability
2. **Stateless operations** - Minimize session state complexity
3. **Error handling** - Provide clear error messages and codes
4. **Schema validation** - Validate inputs/outputs strictly

### Performance
1. **Lazy initialization** - Start tools only when needed
2. **Connection pooling** - Reuse tool sessions where possible
3. **Timeout handling** - Set reasonable operation timeouts
4. **Resource cleanup** - Ensure proper session termination

### Security
1. **Sandbox isolation** - Run tools in restricted environments
2. **Input validation** - Sanitize all tool inputs
3. **Capability limits** - Restrict tool access to necessary resources
4. **Audit logging** - Track tool usage and outcomes

## Troubleshooting

### Common Issues

**Tool not found**
- Verify tool is registered in registry
- Check agent's tool dependencies
- Ensure registry URL is accessible

**Schema validation errors**
- Compare manifest schema with BAML expectations
- Regenerate BAML types after tool changes
- Validate JSON schema syntax

**Session timeout**
- Check tool adapter responsiveness
- Verify container resource limits
- Review tool operation complexity

**Registry connection issues**
- Confirm registry URL and authentication
- Test network connectivity
- Check snapshot cache availability

### Debug Commands

```bash
# Validate tool manifest
builder check-external-tool ./tools/weather-api

# Inspect registry snapshot
builder external-tool inspect weather-api --json

# Report snapshot cache contents
builder snapshot-report --snapshot-cache ./cache

# Workspace integrity check
builder doctor --ci
```

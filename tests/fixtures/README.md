# Test Fixtures

This directory contains test fixtures used by the test suite.

## Structure

- `agents/` - Complete test agent applications
  - `voidship-rites/` - Grimdark hybrid A2A + streaming fixture

- `baml/` - BAML schema fixtures
  - `simple_prompt.baml` - Simple greeting function
  - `tool_calling.baml` - Tool calling example
  - `tool_union.baml` - Union type tool calling
  - `weather_tool.baml` - Weather tool definition

- `packages/` - Pre-built test packages (generated during tests)
  - This directory is for packages created during test execution

## Usage

Use the fixture helpers in `tests/support/fixtures.rs`:

```rust
use tests::support::*;

let baml_path = baml_fixture("simple_prompt.baml");
let agent_path = agent_fixture("voidship-rites");
```

## Adding New Fixtures

1. Place BAML files in `baml/`
2. Place agent applications in `agents/{name}/`
3. Update this README if adding new categories

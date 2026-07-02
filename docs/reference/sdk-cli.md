# SDK CLI Reference (moved)

The SDK CLI is now the unified **`agentium`** binary. This page is a redirect and migration cheat sheet.

**Canonical reference:** [`agentium-cli.md`](agentium-cli.md)

## Migration

| Before | After |
|--------|-------|
| `cargo run -p cargo-agent-platform -- …` | `agentium …` or `cargo run -p agentium -- …` |
| `baml-agent-runner --serve-http …` | `agentium serve --serve-http …` |
| `cargo-agent-platform push` | `agentium install agent` |
| `cargo-agent-platform build` / `regen` | removed — server builds on `POST /repository/publish`; use `just regen-fixtures` in the monorepo |
| `cargo-agent-platform deploy` | `agentium deploy --hash …` |
| `cargo-agent-platform publish` | `agentium publish --agent-dir …` |

Install and quick start: [`INSTALL.md`](../../INSTALL.md).

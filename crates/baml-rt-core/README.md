# baml-rt-core

Core types and shared utilities for the Agentium OS workspace.

## Responsibilities

- Shared error and result types (`BamlRtError`, `Result`).
- Correlation ID helpers for tracing and request continuity.
- Core type wrappers used by higher-level crates.
- Stream-first A2A boundary types (`A2aRequestHandler`, `BusStream`) and stream collection helpers for edge adapters.
- Effect bus runtime primitives (`Bus`, `BusWithEffects`, envelopes, effect liveness) used to stabilize async orchestration.

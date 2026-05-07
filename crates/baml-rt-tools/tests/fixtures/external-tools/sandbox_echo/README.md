# `support/sandbox_echo`

Smallest possible `runtime.kind = "sandbox"` tool package. Exists to exercise
the end-to-end resolve path introduced in Workstream Y of `tool_sandbox.md`:

- parsed into the runtime view by `read_runtime_external_metadata`
- routed through `DevModeResolver::from_dirs_with_sandbox`
- dispatched to a `SandboxToolHandler` built by the runner-wired
  `SandboxSpecFactory`

## Local runs

```sh
# Dev / fixture mode — uses MockSandboxProvider, no VM required.
export BAML_SANDBOX_PROVIDER=mock
export BAML_EXTERNAL_TOOLS_DIR=$(pwd)/crates/baml-rt-tools/tests/fixtures/external-tools/sandbox_echo

# Real microsandbox backend (requires the cargo feature + a KVM-enabled Linux host
# or Apple Silicon).
export BAML_SANDBOX_PROVIDER=microsandbox
cargo run -p baml-agent-runner --features sandbox-provider
```

## Notes

- `image` is a placeholder digest-pinned reference (`sha256:00…`). The real
  echo adapter image isn't built here — publishing the image is a separate
  ops task tracked under Workstream F.
- OCI identity is the digest-pinned image reference itself; no separate
  `runtime_digest` field is used.
- Schema matches the process-backed `e2e_echo` sibling so the same BAML
  contract can be swapped between backends without agent changes.

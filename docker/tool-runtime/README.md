# Tool runtime images

The guest image for external tools v2. `node.Dockerfile` builds the blessed
Node 22 image; the same contract applies to future `python`/`native` images.

## Build

```bash
just tool-runtime-image
```

The recipe builds `baml-tool-supervisor` in release mode, then the image. Push
it digest-pinned and register that reference with the runner:

```rust
provider.with_runtime_image(default_node_runtime_image(), "registry/tool-runtime@sha256:…")
```

`default_node_runtime_image()` covers `{node_major, protocol, supervisor_abi}`.
Changing node major, the protocol, or the supervisor ABI changes the registry
digest, so every tool digest changes too. Toolsets vendor their own MCP SDK and
dependencies.

## Contract

| Path | Purpose |
| --- | --- |
| `/usr/local/bin/baml-tool-supervisor` | Entrypoint/PID 1, the only process on the vsock |
| `/tool/code/node_modules` | Vendored MCP SDK and tool dependencies |
| `/tool` | Read-only bundle mount per worker (created by the supervisor) |
| `/run/ktool` | Private bundle tmpfs root (mounted by the supervisor as root) |
| `/tmp` | Per-worker sized tmpfs |
| uid/gid `100000+` | One distinct pair per tool digest |

The seccomp allowlist is compiled into the supervisor and pinned by
`supervisor_abi`; there is no external profile file to drift. vsock port
defaults to `5000` and must match the provider's `--vsock` value (override with
`KARAKARA_VSOCK_PORT`).

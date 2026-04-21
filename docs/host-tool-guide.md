# Host + External Tool Guide

This guide covers both tool paths:

1. **External tools (recommended default)** — standalone tool directories loaded at runtime
2. **Static host tools (Rust, compiled into platform)** — platform-internal only

For agent authoring (BAML prompts, planner loops, `StructuredReply`), see [How to write agents](how-to-write-agents.md).

---

## 0) Prerequisites

For sandboxed external tools (especially `microsandbox`):

- Rust toolchain + Cargo
- Docker
- `jq`
- Host virtualization support:
  - Linux with KVM enabled, or
  - macOS Apple Silicon (where supported by your local setup)
- Runner built with sandbox provider feature when using `microsandbox`:

```bash
cargo run -p baml-agent-runner --features sandbox-provider
```

Ubuntu/Debian packages commonly needed:

```bash
sudo apt-get update
sudo apt-get install -y libcap-ng-dev libcap-ng0 pkg-config
```

---

## 0.5) Golden path (end-to-end)

Use this sequence for a new sandboxed external tool:

1. Scaffold tool (`new-tool`)
2. Build adapter artifact (OCI image and optionally bind rootfs export)
3. Compute/refresh digest (`sandbox-digest` when needed)
4. Validate metadata (`check-external-tool`)
5. Configure + start runner (`BAML_EXTERNAL_TOOLS_DIR`, sandbox env)
6. Allowlist tool in agent manifest
7. Publish + deploy agent
8. Verify with chat

---

## 1) External tools (standalone, recommended)

## 1.1 Scaffold a new tool

```bash
cargo agent-platform new-tool my_tool \
  --bundle dev \
  --lang rust \
  --access read \
  --output ./examples/external-tools/my_tool
```

Useful sandbox flags:

- `--runtime sandbox`
- `--sandbox-source oci|bind`
- `--sandbox-image <ref@sha256:...>` (OCI)
- `--sandbox-bind-path <dir>` (Bind)
- `--sandbox-entrypoint <argv,...>`
- `--runtime-digest <sha256:...>` (optional; auto in common paths)

Tool naming rules:

- local tool name: lowercase `a-z0-9_-`
- fully-qualified name: `bundle/local_name`

## 1.2 What gets scaffolded

`new-tool` creates a standalone tool directory with at least:

- `tool-metadata.json` (runtime/access/schema contract)
- `tool-server` launcher
- `README.md`
- language-specific source files

For sandbox tools, metadata points to a sandbox image source, but you still need to build/provide the adapter artifact (next section).

---

## 2) Sandbox adapter mental model (important)

Sandbox tool invocation uses a small adapter binary (commonly `tool-adapter`) inside the guest rootfs/image.

- The adapter speaks the sandbox protocol over stdin/stdout.
- Framing is length-prefixed JSON (4-byte big-endian length + JSON payload).
- Source of truth for framing: `crates/baml-sandbox-protocol/src/codec.rs`.

In bind-rootfs flows, adapter execution first tries absolute path `/tool-adapter` (distroless-friendly), then falls back to `tool-adapter` via `PATH`.

---

## 3) Build adapter artifact (OCI vs Bind)

## 3.1 OCI source

Build/push a digest-pinned image:

```bash
docker build -t ghcr.io/acme/my-tool:latest -f <Dockerfile> .
# push + obtain digest, then use ghcr.io/acme/my-tool@sha256:...
```

Set metadata runtime image to OCI ref and ensure `runtime_digest` matches.

## 3.2 Bind source

Typical pattern:

1. Build local image
2. Export rootfs dir (`export_rootfs.sh` pattern)
3. Point metadata runtime image to bind path
4. Set/refresh `runtime_digest`

Reference runnable example:
- `examples/external-tools/dev_echo_sandbox/README.md`

---

## 4) Digest workflow (`runtime_digest`)

`runtime_digest` is required for sandbox runtime identity.

### When digest is automatic

- `new-tool --runtime sandbox --sandbox-source oci --sandbox-image <...@sha256:...>`: digest derives from image ref suffix.
- `new-tool --runtime sandbox --sandbox-source bind --sandbox-bind-path <dir>`: digest is computed from bind rootfs content.

### When to run `sandbox-digest`

Use `sandbox-digest` when:

- you patch metadata manually,
- rootfs content changed after scaffold,
- CI/script needs deterministic recompute.

```bash
cargo agent-platform sandbox-digest --source bind /abs/path/to/rootfs
```

---

## 5) Validate metadata before deploy

```bash
cargo agent-platform check-external-tool --path ./examples/external-tools/my_tool
```

This validates:

1. schema compliance,
2. runtime typed parse,
3. sandbox source consistency:
   - OCI digest pin + match,
   - Bind path resolve + recomputed digest match.

---

## 6) Configure and start runner

Core env:

```bash
export BAML_EXTERNAL_TOOLS_DIR=/abs/path/to/external-tools
```

Sandbox provider:

- `BAML_SANDBOX_PROVIDER=off` (no sandbox)
- `BAML_SANDBOX_PROVIDER=mock` (fast wiring/dev checks)
- `BAML_SANDBOX_PROVIDER=microsandbox` (real microVM)

Bind allowlist (colon-separated roots):

```bash
export BAML_SANDBOX_BIND_ROOTS=/abs/root1:/abs/root2
```

Start runner for microsandbox:

```bash
cargo run -p baml-agent-runner --features sandbox-provider
```

---

## 7) Agent wiring + publish/deploy/chat

## 7.1 Allowlist tool in agent manifest

In `agents/<agent>/manifest.json`:

```json
{
  "tools": ["dev/my_tool"]
}
```

If not allowlisted, tool registration/invocation fails.

## 7.2 Publish + deploy + verify

```bash
cargo agent-platform publish --agent-dir agents/my-agent
cargo agent-platform deploy --hash <HASH_FROM_PUBLISH>
cargo agent-platform list-deployed-instances
cargo agent-platform chat --agent my-agent
```

---

## 8) Iteration workflow (most common)

After adapter/rootfs changes in bind mode, run:

1. rebuild adapter image
2. re-export rootfs (`--force`)
3. recompute digest (`sandbox-digest`)
4. update metadata
5. re-run `check-external-tool`
6. re-publish + re-deploy agent

If you skip digest refresh after rootfs mutation, metadata identity is stale.

---

## 9) Debugging and troubleshooting

### Useful debugging helper

Use the shipped inspector to test adapter protocol directly (outside microVM):

```bash
./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py --adapter /path/to/tool-adapter describe
./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py --adapter /path/to/tool-adapter invoke --message "hello"
```

### Common failures

- **"microsandbox support was not built in"**
  - start runner with `--features sandbox-provider`
- **Bind path rejected / escapes allowlist**
  - ensure bind path is under `BAML_SANDBOX_BIND_ROOTS`
- **`unable to find library -lcap-ng`**
  - install `libcap-ng-dev` + `pkg-config`
- **KVM/unavailable virtualization errors**
  - use `mock` provider or fix host virtualization setup
- **adapter not found at `/tool-adapter`**
  - ensure rootfs/image contains adapter at `/tool-adapter` (or PATH fallback)
- **bind digest mismatch on check**
  - rerun `sandbox-digest`, patch metadata, validate again

---

## 10) Security notes for Bind (v1)

Bind mode is for pragmatic local/dev workflows and controlled deployments.

Important v1 realities:

- bind rootfs is mutable host state,
- runner enforces only configured allowlist roots,
- choose narrow allowlist roots (avoid broad shared directories),
- on shared hosts, enforce strict ownership/permissions on allowlisted roots,
- bind reattach is disabled in v1 (cold-create policy) to avoid drift surprises.

For stronger immutability and supply-chain posture, prefer OCI digest-pinned images.

---

## 11) Static host tools (compiled-in Rust)

Use only for platform-internal extensions.

```bash
cargo agent-platform new-static-tool my-tool
```

Then:

1. implement `BamlTool`
2. register metadata/builder hooks
3. ensure workspace/tool-link wiring is correct
4. allowlist in agent manifest

For most integrations, use external tools instead.

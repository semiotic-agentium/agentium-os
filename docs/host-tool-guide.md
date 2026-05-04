# Host + External Tool Guide

This guide covers both tool paths:

1. **External tools (recommended default)** — standalone tool directories loaded at runtime
2. **Static host tools (Rust, compiled into platform)** — platform-internal only

For agent authoring (BAML prompts, planner loops, `StructuredReply`), see [How to write agents](how-to-write-agents.md).

---

## 0) Prerequisites

For external tools in general:

- Rust toolchain + Cargo (nightly pinned via `rust-toolchain.toml`)
- `jq` (optional; only needed for manual JSON patching outside CLI helpers)

Additional for sandboxed tools (especially `microsandbox`):

- Docker (for building adapter images / exporting rootfs)
- Host virtualization support:
  - Linux with KVM enabled, or
  - macOS Apple Silicon
- Ubuntu/Debian packages commonly needed:

```bash
sudo apt-get update
sudo apt-get install -y libcap-ng-dev libcap-ng0 pkg-config
```

> The runner itself is launched in §6 — that step needs the `sandbox-provider` cargo feature when using `microsandbox`. We recommend running with `--all-features` so other runner-side integrations (memory, http-tools, etc.) are available simultaneously.

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

## 0.6) Quick session-mode sanity check

If you just want to verify the full session path quickly:

```bash
# 1) Scaffold a sandbox session-mode tool
cargo agent-platform new-tool streamed_echo \
  --runtime sandbox \
  --invocation-mode session \
  --sandbox-source oci \
  --sandbox-image ghcr.io/acme/streamed-echo@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

# 2) Validate metadata
cargo agent-platform check-external-tool --path ./streamed_echo

# 3) Start runner with a runner.toml (recommended) or fall back to env vars
cat > runner.toml <<'EOF'
[external_tools]
dirs = ["./streamed_echo"]

[sandbox.bind]
roots = []  # add host paths here when sandbox tools need bind mounts
EOF
cargo run -p baml-agent-runner --all-features -- --runner-config ./runner.toml

# Equivalent legacy form (still supported; env wins when both file and env are set):
# export BAML_EXTERNAL_TOOLS_DIR="$(pwd)/streamed_echo"
# cargo run -p baml-agent-runner --all-features
```

Then allowlist the tool in an agent manifest, publish + deploy, and verify with `cargo agent-platform chat`.

## 1) External tools (standalone, recommended)

## 1.1 Scaffold a new tool

```bash
cargo agent-platform new-tool my_tool \
  --bundle dev \
  --lang rust \
  --access read \
  --output ./my_tool
```

> Use any absolute path for `--output`. External tools are designed to live in independent repos; the path above is neutral. To try in-tree with this platform, use `./examples/external-tools/my_tool`.

Useful sandbox flags:

- `--runtime sandbox`
- `--invocation-mode single-shot|session`
- `--sandbox-source oci|bind`
- `--sandbox-image <ref@sha256:...>` (OCI)
- `--sandbox-entrypoint <argv,...>`
- `--runtime-digest <sha256:...>` (optional; auto in common paths)
- `--generate-docker` (bind-only scaffold helper: emits `adapter/Dockerfile` + `setup_bind_sandbox.sh` wrapper)

Tool naming rules:

- local tool name: lowercase `a-z0-9_-`
- fully-qualified name: `bundle/local_name`

## 1.2 What gets scaffolded

`new-tool` creates a standalone tool directory with at least:

- `tool-metadata.json` (runtime / access / schema contract)
- `README.md`
- language-specific source files

Runtime-specific emissions:

- **`--runtime process`** (default): a host-side `tool-server` launcher executable is scaffolded. The runner invokes it directly over stdio.
- **`--runtime sandbox`**: runner execution goes through the guest adapter at `/tool-adapter`. Some scaffolds may still include a host `tool-server` helper for local probing, but it is not the runtime invoke path. The adapter binary/script (usually `tool-adapter`) must be built by you and placed inside the OCI image or bind rootfs at `/tool-adapter` (see §2–§3). Metadata references the image/rootfs; the runner spawns the adapter inside the guest.

For sandbox tools, metadata points to a sandbox image source, but you still need to build/provide the adapter artifact (next section).

## 1.3 Process-runtime shortcut (non-sandboxed)

If you chose `--runtime process` (the default), most of the sandbox sections below do not apply. Minimum path:

1. §1.1 scaffold → §1.2 understand the emitted `tool-server` launcher
2. Implement the launcher for your language — it reads length-prefixed JSON on stdin, writes replies on stdout
3. §5 validate metadata (`check-external-tool`)
4. §6 configure runner with `BAML_EXTERNAL_TOOLS_DIR` (`BAML_SANDBOX_*` env vars not needed)
5. §7 agent wiring + publish + deploy + chat

Skip §2 (sandbox adapter model), §3 (build adapter artifact), §4 (runtime_digest), §9's sandbox-specific entries, and §10 (Bind security).

## 1.4 Session-mode external tools (`invocation_mode=session`)

Use session mode when the external tool needs persistent in-session state and/or chunked streaming.

Metadata essentials:

- `runtime.kind = "sandbox"` (required)
- `invocation_mode = "session"`
- host must be built with the `sandbox-provider` feature (default for the runner) so sandbox wiring is present
- default scaffold behavior keeps:
  - `session_policy = "strict"`
  - `secret_scope = "send"`

Required adapter methods (advertised via `tool/describe.supported_methods`):

- `tool/session_open`
- `tool/session_send`
- `tool/session_read`
- `tool/session_finish`
- `tool/session_abort`

Host call pattern for external session tools:

1. open session
2. send input
3. read steps until `done`/`error`
4. finish (or abort)

Important: external `read()` is payloadless at the protocol level. If a caller passes payload to host `read()`, the runtime rejects it; use explicit `send(input)` before `read()`.

---

## 2) Sandbox adapter mental model (important)

Sandbox tool invocation uses a small adapter binary (commonly `tool-adapter`) inside the guest rootfs/image.

**Protocol invariant (all sandbox image sources, including Bind):**
- sandbox adapter transport is TSRPC-framed JSON-RPC over stdin/stdout,
- framing is length-prefixed JSON (4-byte big-endian length + JSON payload),
- newline-delimited/raw stdio JSON-RPC is **not** sufficient for sandbox adapter mode.

Additional notes:
- Max frame size is capped at `MAX_FRAME_BYTES` — don't ship multi-MiB payloads without chunking.
- Source of truth for framing + constants: `crates/baml-sandbox-protocol/src/codec.rs`.

Adapter lookup order at invoke time:

1. absolute path `/tool-adapter` (distroless-friendly, matches bind-rootfs convention),
2. `tool-adapter` via guest `PATH` (fallback for images with a populated PATH).

Place your adapter at `/tool-adapter` for the most reliable behavior.

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

Bind mode points the guest at a host directory used directly as the rootfs — no registry needed.

Generic export flow (any OCI image → bind rootfs dir):

```bash
# 1. build or pull the image locally
docker build -t my-tool:local -f Dockerfile .

# 2. create a scratch container and export its filesystem
CID="$(docker create my-tool:local)"
mkdir -p /abs/path/to/rootfs
docker export "$CID" | tar -x -C /abs/path/to/rootfs
docker rm "$CID"
```

Non-Docker alternatives: `skopeo copy oci:...` followed by manual layer flatten, or `umoci unpack`.

Then:

3. Point source metadata `runtime.image` to a portable relative path such as `{"kind": "bind", "path": "./.tmp/my-tool-rootfs"}`.
4. Run `sandbox-bind-sync` (§4) to write the local `tool-metadata.lock.json` with the canonical absolute path + `runtime_digest`.
5. Ensure the resolved bind path is under `BAML_SANDBOX_BIND_ROOTS` before starting the runner (§6).

Reference runnable example:
- `examples/external-tools/dev_echo_sandbox/README.md` (includes an `export_rootfs.sh` helper that wraps the steps above)

---

## 4) Digest workflow (`runtime_digest`)

`runtime_digest` is required for sandbox runtime identity.

### When digest is automatic

- `new-tool --runtime sandbox --sandbox-source oci --sandbox-image <...@sha256:...>`: digest derives from image ref suffix.
- `new-tool --runtime sandbox --sandbox-source bind`: scaffold emits a portable tool-relative bind path and no source `runtime_digest`; run `sandbox-bind-sync` after rootfs materialization to generate the local lock sidecar.
  - default mode emits metadata only (no setup script)
  - adding `--generate-docker` emits `adapter/Dockerfile` + `adapter/tool-adapter` + `setup_bind_sandbox.sh` wrapper for Docker build/export + runtime lock/sidecar sync/validation

`setup_bind_sandbox.sh` resolves the SDK CLI command in this order:
1. `AGENT_PLATFORM_CMD` (if set)
2. `cargo agent-platform <subcommand>` (validated per subcommand)
3. `cargo run -q -p cargo-agent-platform -- <subcommand>` (workspace fallback)

This avoids stale installed-plugin mismatches.

### Preferred bind sync command

Use `sandbox-bind-sync` to write the local `tool-metadata.lock.json` with the resolved bind path + digest, generate the adapter sidecar bundle (`/etc/agentium/tool-bundle.json`), and optionally validate metadata. It never mutates committed `tool-metadata.json`. Relative `--rootfs` and `--dockerfile` paths resolve against `--tool-dir`; if `--rootfs` is omitted, it defaults to the source metadata `runtime.image.path`.

```bash
cargo agent-platform sandbox-bind-sync \
  --tool-dir ./examples/external-tools/my_tool \
  --check
```

Docker-assisted mode (build + export + sync + validate). `--image` is explicit; when it is provided, `--dockerfile` defaults to `adapter/Dockerfile`:

```bash
cargo agent-platform sandbox-bind-sync \
  --tool-dir ./examples/external-tools/my_tool \
  --image support-my-tool-sandbox:local \
  --force \
  --check
```

### OCI sidecar preparation (no registry pull)

Use `sandbox-oci-prepare` when metadata already points at an OCI image and you
want to materialize the adapter sidecar bundle next to your tool sources:

```bash
cargo agent-platform sandbox-oci-prepare \
  --tool-dir ./examples/external-tools/my_tool \
  --check
```

This validates OCI metadata/digest consistency and writes the default output at:
`adapter/sidecars/etc/agentium/tool-bundle.json`.

### When to run `sandbox-digest`

Use `sandbox-digest` for low-level digest-only checks (without metadata patching):

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
   - OCI digest pin + match against `runtime_digest`,
   - Bind source metadata is portable, `tool-metadata.lock.json` resolves to a directory, and recomputed bind digest matches the locked `runtime_digest`.

---

## 6) Configure and start runner

Recommended: a `runner.toml` consumed via `--runner-config`. Falls back to env vars; env wins when both file and env are set.

```toml
# runner.toml — paths relative to this file's parent dir
[external_tools]
dirs = ["/abs/path/to/external-tools"]

[sandbox.bind]
roots = ["/abs/root1", "/abs/root2"]
```

```bash
cargo run -p baml-agent-runner --all-features -- --runner-config ./runner.toml
```

Each entry under `[external_tools].dirs` is a tool package directory containing its own `tool-metadata.json`. The runner loads the catalog at boot — **restart the runner after adding / editing / removing tools**; there is no hot reload in v1.

Legacy env vars still work (for back-compat and quick scripts):

```bash
export BAML_EXTERNAL_TOOLS_DIR=/abs/path/to/external-tools   # colon-separated
export BAML_SANDBOX_BIND_ROOTS=/abs/root1:/abs/root2         # colon-separated
```

Resolution precedence per list: env (when set and non-empty) replaces the file value; otherwise the file value applies. Startup logs show the resolved source: `external_tools.dirs source=<file|env|none>` and `sandbox.bind.roots source=<file|env|none>`.

Sandbox provider:

- `BAML_SANDBOX_PROVIDER=off` (no sandbox; process-runtime tools only)
- `BAML_SANDBOX_PROVIDER=mock` (fast wiring/dev checks, no VM)
- `BAML_SANDBOX_PROVIDER=microsandbox` (real microVM)

Current network default (microsandbox path):

- sandbox egress policy defaults to `public_only`
- allows public internet egress
- blocks loopback, private RFC1918 ranges, link-local, and cloud metadata endpoints

This default is runtime-level behavior (not currently per-tool configurable in metadata for v1).

Bind-mode note: bind rootfs uses filesystem contents, not full OCI image config.
Do not rely on Dockerfile `ENV` to bootstrap `/tool-adapter`; keep adapter startup
self-contained (or provide explicit runtime env from runner wiring).

### Child process stdio discipline

`/tool-adapter` speaks **length-prefixed framed JSON-RPC over stdout**. The
tool child process spawned for `tool/invoke` therefore must:

- write **only** a single JSON-RPC response to stdout (newline-terminated raw
  JSON-RPC — the adapter re-frames it);
- use **stderr** for logs, diagnostics, or any other output;
- **not** print banners, progress bars, or debug prints to stdout.

Anything emitted to stdout that is not the framed response will corrupt the
channel and surface as `invalid JSON` or truncated-frame errors at the runner.

Bind allowlist (configured via `runner.toml` `[sandbox.bind].roots` or `BAML_SANDBOX_BIND_ROOTS`):

- Empty or unset → **all Bind sources are rejected** (`bind rootfs is disabled`). Safe default for non-dev deployments.
- Narrow the roots. Broad roots (e.g. `/`) are tenant-escape vectors; see §10.

Start the runner with the full feature set (we recommend `--all-features` so sandbox, memory, http-tools, etc. are all available in one process):

```bash
cargo run -p baml-agent-runner --all-features
```

If you only need `microsandbox` and nothing else, `--features sandbox-provider` also works; `--all-features` is the recommended default so you don't need to re-launch when you enable another integration.

---

## 7) Agent wiring + publish/deploy/chat

## 7.1 Allowlist tool in agent manifest

In `agents/<agent>/manifest.json`:

```json
{
  "tools": ["dev/my_tool"]
}
```

The allowlist entry must match the `name` field in the tool's `tool-metadata.json` exactly (`bundle/local_name`). If not allowlisted, tool registration/invocation fails.

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

1. rebuild/export rootfs (or materialize it via your own pipeline)
2. sync runtime lock + digest:

   ```bash
   cargo agent-platform sandbox-bind-sync \
     --tool-dir /path/to/tool \
     --check
   ```

3. restart runner (no hot reload) and re-publish + re-deploy agent

If you skip digest refresh after rootfs mutation, the local runtime lock is stale and the running agent boots from drifted content.

---

## 9) Debugging and troubleshooting

### 9.1 Session-mode runbook (operator quick actions)

Use this when `invocation_mode=session` tools fail in production-like runs.

- **Tool rejected at load (no sandbox wiring):**
  - Cause: runner built without the `sandbox-provider` feature, so no SandboxRuntimeWiring is registered.
  - Action: rebuild the runner with the `sandbox-provider` feature enabled (default) and restart.
  - Verify: resolver no longer emits sandbox-wiring rejection; tool appears in load/resolve logs.

- **`pool_exhausted`:**
  - Cause: all sandboxes for that tool key are live and checkout timed out.
  - Action: retry; if sustained, raise session pool capacity in host config (current knob is pool-wide `SessionPoolConfig.default_pool_max`).
  - Verify: checkout wait and `pool_exhausted` frequency drops.

- **`unknown_session`:**
  - Cause: stale/foreign `session_id` (restart, eviction, or wrong invoker scope).
  - Action: reopen a fresh session; do not reuse old ids across runner restarts.
  - Verify: `session_open -> session_send -> session_read` succeeds with new id.

- **`resume_token_mismatch`:**
  - Cause: incorrect or missing resume token after a `suspended` step.
  - Action: enforce flow `read(suspended) -> send(resume_token) -> read`.
  - Verify: mismatch errors stop and steps progress to `streaming`/`done`.

- **Timeout confusion (`chunk_timeout` vs session lifecycle timeout):**
  - Cause: chunk timeout means “no next step yet”; lifecycle timeout means session-level failure.
  - Action: on chunk timeout, retry/read again in-session; on lifecycle timeout, reopen session.
  - Verify: error code/disposition aligns with expected timeout class.

### Useful debugging helper

Use the shipped inspector to test adapter protocol directly (outside microVM):

```bash
./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py --adapter /path/to/tool-adapter describe
./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py --adapter /path/to/tool-adapter invoke --message "hello"
```

### Common failures

- **"microsandbox support was not built in"**
  - start runner with `--all-features` (or at minimum `--features sandbox-provider`)
- **`bind rootfs is disabled: sandbox.bind_roots is empty`**
  - `BAML_SANDBOX_BIND_ROOTS` is unset/empty; export it before starting the runner
- **`bind path escapes allowlist: <path>`**
  - bind path (after canonicalisation) is not under any `BAML_SANDBOX_BIND_ROOTS` entry
- **`bind path does not resolve: <path>`**
  - the directory doesn't exist or symlink target is missing — re-export rootfs
- **`bind path is not a directory: <path>`**
  - pointed at a file; bind sources must be directories
- **`runtime_digest mismatch for bind source: metadata runtime_digest=... but computed=...`**
  - rootfs mutated since the last sync; rerun `sandbox-bind-sync` to refresh `tool-metadata.lock.json` + sidecar bundle
- **`pull_policy=always is invalid for bind sandbox images`**
  - `PullPolicy::Always` applies only to OCI; drop it or switch source to `oci`
- **`unable to find library -lcap-ng`**
  - install `libcap-ng-dev` + `pkg-config`
- **KVM/unavailable virtualization errors**
  - use `mock` provider or fix host virtualization setup
- **adapter not found at `/tool-adapter`**
  - ensure rootfs/image contains adapter at `/tool-adapter` (or a PATH-resolvable `tool-adapter`)
- **adapter hangs without responding**
  - test the adapter in isolation with `inspect_tsrpc.py` (above) before blaming the VM
- **`resume_token_mismatch`**
  - adapter expects a resume token from a prior `suspended` step; ensure your client flow is `send -> read(suspended) -> send(resume_token) -> read`
- **`unknown_session`**
  - `session_id` is stale/foreign to the invoker instance (often after restart); reopen a new session
- **`pool_exhausted`**
  - session pool cap reached; retry or raise pool capacity in host config (currently pool-wide)

---

## 10) Security notes for Bind (v1)

Bind mode is for pragmatic local/dev workflows and controlled deployments.

Important v1 realities:

- bind rootfs is mutable host state,
- bind rootfs is mounted **read-write** to the guest (microsandbox 0.3.x does not expose an RO-rootfs toggle); if you run multi-tenant, mode the allowlisted root `0555` + root-owned so the guest cannot write back,
- runner enforces only configured allowlist roots; a small TOCTOU window exists between canonicalisation and guest open — restrict allowlisted roots to directories owned exclusively by the runner user (mode `0750`),
- choose narrow allowlist roots (avoid broad shared directories),
- on shared hosts, enforce strict ownership/permissions on allowlisted roots,
- bind reattach is disabled in v1 (cold-create policy) to avoid drift surprises. Within a running process the sandbox cache still serves; the cost is per runner restart.

For stronger immutability and supply-chain posture, prefer OCI digest-pinned images.

Richer hardening (runtime write-probe, copy-on-write staging, overlayfs snapshots, O_NOFOLLOW traversal) is tracked in the refactor plan §10 as follow-ups.

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

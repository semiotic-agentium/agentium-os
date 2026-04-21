# `dev/echo` sandbox demo

Runnable end-to-end example for a sandboxed external tool (`dev/echo`) used by `echo-agent`.

This README is the canonical walkthrough for the echo sandbox demo.

For generic tool guidance, see: `docs/host-tool-guide.md`.

---

## Prerequisites

- Rust + Cargo
- Docker
- `jq`
- Runner host capable of sandbox provider mode (`microsandbox`) when using real microVM path

Ubuntu/Debian package fix for common `-lcap-ng` build/link error:

```bash
sudo apt-get update
sudo apt-get install -y libcap-ng-dev libcap-ng0 pkg-config
```

---

## 1) Build local adapter image

From repo root:

```bash
docker build -t dev-echo-sandbox:local \
  -f examples/external-tools/dev_echo_sandbox/adapter/Dockerfile \
  .
```

---

## 2) Pick demo mode

## Option A — Fast local wiring check (no microVM)

```bash
export BAML_SANDBOX_PROVIDER=mock
```

## Option B — Real microVM with Bind rootfs (recommended)

### 2.1 Export rootfs from image

```bash
./examples/external-tools/dev_echo_sandbox/export_rootfs.sh \
  --image dev-echo-sandbox:local \
  --out ./.tmp/dev-echo-rootfs \
  --force
```

### 2.2 Compute bind digest + patch metadata

```bash
BIND_ROOTFS="$(pwd)/.tmp/dev-echo-rootfs"
TOOL_METADATA="examples/external-tools/dev_echo_sandbox/tool-metadata.json"

DIGEST="$(cargo run -q -p cargo-agent-platform -- sandbox-digest --source bind "$BIND_ROOTFS")"

jq --arg path "$BIND_ROOTFS" --arg digest "$DIGEST" '
  .runtime.image = {"kind":"bind","path":$path}
  | .runtime.entrypoint = ["/tool-adapter"]
  | .runtime_digest = $digest
' "$TOOL_METADATA" > /tmp/dev-echo-tool-metadata.json

mv /tmp/dev-echo-tool-metadata.json "$TOOL_METADATA"
```

### 2.3 Validate metadata

```bash
cargo run -p cargo-agent-platform -- check-external-tool --path examples/external-tools/dev_echo_sandbox
```

Shortcut script for 2.1–2.3:

```bash
./examples/external-tools/dev_echo_sandbox/setup_bind_demo.sh --image dev-echo-sandbox:local --force
```

> Note: committed `tool-metadata.json` is intentionally portable (placeholder bind path + digest).
> The setup flow patches it with your local absolute bind rootfs path and computed digest.
> After demo runs, you can reset it with:
>
> ```bash
> git checkout -- examples/external-tools/dev_echo_sandbox/tool-metadata.json
> ```

---

## 3) Start runner

Set env:

```bash
export BAML_EXTERNAL_TOOLS_DIR="$(pwd)/examples/external-tools/dev_echo_sandbox"
export BAML_SANDBOX_PROVIDER=microsandbox
export BAML_SANDBOX_BIND_ROOTS="$(pwd)/.tmp"
```

> `BAML_SANDBOX_BIND_ROOTS` is colon-separated (`/path1:/path2`) and should be narrow.

Start runner with sandbox feature:

```bash
cargo run -p baml-agent-runner --features sandbox-provider
```

---

## 4) Publish + deploy echo agent

Example agent dir (adjust if your repo layout differs):

```bash
cargo run -p cargo-agent-platform -- publish --agent-dir examples/agents/echo-agent
```

Copy returned hash, then:

```bash
cargo run -p cargo-agent-platform -- deploy --hash <PASTE_HASH_HERE>
```

---

## 5) Verify in chat

```bash
cargo run -p cargo-agent-platform -- list-deployed-instances
cargo run -p cargo-agent-platform -- chat --agent echo-agent
```

Type:

```text
hello-echo!
```

Expected reply shape:

```text
echo returned: hello-echo! [at=<timestamp> pid=<pid> invocation_id=<uuid>]
```

---

## 6) Iterating on adapter/rootfs (Bind mode)

After adapter code changes, re-run this loop:

1. rebuild image
2. re-export rootfs with `--force`
3. recompute digest with `sandbox-digest`
4. patch metadata
5. run `check-external-tool`
6. re-publish + re-deploy agent

If you skip steps 2–4, you may run with stale rootfs identity.

---

## 7) Debugging helper

Use protocol inspector to test adapter directly (outside microVM):

```bash
./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py \
  --adapter ./.tmp/dev-echo-rootfs/tool-adapter describe

./examples/external-tools/dev_echo_sandbox/inspect_tsrpc.py \
  --adapter ./.tmp/dev-echo-rootfs/tool-adapter invoke \
  --message "hello-echo?"
```

This is useful to isolate protocol issues from sandbox boot/runtime issues.

---

## Notes / gotchas

- Tool name in metadata must remain `dev/echo` to match `echo-agent` references.
- Bind mode is denied unless bind path is under `BAML_SANDBOX_BIND_ROOTS`.
- Bind reattach is disabled in v1 (cold-create behavior) because bind rootfs is mutable.
- Bind v1 security depends on path allowlist + operator discipline on directory ownership/permissions.

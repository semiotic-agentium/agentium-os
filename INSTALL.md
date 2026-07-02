# Installing Agentium OS

Prebuilt release tarballs ship a single Linux binary:

| Binary | Role |
|--------|------|
| `agentium` | Platform host (`agentium serve`) and developer SDK (scaffold, install, chat, eval) |

**Supported platforms (v1):** `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.

---

## Prerequisites

### Runtime libraries

```bash
# Debian/Ubuntu — runner feature set (http-tools, memory, sandbox-provider)
sudo apt install -y libdbus-1-3 libcap-ng0 ca-certificates
```

`libcap-ng0` is required for the microVM sandbox provider (`sandbox-provider` feature). Confirm linkage with `ldd ./agentium | grep -E 'dbus|cap'`.

### Publishing agents — Node.js 22+ and TypeScript 6

When **`agentium serve`** builds source via `POST /repository/publish`, the embedded builder runs `tsc`. Install TypeScript 6 on the **serve host**:

```bash
npm install -g typescript@6
```

**Not** needed to deploy a pre-built artifact (`POST /deploy`) or to **run** deployed agents (QuickJS runtime). A runner that only deploys pre-built blobs needs no Node/tsc.

### Build from source (`cargo install`, local clone)

```bash
sudo apt install -y libdbus-1-dev libcap-ng-dev pkg-config ca-certificates
just check-host
```

### Embedding models (optional)

- Clone with Git LFS: `git lfs pull`
- Or set `BAML_MODELS_DIR` to a directory containing fastembed ONNX models (see `just download-models`)

---

## Option A — GitHub Release (recommended)

1. Open [Releases](https://github.com/semiotic-agentium/agentium-os/releases) and pick a version.
2. Download the tarball for your architecture.
3. Verify the checksum.
4. Extract and install:

```bash
VERSION=v0.1.1   # replace with your release
ARCH="$(uname -m)"
tar -xzf "agentium-os-${VERSION}-${ARCH}-unknown-linux-gnu.tar.gz"
sudo install -m 755 agentium /usr/local/bin/
```

5. Confirm version:

```bash
agentium --version
```

---

## Option B — `cargo install --git` (compile from source)

Requires Rust and [build prerequisites](#build-from-source-cargo-install-local-clone). Prefer Option A for end users.

```bash
cargo install --git https://github.com/semiotic-agentium/agentium-os --tag vX.Y.Z \
  --features http-tools,memory,sandbox-provider,dev-tools agentium
```

Ensure `~/.cargo/bin` is on your `PATH`.

---

## Option C — build from a local clone

```bash
just check-host
just build-release   # release agentium binary
```

Binary lands in `target/release/agentium`.

---

## Quick start after install

```bash
# Terminal 1 — start platform
agentium serve --serve-http 127.0.0.1:18080 --repository-dir ./.repository --state-dir ./.runner-state

# Terminal 2 — publish + deploy from agent source
agentium init --with-agent --agent-name my-agent
agentium install agent --path ./my-agent
agentium chat --agent my-agent
```

Further reading:

- [`docs/reference/agentium-cli.md`](docs/reference/agentium-cli.md) — CLI subcommands (`serve`, install, chat, eval, …)

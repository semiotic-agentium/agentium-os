# Installing Agentium OS binaries

Prebuilt release tarballs ship three Linux binaries:

| Binary | Role |
|--------|------|
| `baml-agent-runner` | HTTP A2A host — run the platform |
| `baml-agent-builder` | Package agents locally (compile, type-check, tar.gz) |
| `cargo-agent-platform` | SDK CLI — scaffold, publish, deploy (`cargo agent-platform …`) |

**Supported platforms (v1):** `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.

---

## Prerequisites

### Runtime libraries

Dynamic libraries linked into the prebuilt binaries. The scope differs per binary:

```bash
# Debian/Ubuntu — superset that covers all three (i.e. what the runner needs)
sudo apt install -y libdbus-1-3 libcap-ng0 ca-certificates
```

| Binary | Runtime libs | Why |
|--------|--------------|-----|
| `baml-agent-runner` | `libdbus-1-3`, `libcap-ng0`, `ca-certificates` | D-Bus: keyring (`memory`); libcap-ng: microVM sandbox capability management (`sandbox-provider`) |
| `baml-agent-builder` | `libdbus-1-3`, `ca-certificates` | D-Bus keyring (`memory`); **no libcap-ng** (no sandbox) |
| `cargo-agent-platform` | `ca-certificates` | HTTPS only |

`libcap-ng0` is required **only by the runner** — it's the one binary built with `sandbox-provider` (pulled in via `microsandbox` → `capng`). Confirm what a given build actually links with `ldd ./baml-agent-runner | grep -E 'dbus|cap'`.

### Building or publishing agents — Node.js 22+ and TypeScript 6

Packaging an agent compiles its TypeScript with `tsc` (falling back to `npx tsc`), so a **TypeScript 6 compiler must be on `PATH`** wherever a *build* happens:

```bash
# TypeScript 6 on PATH (requires Node.js 22+)
npm install -g typescript@6
```

Needed by:

- `baml-agent-builder package` / `publish` — always; it *is* the build tool.
- `baml-agent-runner` **only when it builds source** via `POST /repository/publish` — the runner embeds the builder and shells out to `tsc`.

**Not** needed to deploy a pre-built artifact (`POST /deploy`) or to **run** agents: deployed agents execute in the embedded QuickJS engine, which needs no Node or TypeScript at runtime. A runner that only deploys pre-built blobs and serves agents needs no `tsc`/Node on its host.

### Build from source (`cargo install`, local clone)

Add development headers and `pkg-config` (plus the TypeScript compiler above):

```bash
# Debian/Ubuntu
sudo apt install -y libdbus-1-dev libcap-ng-dev pkg-config ca-certificates
```

Validate host deps from a clone:

```bash
just check-host
```

### Embedding models (optional)

For runner drift / embedding features:

- Clone with Git LFS: `git lfs pull`
- Or set `BAML_MODELS_DIR` to a directory containing fastembed ONNX models (see `just download-models` in the repo)

---

## Option A — GitHub Release (recommended)

1. Open [Releases](https://github.com/semiotic-agentium/agentium-os/releases) and pick a version.
2. Download the tarball for your architecture:

   | Machine | Asset name |
   |---------|------------|
   | amd64 | `agentium-os-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |
   | arm64 | `agentium-os-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz` |

3. Verify the checksum (per-archive `SHA256SUMS` inside the tarball, or the aggregate file on the Release page).
4. Extract and install:

```bash
VERSION=v0.1.1   # replace with your release
ARCH="$(uname -m)"
tar -xzf "agentium-os-${VERSION}-${ARCH}-unknown-linux-gnu.tar.gz"
sudo install -m 755 baml-agent-runner baml-agent-builder cargo-agent-platform /usr/local/bin/
```

5. Confirm versions (all three should report the same SemVer):

```bash
baml-agent-runner --version
baml-agent-builder --version
cargo agent-platform --version
```

---

## Option B — `cargo install --git` (compile from source)

Requires a Rust toolchain and the [build prerequisites](#build-from-source-cargo-install-local-clone) above. Expect long compile times (~30+ minutes for a release runner build). Prefer Option A for end users.

Replace `vX.Y.Z` with the tag you want:

```bash
# SDK CLI (invoked as `cargo agent-platform`)
cargo install --git https://github.com/semiotic-agentium/agentium-os --tag vX.Y.Z cargo-agent-platform

# Runner
cargo install --git https://github.com/semiotic-agentium/agentium-os --tag vX.Y.Z \
  --features http-tools,memory,sandbox-provider baml-agent-runner

# Builder
cargo install --git https://github.com/semiotic-agentium/agentium-os --tag vX.Y.Z \
  --features http-tools,memory baml-rt-builder --bin baml-agent-builder
```

Ensure `~/.cargo/bin` is on your `PATH`.

---

## Option C — build from a local clone

For contributors or offline builds:

```bash
just check-host
just build-release                              # runner + builder (release)
cargo build --release -p cargo-agent-platform   # SDK CLI
```

Binaries land in `target/release/`.

---

## Quick start after install

```bash
# Terminal 1 — start runner
baml-agent-runner --serve-http 127.0.0.1:18080

# Terminal 2 — publish + deploy an agent
cargo agent-platform push --agents agents/my-agent
# or: baml-agent-builder publish --agent-dir agents/my-agent --deploy-url http://127.0.0.1:18080
```

Further reading:

- [`docs/reference/agent-runner.md`](docs/reference/agent-runner.md) — runner HTTP API and flags
- [`docs/reference/sdk-cli.md`](docs/reference/sdk-cli.md) — SDK subcommands

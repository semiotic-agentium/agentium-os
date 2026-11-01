# Karakara CLI Reference

The unified **`karak`** binary is the only user-facing entrypoint for Karakara OS:

- **`karak serve`** — run the platform (HTTP A2A, repository, provenance, config)
- **SDK subcommands** — scaffold, install, chat, eval, registry, sandbox tooling

For HTTP API details (deploy, A2A, auth tiers), see [`agent-runner.md`](agent-runner.md).

## Installation

See [`INSTALL.md`](../../INSTALL.md). From a clone:

```bash
cargo build --release -p karakara-os --all-features
./target/release/karak --help
```

Local dev shortcuts: `just build`, `just runner` (build + start on `127.0.0.1:18080`).

## Migration from `baml-agent-runner`

| Before | After |
|--------|-------|
| `baml-agent-runner --serve-http …` | `karak serve --serve-http …` |

`build`, `regen`, and `push` are not registered on `karak`.

## Project config

`karak init` creates `karakara.toml`:

```toml
[runner]
url = "http://127.0.0.1:18080"
token_env = "RUNNER_TOKEN"

[project]
default_agent = "my-agent"
agent_path = "./my-agent"
```

Resolution order: `--config` flag → `./karakara.toml` → `~/.config/karakara/config.toml`.

```bash
karakara config show
karakara config set runner.url http://127.0.0.1:18080
```

## Platform host

```bash
karak serve --serve-http 127.0.0.1:18080 \
  --repository-dir ./.repository \
  --state-dir ./.runner-state \
  --provenance-db ./provenance.db
```

All former `baml-agent-runner` flags are available on `serve` (flattened runner CLI). Set `RUNNER_TOKEN` (or pass via env) when operator routes should require auth.

## Primary developer loop

```bash
karak init --with-agent --agent-name support-bot --tags support,demo
karak skill install agent
# edit agent source only (manifest, baml_src/, src/)
karak install agent --path ./support-bot
karakara sync-types
karak chat --agent support-bot --instance default
karak eval run --min-pass-rate 0.9
```

| Command | Purpose |
|---------|---------|
| `install agent` | `POST /repository/publish` (server build) + `POST /deploy` |
| `install tool` | Approve and register an external tool snapshot |
| `publish` | Publish source only (no deploy); prints `karak deploy …` hint |
| `deploy --hash HASH --url URL` | Activate a published artifact |
| `sync-types` | Pull `_baml_runtime.baml` + `baml-runtime.d.ts` from `GET /repository/dev-artifacts` |

## Scaffolding

```bash
karak new-agent my-agent --template simple --tags support,demo
karak tool new weather --bundle support    # v2 TypeScript tool (@karakara/tool)
karak tool build ./weather                 # derive descriptor + digest
karak tool test ./weather --input '{"message":"hi"}'
karak new-static-tool github   # platform-internal (monorepo contributors only)
```

Interactive mode: omit positional names (`karak new-agent`, `karak tool new`, …).

## Eval harness

```bash
karak eval init                    # writes eval/cases.toml from example
karak eval run --min-pass-rate 0.9
karak eval run --cases lifecycle-demo,smoke-one-shot
karak eval report                # prints eval/out/last-run.json
```

Eval talks to a **running** runner over HTTP:

1. **`POST /eval/sessions`** (operator auth) — optional ephemeral model override scope; eval run sends `X-Karakara-Eval-Session` on A2A turns.
2. **`POST /agents/{agent}/a2a`** — chat turns via `message.sendStream` (JSON-RPC `id` must use `corr-<millis>-<counter>` format).
3. **`POST /events/publish`** — ingress turns (fixture JSON on disk).

Manifest syntax: `crates/karakara/eval/cases.toml.example`. Turn modes:

- **`chat`** (default) — A2A user message; assert on task states and streamed text.
- **`ingress`** — publish host event fixture; assert on `contains` / `not_contains` in the publish response (non-empty `failures` fails the turn).

Flags: `--url`, `--model`, `--deploy` (publish+deploy agent path before eval), `--path`, `--runner-token`.

## Registry, sandbox, and diagnostics

Unchanged in role from the former SDK:

- `external-tool enable|inspect|refresh`
- `mcp list|enable|server|versions|tool`
- `export-snapshot-cache`, `snapshot-report`, `doctor`, `list-*`

(The v1 `sandbox-bind-sync`, `sandbox-oci-prepare`, and `check-external-tool`
commands were retired with the v1 bind/rootfs scaffold — external tools v2 uses
`karak tool new|build|test` instead.)

## Skills

```bash
karak skill install agent   # Cursor skill: agent authoring
karak skill install tool    # Cursor skill: external tool authoring
```

Bundled skills live under `crates/karakara/skills/`.

## Authentication

Operator routes accept `X-Runner-Token` or `--runner-token` / `RUNNER_TOKEN` env (see `karakara.toml` `runner.token_env`).

Public routes (no token): discovery, A2A, `/events/publish`, read-only `/repository/*` including **`GET /repository/dev-artifacts`**.

## Server-generated dev artifacts

On **`POST /repository/publish`**, the runner captures `_baml_runtime.baml` and `src/baml-runtime.d.ts` from the build workspace and stores them in the repository blob store (keyed by content hash).

```bash
# After publish
karakara sync-types --path ./my-agent
# or
curl 'http://127.0.0.1:18080/repository/dev-artifacts?agent=my-agent'
curl 'http://127.0.0.1:18080/repository/dev-artifacts?hash=<content-hash>'
```

Response `status`: `ok` | `not_found` | `not_implemented` (hash unknown or artifacts not yet published).

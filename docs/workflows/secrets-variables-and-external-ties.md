# Secrets, Variables & External Ties Reference

This document catalogs every GitHub Actions secret, repository variable, local secret-store key ([`fnox.toml`](../../fnox.toml) / [`.env.example`](../../.env.example)), and external dependency tied to the repository, along with recommended post-fork alternatives.

---

## 1. GitHub Actions Secrets (`secrets.*`)

Configured under **Settings → Secrets and variables → Actions → Secrets**:

| Secret Name | Referenced In | Required For | Fallback / Behavior When Missing | `$0` / Low-Cost Alternative |
|---|---|---|---|---|
| `OPENROUTER_API_KEY` | [`rust-ci.yml`](../../.github/workflows/rust-ci.yml), [`claude-md-sync.yml`](../../.github/workflows/claude-md-sync.yml), [`fnox.toml`](../../fnox.toml) | Live LLM integration tests (`llm-tests` Cargo feature) and automated doc-sync PR generation | `if_missing = "warn"` in `fnox.toml`. Non-LLM tests (`just test-unit`) do not use it. | Keep `llm-tests` and `claude-md-sync` disabled in CI; use local unit/snapshot tests or a local OpenAI-compatible server for development. |
| `CLICKUP_API_KEY` | [`rust-ci.yml`](../../.github/workflows/rust-ci.yml), [`fnox.toml`](../../fnox.toml) | Live ClickUp tool / `clickup-agent` integration tests | `if_missing = "ignore"` in `fnox.toml`. Offline tests use recorded fixtures. | Not needed in CI unless testing live ClickUp API calls. |
| `NOTION_API_TOKEN` | [`rust-ci.yml`](../../.github/workflows/rust-ci.yml), [`fnox.toml`](../../fnox.toml) | Live Notion tool / `notion-agent` integration tests | `if_missing = "ignore"` in `fnox.toml`. Offline tests use recorded fixtures. | Not needed in CI unless testing live Notion API calls. |
| `SLACK_BOT_TOKEN` | [`rust-ci.yml`](../../.github/workflows/rust-ci.yml), [`fnox.toml`](../../fnox.toml) | Live Slack bot-scoped read tests (`xoxb-...`) | `if_missing = "ignore"` in `fnox.toml`. | Optional; use recorded fixtures or a free Slack developer sandbox workspace. |
| `SLACK_USER_TOKEN` | [`rust-ci.yml`](../../.github/workflows/rust-ci.yml), [`fnox.toml`](../../fnox.toml) | Live Slack user-scoped search tests (`xoxp-...`) | `if_missing = "ignore"` in `fnox.toml`. | Optional; use recorded fixtures or a free Slack developer sandbox workspace. |

> [!NOTE]
> `GITHUB_TOKEN` (`${{ github.token }}`) is provided automatically by GitHub Actions at `$0` cost and is the only token required by `release-please.yml`, `release-pr-postprocess.yml`, and `release-publish.yml`.

---

## 2. GitHub Repository Variables (`vars.*`)

Configured under **Settings → Secrets and variables → Actions → Variables**:

| Variable Name | Referenced In | Default Fallback | Purpose |
|---|---|---|---|
| `BAML_TEST_MODEL` | [`rust-ci.yml`](../../.github/workflows/rust-ci.yml) (lines 60, 189), [`fnox.toml`](../../fnox.toml) | `x-ai/grok-4.3` | Controls which model ID BAML (`env.BAML_TEST_MODEL`) and Rust (`test_model_default()`) use during `llm-tests`. Can be overridden to a lower-cost model (e.g. `google/gemini-2.5-flash` or `openai/gpt-4.1-mini`). |

---

## 3. Local & Runtime Environment Variables (`.env.example` / `scripts/`)

| Variable(s) | Where Used | Purpose | Post-Fork Guidance |
|---|---|---|---|
| `TASK_DAEMON_LLM_BASE_URL`, `TASK_DAEMON_LLM_API_KEY`, `TASK_DAEMON_LLM_MODEL` | [`.env.example`](../../.env.example), `crates/task-daemon/` | Primary OpenAI-compatible LLM endpoint for `task-daemon` (defaults to `openai/gpt-4.1-mini`). | Can point to any OpenAI-compatible provider or local server. |
| `TASK_DAEMON_LLM_FALLBACK_BASE_URL`, `TASK_DAEMON_LLM_FALLBACK_MODEL` | [`.env.example`](../../.env.example), `crates/task-daemon/` | Local OpenAI-compatible fallback (`http://localhost:1234/v1`). | Supports `$0` local inference via **Ollama**, **LM Studio**, or **vLLM**. |
| `TYPESAFE_API_KEY`, `TYPESAFE_API_BASE_URL`, `TYPESAFE_MODEL` | [`scripts/capture_typesafe_system_one_fixtures.sh`](../../scripts/capture_typesafe_system_one_fixtures.sh) | Helper script to capture live fixtures from `https://api.typesafe.ai`. | Committed fixtures already live under `crates/tools/typesafe/tests/fixtures/`; this key is never needed in CI or normal builds. |
| `BAML_MODELS_DIR` | [`.env.example`](../../.env.example), [`Dockerfile`](../../Dockerfile) | Directory containing local ONNX models (`./models`). | Defaults to `./models` when running from the workspace. |

---

## 4. External Infrastructure & Code Dependencies Outside GitHub Actions

### 1. External BAML Git Dependency (`ryan-s-roberts/baml`)
- **Location:** [`Cargo.toml`](../../Cargo.toml) (`[workspace.dependencies]`):
  ```toml
  baml-runtime = { git = "https://github.com/ryan-s-roberts/baml", branch = "canary", default-features = false, features = ["minimal-runtime", "internal"] }
  baml-types = { git = "https://github.com/ryan-s-roberts/baml", branch = "canary" }
  internal-baml-core = { git = "https://github.com/ryan-s-roberts/baml", branch = "canary" }
  internal-llm-client = { git = "https://github.com/ryan-s-roberts/baml", branch = "canary" }
  ```
- **Risk:** Depends on a personal GitHub repository outside `KarakaraLabs` (`pinned` in `Cargo.lock` to commit `bf2d0c33facfc46d7a4c8a3ae96369cf8395eb91`).
- **Recommendation:** Fork `https://github.com/ryan-s-roberts/baml` (preserving the `canary` branch and commit `bf2d0c33facfc46d7a4c8a3ae96369cf8395eb91`) into **`https://github.com/KarakaraLabs/baml`** and update `Cargo.toml` so `KarakaraLabs` controls the upstream source.

### 2. Container Registry Placeholders (`ghcr.io/KarakaraLabs/*`)
- **Location:**
  - [`deploy/values/dev/images.yaml`](../../deploy/values/dev/images.yaml) & [`scripts/release/set-deploy-ref.sh`](../../scripts/release/set-deploy-ref.sh) (`ghcr.io/KarakaraLabs/karakara-runner`)
- **Guidance:** Local Kubernetes testing (`just up`, `just sync`, `just verify-k8s-pilot-package`) pushes images to a local `k3d` registry and does not require GHCR. When remote cluster deployments are needed, GHCR hosting is free for public packages under `KarakaraLabs` using `GITHUB_TOKEN` (`packages: write`).

### 3. Git LFS Quota (`models/fastembed/`)
- **Location:** [`models/fastembed/`](../../models/fastembed/), [`Dockerfile`](../../Dockerfile), and [`k8s-pilot-validation.yml`](../../.github/workflows/k8s-pilot-validation.yml).
- **Guidance:** GitHub's free Git LFS plan includes 1 GB storage and 1 GB/month bandwidth. Avoid running `git lfs pull` in routine CI workflows to prevent exhausting free monthly LFS bandwidth, and evaluate removing the vendored ONNX model copy from `Dockerfile` since [`Cargo.toml`](../../Cargo.toml) notes that citation integrity replaced ONNX drift scoring.


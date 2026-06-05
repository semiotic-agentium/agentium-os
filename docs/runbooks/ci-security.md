# CI security (public repository)

Agentium OS is public. GitHub Actions must treat **fork pull requests** as
untrusted: they can change any file in the PR branch, including workflow
definitions in the fork (upstream workflow files still govern what runs, but
the **checked-out commit** is attacker-controlled).

## Trust model

| Trigger | Trust | Runners | Secrets | Full test lanes |
|---------|-------|---------|---------|-----------------|
| Push to `main` | Trusted | Self-hosted (`arc-runner-set`, `kvm-enabled`) | Yes | Yes |
| `workflow_dispatch` | Trusted | Self-hosted | Yes | Yes |
| PR from a branch **in this repo** | Trusted | Self-hosted | Yes | Yes |
| PR from a **fork** | Untrusted | GitHub-hosted (`ubuntu-latest`) only | No | Reduced (no `llm-tests`, no k8s pilot) |

**Trusted** is computed as:

```text
event != pull_request  OR  pull_request.head.repo == github.repository
```

Workflows:

- [`.github/workflows/rust-ci.yml`](../../.github/workflows/rust-ci.yml) — trusted vs `nextest-untrusted` / `doctor-untrusted`
- [`.github/workflows/k8s-pilot-validation.yml`](../../.github/workflows/k8s-pilot-validation.yml) — k8s validate skipped on fork PRs; full run on `main` after merge

Always safe on fork PRs (already on `ubuntu-latest`, no secrets):

- `secret-scan.yml` (gitleaks)
- `reuse.yml`

Never fork-triggered:

- `claude-md-sync.yml` (push to `main` only; uses `OPENROUTER_API_KEY` + `contents: write`)
- `sandbox-e2e-kvm.yml` (`workflow_dispatch` / weekly cron; `kvm-enabled` runners)
- `release-tag-verify.yml` (tag push only)

## Required GitHub settings

Configure these in the **organization** or **repository** settings (Settings →
Actions → General). Operators should verify after making the repo public.

### 1. Fork pull request workflows

**Actions → General → Fork pull request workflows**

- Enable: **Require approval for all outside collaborators** (recommended)

First-time contributors from forks need a maintainer to approve before any
workflow runs. After approval, only the **untrusted** lane runs (GitHub-hosted,
no upstream secrets).

### 2. Workflow permissions

**Actions → General → Workflow permissions**

- Recommended: **Read repository contents and packages permissions**
- Do **not** grant default `write` unless a workflow explicitly needs it (only
  `claude-md-sync` on `main` uses elevated permissions in its job).

### 3. Branch protection on `main`

**Settings → Branches → Branch protection rules**

Suggested required status checks:

- `Rust CI / Nextest (workspace)` (either trusted or untrusted job satisfies the name)
- `reuse / reuse`
- `secret-scan / gitleaks`

Optional (heavy; runs on trusted CI and on every push to `main`):

- `K8s Pilot Validation / Helm install + registry-backed package validation`

Fork PRs **do not** run k8s pilot validation. Rely on merge-to-`main` push
validation or ask a maintainer to run `workflow_dispatch` on a trusted branch.

### 4. Self-hosted runners

**Actions → Runners**

- Restrict runner groups (`arc-runner-set`, `kvm-enabled`) to **this repository
  only** (not org-wide shared with untrusted repos).
- Prefer **ephemeral** runners or regular image rebuilds.
- Runners must not mount production credentials, kubeconfigs, or long-lived
  `.env` / `fnox.toml` files.
- Untrusted workflows must **never** use self-hosted labels (enforced in
  workflow `if:` conditions).

### 5. Secrets and environments

**Settings → Secrets and variables → Actions**

- Keep `OPENROUTER_API_KEY`, Slack/Notion/ClickUp tokens in **Actions secrets**
  (never commit).
- Optional hardening: move LLM secrets into a **`llm-tests` environment** with
  **required reviewers** so only trusted workflows target that environment.

### 6. Dependabot / third-party actions

- Pin major action versions (`@v4`, `@v5`) as today.
- Review new workflow changes in PRs that touch `.github/workflows/**`.

## Maintainer playbook

### External fork PR

1. Review diff (especially `.github/` and `scripts/`).
2. Approve workflow run if prompted.
3. Wait for **untrusted** CI (fmt, clippy, nextest without LLM, reuse, gitleaks).
4. For k8s or LLM coverage before merge: check out branch locally, or merge
   to a trusted branch and use `workflow_dispatch`.

### Same-repo branch PR (collaborator)

- Full trusted CI runs automatically, including secrets and self-hosted runners.
- Treat as **trusted code** only for collaborators with write access; still review
  before merge.

### Incident: suspected secret exfiltration

1. Rotate all Actions secrets immediately.
2. Revoke and re-issue any PATs used on self-hosted runners.
3. Audit runner VM images and disconnect from production networks.
4. Search workflow logs for unexpected `curl` / `POST` steps added in the PR.

## Related docs

- [Contributing — CI for fork PRs](../../CONTRIBUTING.md#continuous-integration-fork-prs)
- [Testing handbook](../assertions/testing-handbook.md)

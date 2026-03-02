# Slack Tool (`support/slack`)

This repository now includes a read-only Slack host tool and `agents/slack-agent` for source-backed todo extraction.

## SAML + Auth Model

SAML does not change Slack API authentication mechanics for this integration.

- Users/admins authenticate to Slack through SAML during app install/authorization.
- API calls are made with Slack OAuth tokens (`xoxb` and/or `xoxp`), not username/password scraping.
- For Business+ and user-scoped least privilege, prefer user tokens for user-scoped reads.

## Required Scopes

Minimum read-only scope set:

- `channels:read`
- `groups:read`
- `im:read`
- `mpim:read`
- `channels:history`
- `groups:history`
- `im:history`
- `mpim:history`
- `users:read`

Nice-to-have:

- `search:read` (required for `SearchMessagesInput`)
- `users:read.email` (only if email-based identity mapping is needed)

Reference manifest: [slack-app-manifest.example.yaml](/Users/joseph/git/semiotic-agentium/agent-platform/docs/slack-app-manifest.example.yaml)

## Environment Variables

Tool runtime:

- `SLACK_BOT_TOKEN` (recommended default token for read flows)
- `SLACK_USER_TOKEN` (required for search, recommended for user-scoped access)
- `SLACK_API_BASE_URL` (optional; default `https://slack.com/api`; supports overrides such as GovSlack API domains)

OAuth helper / install flow:

- `SLACK_APP_CLIENT_ID`
- `SLACK_APP_CLIENT_SECRET`
- `SLACK_REDIRECT_URI`
- `SLACK_OAUTH_BASE_URL` (optional; default `https://slack.com`)

Optional metadata:

- `SLACK_TEAM_ID`
- `SLACK_ENTERPRISE_ID`

## OAuth Helper

Script: [slack-oauth-helper.sh](/Users/joseph/git/semiotic-agentium/agent-platform/scripts/slack-oauth-helper.sh)

Generate install URL:

```bash
SLACK_APP_CLIENT_ID=... \
SLACK_REDIRECT_URI=https://localhost:8787/slack/oauth/callback \
scripts/slack-oauth-helper.sh install-url
```

Exchange code for tokens:

```bash
SLACK_APP_CLIENT_ID=... \
SLACK_APP_CLIENT_SECRET=... \
SLACK_REDIRECT_URI=https://localhost:8787/slack/oauth/callback \
scripts/slack-oauth-helper.sh exchange-code --code "<oauth-code>"
```

The helper prints `export` statements for `SLACK_BOT_TOKEN`, `SLACK_USER_TOKEN`, and team/org IDs when available.

## Demo Paths

Interactive runner:

```bash
just slack-agent
just slack-agent-provenance
```

HTTP demo script:

```bash
SLACK_DEMO_THREAD_URL="https://<workspace>.slack.com/archives/C123.../p1735689600000000" just slack-demo
```

## Known Limitations (MVP)

- Read-only only (no `chat.postMessage`, edits, or deletes).
- Search requires `SLACK_USER_TOKEN` + `search:read`.
- Token rotation is not automated in the tool runtime yet; re-run OAuth exchange for refresh/rotation as an operational process.
- Todo extraction in `agents/slack-agent` is deterministic/rule-based for auditable mock-backed testing.

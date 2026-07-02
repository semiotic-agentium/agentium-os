---
name: agentium-tool-authoring
description: Author external Agentium tools — tool-manifest.json, JSON-RPC handlers, FSM transitions. Use when scaffolding host tools outside the monorepo.
---

# Agentium tool authoring

## When to apply

- Creating external tools (`tool-manifest.json`, handler source)
- Enabling tools on a running instance
- Referencing tools from agent manifests

## Workflow

1. `agentium new tool <name>`
2. `agentium skill install tool` (this skill)
3. Implement handlers; `agentium check tool <dir>`
4. `agentium install tool <dir>` — enables on runner registry
5. Add exact tool name to agent `manifest.json` → `agentium install agent`

## Hard rules

- `tool-manifest.json` name must match agent manifest allowlist exactly
- Session tools follow Open → Send → SearchRead → Finish/Abort FSM
- Return structured JSON at tool boundaries

## Commands

| Command | Purpose |
|---------|---------|
| `agentium install tool <dir>` | Enable external tool on runner |
| `agentium check tool <dir>` | Validate manifest locally |

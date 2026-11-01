---
name: karakara-tool-authoring
description: Author typed external Karakara tools with @karakara/tool. Use when scaffolding, testing, or publishing a tool outside the monorepo.
---

# Karakara tool authoring

## When to apply

- Creating a TypeScript external tool
- Declaring input, output, network, or secret requirements
- Publishing a tool for use by an agent

## Workflow

1. Run `karak tool new <name> --bundle <bundle>`.
2. Install dependencies with `npm install`.
3. Implement the typed `defineTool` function in `src/tool.ts`.
4. Run `karak tool test --input '<json>'`.
5. Run `karak publish`. The CLI derives the descriptor, vendors the JavaScript, builds a `.ktool`, asks for approval, and uploads it to the repository.
6. Pin the published tool name and digest from the agent.

## Rules

- Treat the input and output TypeBox schemas as the contract. Do not write a separate manifest or schema file.
- Declare every outbound host and secret in `defineTool`. The declaration requests access; the host decides the grant.
- Never read secret values in author code. The host binds them to approved egress.
- Return structured JSON that matches the output schema.
- Use `ctx.error(...)` to classify expected failures.

## Commands

| Command | Purpose |
|---------|---------|
| `karak tool new <name>` | Create a typed TypeScript tool |
| `karak tool build [dir]` | Compile and derive `tool-descriptor.json` |
| `karak tool test [dir] --input '<json>'` | Invoke through the stdio DevBackend |
| `karak tool publish [dir]` | Build, approve, and ingest a `.ktool` |
| `karak publish` | Auto-detect a tool project in the current directory and run the same publish path |

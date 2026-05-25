# Agentium Observability Demo Helm Chart

Phase 0 scaffold. Full templates land in later implementation phases.

Planned install:

```bash
helm upgrade --install agentium-observability-demo ./demo/ford-observability/helm \
  --namespace agentium-demo \
  --create-namespace \
  --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
```

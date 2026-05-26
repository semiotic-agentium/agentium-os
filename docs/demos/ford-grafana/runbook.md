# Ford Grafana Demo Runbook

Primary runbook lives in `demo/ford-observability/README.md` so demo artifact stays self-contained.

Quick path:

```bash
demo/ford-observability/demo.sh install --set secrets.openrouterApiKey="$OPENROUTER_API_KEY"
kubectl -n agentium-demo port-forward svc/grafana 3000:3000
kubectl -n agentium-demo port-forward svc/agentium-runner 18080:18080
demo/ford-observability/demo.sh inject
```

Open:

- Grafana: <http://127.0.0.1:3000>
- Agentium dashboard: `http://127.0.0.1:18080/?view=dashboard&contextId=<context_id>`

Framing: metrics/logs are live; trace evidence is synthetic Grafana annotation records, not Tempo. Final report lives in Agentium provenance, not Grafana.

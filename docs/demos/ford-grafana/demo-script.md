# Ford Grafana Demo Script

Use `demo/ford-observability/README.md` as operator source of truth.

Narration beats:

1. Show Grafana baseline: checkout/payments healthy, k6 load steady.
2. Inject latency: `demo/ford-observability/demo.sh inject`.
3. Grafana alert fires from Prometheus metric.
4. Agentium runner receives webhook and opens investigation context.
5. Coordinator delegates to Grafana investigator.
6. Investigator gathers Prometheus metrics, Loki logs, synthetic trace annotations.
7. Coordinator writes final report to provenance and Slack notifier posts pointer.
8. Open Agentium dashboard/provenance by `context_id`.
9. Reset: `demo/ford-observability/demo.sh reset`.

Presenter note: Grafana = telemetry/evidence timeline. Agentium dashboard = investigation/report. Grafana annotations do not contain report body.

/**
 * Static, in-repo sample catalog for the Event Console MVP.
 *
 * Each entry describes the event-shape an operator can dispatch through
 * POST /agents/{pkg}/{inst}/dispatch. Samples are starting points; the
 * operator can edit the JSON before sending.
 *
 * The Ford IncidentRaised sample is a sample, not a real FireHydrant
 * integration. See issues #512, #519, #520, #521.
 */

/** Field shape the dispatch endpoint accepts (see crates/baml-rt-api openapi.rs). */
export interface AgentDispatchRequestBody {
  routing_key: string;
  message_type: string;
  messages: unknown[];
  context_id?: string;
  task_id?: string;
  message_id?: string;
  metadata?: Record<string, unknown>;
}

export interface EventSample {
  /** Unique key for selecting this sample. */
  id: string;
  /** Operator-facing label. */
  label: string;
  /** Short purpose line (rendered under the label). */
  summary: string;
  /** Optional source family label (`firehydrant`, `dispatch-echo`, ...). */
  sourceKind: string;
  /** routing_key the dispatch will use; agent subscriptions are matched against this. */
  routingKey: string;
  /** message_type / schema-version identifier carried on the wire. */
  messageType: string;
  /** messages[] payload — what the agent's onDispatch() receives. */
  messages: unknown[];
  /** Extra metadata merged on top of the operator-eval-console origin. */
  extraMetadata?: Record<string, unknown>;
  /** Optional sample-specific hint shown above the JSON editor. */
  notes?: string;
}

/**
 * Smoke sample compatible with the dispatch-echo fixture's subscriptions.
 * Matches schema `task-daemon.interpretation.v1`, source_kind `slack`.
 */
export const DISPATCH_ECHO_SMOKE: EventSample = {
  id: "dispatch-echo-smoke",
  label: "dispatch-echo smoke",
  summary: "Round-trip through the dispatch-echo fixture's onDispatch.",
  sourceKind: "slack",
  routingKey: "slack:intake",
  messageType: "task-daemon.interpretation.v1",
  messages: [
    {
      source_kind: "slack",
      source_key: "C0123/1700000000.000100",
      records: [
        {
          channel: "#incident-room",
          user: "U-operator",
          text: "Smoke-test event from the Event Console.",
          ts: "1700000000.000100",
        },
      ],
      messages_scanned: 1,
      derived_tasks: [],
    },
  ],
  notes: "dispatch-echo subscribes to task-daemon.interpretation.v1 / source_kind=slack.",
};

/**
 * Build the Ford-shaped IncidentRaised sample with timestamps relative to `now`.
 *
 * Captured at module-load so the sample reads as a current-looking incident
 * regardless of when the operator opens the Event Console. The operator can
 * still edit the JSON before dispatch.
 */
function fordIncidentRaised(now: number): EventSample {
  const windowStart = new Date(now - 25 * 60 * 1000); // ~25 min ago
  const startedAt = new Date(now - 13 * 60 * 1000); // ~13 min ago — alert fire
  const windowEnd = new Date(now + 5 * 60 * 1000); // ~5 min ahead
  const day = startedAt.toISOString().slice(0, 10);
  return {
    id: "ford-incident-raised",
    label: "Ford IncidentRaised (sample)",
    summary:
      "FireHydrant-shaped incident envelope for the Ford Grafana MCP triage path. Sample only.",
    sourceKind: "firehydrant",
    routingKey: "incident:raised",
    messageType: "incident.raised.v1",
    messages: [
      {
        incident_id: `INC-${day}-001`,
        source_event_id: "fh-evt-7f0e5a-checkout-api-5xx",
        source: "firehydrant",
        source_type: "incident_management",
        title: "checkout-api elevated 5xx rate",
        severity: "sev2",
        status: "active",
        affected_service: "checkout-api",
        affected_components: ["checkout-api", "orders-db"],
        environment: "prod",
        started_at: startedAt.toISOString(),
        window: {
          start: windowStart.toISOString(),
          end: windowEnd.toISOString(),
        },
        labels: {
          team: "checkout",
          region: "us-east-1",
          cluster: "prod-east-1",
          runbook_owner: "sre-checkout",
        },
        annotations: {
          summary: "5xx rate on checkout-api crossed alert threshold for >5m",
          description:
            "Elevated 5xx originating from checkout-api -> orders-db. p99 latency rising. No recent deploy.",
          impact: "Checkout success rate dropped from 99.4% to ~96%.",
        },
        links: {
          dashboard: "https://grafana.example.test/d/checkout/checkout-overview",
          runbook: "https://runbooks.example.test/checkout-5xx",
          firehydrant: `https://app.firehydrant.io/incidents/INC-${day}-001`,
        },
        reporter: {
          name: "FireHydrant Alerting",
          kind: "automation",
        },
      },
    ],
    extraMetadata: {
      sample: "ford-incident-raised",
      note: "FireHydrant-shaped sample. Not a real integration.",
    },
    notes:
      "FireHydrant-shaped incident sample (#519/#521). Timestamps are seeded relative to module load — edit incident_id, severity, time range to vary the input. Not a real FireHydrant integration.",
  };
}

export const FORD_INCIDENT_RAISED: EventSample = fordIncidentRaised(Date.now());

export const EVENT_SAMPLES: ReadonlyArray<EventSample> = Object.freeze([
  DISPATCH_ECHO_SMOKE,
  FORD_INCIDENT_RAISED,
]);

export function findSampleById(id: string): EventSample | null {
  return EVENT_SAMPLES.find((s) => s.id === id) ?? null;
}

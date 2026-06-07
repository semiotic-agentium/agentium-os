/// <reference path="./baml-runtime.d.ts" />
import type {
  DispatchRunContext,
  HostDispatchAck,
  RunContext,
  SessionResult,
} from "./baml-runtime";

// Contract stamped by the raw datasource producer (RawDatasourceProducer):
// routing_key = source_kind, message_type = schema_version. Must match the
// datasource manifest at examples/external-tools/deploy-health-datasource.
const DEPLOY_HEALTH_SCHEMA = "deploy-health.v1";
const DEPLOY_HEALTH_SOURCE = "deploy-health";

type DeployHealthEvent = {
  service?: unknown;
  status?: unknown;
  environment?: unknown;
  deploy_id?: unknown;
  observed_at?: unknown;
};

function s(value: unknown, fallback = ""): string {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : fallback;
}

function statusEmoji(status: string): string {
  const v = status.toLowerCase();
  if (v === "healthy" || v === "ok" || v === "resolved") return "✅";
  if (v === "degraded" || v === "warning") return "⚠️";
  if (v === "down" || v === "failed" || v === "critical") return "🚨";
  return "ℹ️";
}

// Deterministic in-process summary — no LLM and no inter-agent calls, so the
// demo runs without API keys or other deployed agents. (A realistic agent would
// open a triage task or call a tool here.)
function summarize(event: DeployHealthEvent, contextId: string): string {
  const service = s(event.service, "unknown-service");
  const status = s(event.status, "unknown");
  const env = s(event.environment, "unspecified");
  const parts = [`${statusEmoji(status)} deploy-health: ${service} is ${status} in ${env}`];
  const deployId = s(event.deploy_id);
  if (deployId) parts.push(`deploy_id=${deployId}`);
  const observedAt = s(event.observed_at);
  if (observedAt) parts.push(`observed_at=${observedAt}`);
  parts.push(`context_id=${contextId}`);
  return parts.join(" | ");
}

async function onDeployHealthDispatch(ctx: DispatchRunContext): Promise<HostDispatchAck> {
  const request = ctx.request;
  if (request.message_type !== DEPLOY_HEALTH_SCHEMA) {
    return { accepted: false, detail: `unsupported message_type=${request.message_type}` };
  }
  if (request.routing_key !== DEPLOY_HEALTH_SOURCE) {
    return { accepted: false, detail: `unsupported routing_key=${request.routing_key}` };
  }
  // Raw mode delivers exactly one JSON object (messages[0] = the webhook body).
  const message = request.messages[0];
  if (!message || typeof message !== "object" || Array.isArray(message)) {
    return { accepted: false, detail: "expected exactly one JSON object in messages[0]" };
  }

  const contextId = s(ctx.contextId, s(request.context_id, "unknown-context"));
  const summary = summarize(message as DeployHealthEvent, contextId);
  console.info(`deploy-health-consumer handled event: ${summary}`);
  return { accepted: true, detail: summary };
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const userText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    const message = await RespondDeployHealthConsumer({ user_message: userText });
    return { message };
  },
  onDispatch: onDeployHealthDispatch,
});

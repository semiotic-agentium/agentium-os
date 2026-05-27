/// <reference path="./baml-runtime.d.ts" />
import type {
  ClickUpIntent,
  DispatchRunContext,
  HostDispatchAck,
  JsonObject,
  ReplyPart,
  SessionResult,
  ClickUpPlanStep,
  ClickUpStructuredPlan,
  StructuredReply,
} from "./baml-runtime";
import {
  MAX_REACT_STEPS,
  PKG_CLICKUP_EXECUTE,
  PKG_CLICKUP_FORMAT,
  runClickUpStructuredPlan,
} from "./clickupWorkflow";

export {
  MAX_REACT_STEPS,
  PKG_CLICKUP_EXECUTE,
  PKG_CLICKUP_FORMAT,
  runClickUpStructuredPlan,
};

export function isJsonObject(v: unknown): v is JsonObject {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

export function isNeedClarification(v: unknown): v is { question: string } {
  return (
    isJsonObject(v) &&
    typeof v.question === "string" &&
    v.question.trim().length > 0 &&
    !("message" in v) &&
    !("intent" in v) &&
    !("reason" in v) &&
    !("steps" in v)
  );
}

export function isNotRelevant(v: unknown): v is { reason: string } {
  return isJsonObject(v) && typeof v.reason === "string" && !("question" in v) && !("intent" in v);
}

export function isClickUpIntent(v: unknown): v is ClickUpIntent {
  return (
    isJsonObject(v) &&
    typeof v.intent === "string" &&
    v.intent.trim().length > 0 &&
    typeof v.operation_kind === "string" &&
    !("question" in v) &&
    !("reason" in v)
  );
}

export function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

/** Flatten SessionResult into a short dispatch ack detail string. */
export function sessionResultDetail(result: SessionResult, maxLen = 800): string {
  if ("error" in result) {
    return result.error.length > maxLen ? `${result.error.slice(0, maxLen)}…` : result.error;
  }
  const msg = result.message;
  if (typeof msg === "string") {
    return msg.length > maxLen ? `${msg.slice(0, maxLen)}…` : msg;
  }
  const textPart = msg.parts?.find((p) => p.type === "text" && typeof p.text === "string");
  const text = textPart && "text" in textPart ? String(textPart.text) : "ClickUp lifecycle ingress completed.";
  return text.length > maxLen ? `${text.slice(0, maxLen)}…` : text;
}

/** Coordination-only: verify the runtime value matches ClickUpStructuredPlan after PlanClickUpWork. */
export function parseClickUpStructuredPlanFromPlanning(v: unknown): ClickUpStructuredPlan | null {
  if (!isJsonObject(v)) return null;
  if (typeof v.intent_description !== "string" || typeof v.objective !== "string") return null;
  if (!Array.isArray(v.plan_steps)) return null;
  const c = v.citations;
  if (c != null && !Array.isArray(c)) return null;
  return v as unknown as ClickUpStructuredPlan;
}

/** @deprecated Use parseClickUpStructuredPlanFromPlanning */
export const parseStandardStructuredPlanFromPlanning = parseClickUpStructuredPlanFromPlanning;

export function validateClickUpPlanForExecution(
  plan: ClickUpStructuredPlan,
): ClickUpPlanStep[] | string {
  const raw = plan.plan_steps;
  if (raw.length === 0) return "plan_steps is empty";

  const steps: ClickUpPlanStep[] = [];
  for (let i = 0; i < raw.length; i++) {
    const s = raw[i];
    if (!isJsonObject(s)) return `plan_steps[${i}] is not an object`;
    if (typeof s.sub_message !== "string" || !s.sub_message.trim()) {
      return `plan_steps[${i}].sub_message must be a non-empty string`;
    }
    const pkgRaw = s.agent_package;
    const instRaw = s.agent_instance_id;
    if (typeof pkgRaw !== "string" || typeof instRaw !== "string") {
      return `plan_steps[${i}] missing agent_package or agent_instance_id`;
    }
    const pkg = pkgRaw.trim().toLowerCase();
    if (pkg !== PKG_CLICKUP_EXECUTE && pkg !== PKG_CLICKUP_FORMAT) {
      return `plan_steps[${i}] has invalid agent_package "${pkgRaw}" (expected clickup-execute or clickup-format)`;
    }
    steps.push({
      agent_package: pkgRaw.trim(),
      agent_instance_id: instRaw.trim() || "default",
      sub_message: s.sub_message,
    });
  }

  let executeCount = 0;
  let formatCount = 0;
  for (const st of steps) {
    const p = st.agent_package.trim().toLowerCase();
    if (p === PKG_CLICKUP_EXECUTE) executeCount++;
    if (p === PKG_CLICKUP_FORMAT) formatCount++;
  }
  if (executeCount < 1) return "plan must include at least one clickup-execute step";
  if (formatCount !== 1) return "plan must include exactly one clickup-format step";
  const lastPkg = steps[steps.length - 1]!.agent_package.trim().toLowerCase();
  if (lastPkg !== PKG_CLICKUP_FORMAT) return "last plan step must be clickup-format";

  return steps;
}

// ── Host source-records ingress (host.source-records.v1 / clickup) ───────────

const INGRESS_AGENT_NAME = "clickup-agent";
const RAW_SOURCE_SCHEMA_VERSION = "host.source-records.v1";
const RAW_SOURCE_ROUTING_KEY = "event:intake";

export const CLICKUP_LIFECYCLE_EVENT_KIND = "clickup.lifecycle_event";

export type ClickupLifecycleEventRecord = {
  record_kind: string;
  key: string;
  event: string;
  task_id: string;
  list_id: string;
  revision: number;
  snapshot: JsonObject;
  previous_snapshot?: JsonObject;
};

export type ClickupSourceRecordsBatch = {
  schema_version: string;
  emitted_at_unix: number;
  source: {
    source_kind: string;
    source_key: string;
    source_label: string;
  };
  project?: {
    project_key: string;
    repo_available?: boolean;
    repo_path?: string | null;
  };
  records: ClickupLifecycleEventRecord[];
};

function normalizeOptionalString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

export function parseClickupSourceRecordsBatch(value: unknown): ClickupSourceRecordsBatch | null {
  if (!isJsonObject(value)) return null;
  const schemaVersion = normalizeOptionalString(value.schema_version);
  if (!schemaVersion) return null;
  const emitted = value.emitted_at_unix;
  if (typeof emitted !== "number" || !Number.isFinite(emitted)) return null;
  const source = value.source;
  if (!isJsonObject(source)) return null;
  const sourceKind = normalizeOptionalString(source.source_kind);
  const sourceKey = normalizeOptionalString(source.source_key);
  const sourceLabel = normalizeOptionalString(source.source_label);
  if (!sourceKind || !sourceKey || !sourceLabel) return null;
  if (!Array.isArray(value.records)) return null;

  const records: ClickupLifecycleEventRecord[] = [];
  for (const row of value.records) {
    if (!isJsonObject(row)) return null;
    const recordKind = normalizeOptionalString(row.record_kind);
    const key = normalizeOptionalString(row.key);
    const event = normalizeOptionalString(row.event);
    const taskId = normalizeOptionalString(row.task_id);
    const listId = normalizeOptionalString(row.list_id);
    const revision = row.revision;
    const snapshot = row.snapshot;
    if (
      recordKind !== CLICKUP_LIFECYCLE_EVENT_KIND ||
      !key ||
      !event ||
      !taskId ||
      !listId ||
      typeof revision !== "number" ||
      !Number.isFinite(revision) ||
      !isJsonObject(snapshot)
    ) {
      return null;
    }
    let previous_snapshot: JsonObject | undefined;
    if (row.previous_snapshot !== undefined) {
      if (!isJsonObject(row.previous_snapshot)) return null;
      previous_snapshot = row.previous_snapshot;
    }
    records.push({
      record_kind: recordKind,
      key,
      event,
      task_id: taskId,
      list_id: listId,
      revision,
      snapshot,
      previous_snapshot,
    });
  }

  let project: ClickupSourceRecordsBatch["project"];
  if (isJsonObject(value.project)) {
    const projectKey = normalizeOptionalString(value.project.project_key);
    if (projectKey) {
      project = {
        project_key: projectKey,
        repo_available: value.project.repo_available === true,
        repo_path:
          typeof value.project.repo_path === "string" ? value.project.repo_path : undefined,
      };
    }
  }

  return {
    schema_version: schemaVersion,
    emitted_at_unix: emitted,
    source: {
      source_kind: sourceKind,
      source_key: sourceKey,
      source_label: sourceLabel,
    },
    project,
    records,
  };
}

type ClickupLifecycleUnit = {
  unitKey: string;
  records: ClickupLifecycleEventRecord[];
};

function groupClickupLifecycleUnits(
  records: ClickupLifecycleEventRecord[],
): ClickupLifecycleUnit[] {
  const groups = new Map<string, ClickupLifecycleEventRecord[]>();
  for (const record of records) {
    const key = normalizeOptionalString(record.key);
    if (!key) continue;
    const existing = groups.get(key);
    if (existing) {
      existing.push(record);
    } else {
      groups.set(key, [record]);
    }
  }
  return Array.from(groups.entries()).map(([unitKey, unitRecords]) => ({
    unitKey,
    records: unitRecords,
  }));
}

async function processClickupLifecycleUnit(
  unitKey: string,
  records: ClickupLifecycleEventRecord[],
): Promise<{ ok: true; detail: string } | { ok: false; detail: string }> {
  if (records.length === 0) {
    return { ok: true, detail: "skipped:empty_unit" };
  }

  const intentResult = await InferClickUpIntent({});

  if (isNotRelevant(intentResult)) {
    return { ok: true, detail: "skipped:not_relevant" };
  }
  if (isNeedClarification(intentResult)) {
    return {
      ok: false,
      detail:
        `${INGRESS_AGENT_NAME} cannot clarify during dispatch: ${intentResult.question}`,
    };
  }
  if (!isClickUpIntent(intentResult)) {
    return {
      ok: false,
      detail:
        `${INGRESS_AGENT_NAME} InferClickUpIntent returned an unexpected shape inside withTask.`,
    };
  }

  const planResult = await PlanClickUpWork({
    intent: intentResult.intent,
    operation_kind: intentResult.operation_kind,
  });
  const structured = parseClickUpStructuredPlanFromPlanning(planResult);
  if (!structured) {
    return {
      ok: false,
      detail: `${INGRESS_AGENT_NAME} planning failed: PlanClickUpWork did not return ClickUpStructuredPlan.`,
    };
  }
  const stepsOrErr = validateClickUpPlanForExecution(structured);
  if (typeof stepsOrErr === "string") {
    return {
      ok: false,
      detail: `${INGRESS_AGENT_NAME} invalid plan for ingress: ${stepsOrErr}`,
    };
  }

  const result = await runClickUpStructuredPlan(
    `clickup-ingress-${unitKey}`,
    structured,
    intentResult.intent,
    intentResult.operation_kind,
    stepsOrErr,
  );
  if ("error" in result) {
    return { ok: false, detail: result.error };
  }
  return { ok: true, detail: sessionResultDetail(result) };
}

async function handleClickupLifecycleBatch(ctx: DispatchRunContext): Promise<HostDispatchAck> {
  const batch = ctx.batch as ClickupSourceRecordsBatch;
  const units = groupClickupLifecycleUnits(batch.records);
  if (units.length === 0) {
    return { accepted: true, detail: "No lifecycle units in batch." };
  }

  let unitsProcessed = 0;
  const unitSummaries: string[] = [];

  for (const unit of units) {
    let unitOutcome: { ok: true; detail: string } | { ok: false; detail: string };
    try {
      unitOutcome = await ctx.withTask(
        {
          unitKey: unit.unitKey,
          records: unit.records as unknown as JsonObject[],
        },
        async () => processClickupLifecycleUnit(unit.unitKey, unit.records),
      );
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      return {
        accepted: false,
        detail: `${INGRESS_AGENT_NAME} failed on unit ${unit.unitKey}: ${reason}`,
      };
    }

    if (!unitOutcome.ok) {
      return { accepted: false, detail: unitOutcome.detail };
    }
    if (unitOutcome.detail !== "skipped:not_relevant") {
      unitsProcessed += 1;
      unitSummaries.push(`${unit.unitKey}=${unitOutcome.detail}`);
    }
  }

  return {
    accepted: true,
    detail:
      `Processed ClickUp lifecycle ingress: ${unitsProcessed}/${units.length} unit(s) ` +
      `from ${batch.records.length} record(s)` +
      (unitSummaries.length > 0 ? ` (${unitSummaries.join("; ")})` : ""),
  };
}

export async function onClickupSourceDispatch(ctx: DispatchRunContext): Promise<HostDispatchAck> {
  const request = ctx.request;
  const messageType = normalizeOptionalString(request.message_type);
  if (messageType !== RAW_SOURCE_SCHEMA_VERSION) {
    return {
      accepted: false,
      detail:
        `${INGRESS_AGENT_NAME} expected message_type ${RAW_SOURCE_SCHEMA_VERSION}, ` +
        `got ${messageType ?? "missing"}.`,
    };
  }

  const routingKey = normalizeOptionalString(request.routing_key);
  if (routingKey !== RAW_SOURCE_ROUTING_KEY) {
    return {
      accepted: false,
      detail:
        `${INGRESS_AGENT_NAME} expected routing_key ${RAW_SOURCE_ROUTING_KEY}, ` +
        `got ${routingKey ?? "missing"}.`,
    };
  }

  if (request.messages.length !== 1) {
    return {
      accepted: false,
      detail:
        `${INGRESS_AGENT_NAME} expected exactly one dispatch message, ` +
        `got ${request.messages.length}.`,
    };
  }

  const batch = ctx.batch as ClickupSourceRecordsBatch | null;
  if (!batch || batch.schema_version !== RAW_SOURCE_SCHEMA_VERSION) {
    return {
      accepted: false,
      detail:
        `${INGRESS_AGENT_NAME} expected ${RAW_SOURCE_SCHEMA_VERSION} ClickupSourceRecordsBatch ` +
        `in dispatch.messages[0].`,
    };
  }

  if (batch.records.length === 0) {
    return { accepted: true, detail: "No lifecycle records in batch." };
  }

  try {
    return await handleClickupLifecycleBatch(ctx);
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return {
      accepted: false,
      detail: `${INGRESS_AGENT_NAME} failed: ${reason}`,
    };
  }
}

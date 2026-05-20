/// <reference path="./baml-runtime.d.ts" />
import type { DispatchRunContext, HostDispatchAck, JsonObject, SessionResult } from "./baml-runtime";

const AGENT_NAME = "dispatch-ingress-demo";
const RAW_SOURCE_SCHEMA_VERSION = "host.source-records.v1";
const RAW_SOURCE_ROUTING_KEY = "event:intake";

type IngressUnit = {
  unitKey: string;
  records: JsonObject[];
};

function normalizeOptionalString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function isJsonObject(value: unknown): value is JsonObject {
  return value != null && typeof value === "object" && !Array.isArray(value);
}

function lifecycleUnitKey(row: JsonObject): string | null {
  return normalizeOptionalString(row.key);
}

function groupLifecycleUnits(records: JsonObject[]): IngressUnit[] {
  const groups = new Map<string, JsonObject[]>();
  for (const row of records) {
    if (!isJsonObject(row)) continue;
    const key = lifecycleUnitKey(row);
    if (!key) continue;
    const existing = groups.get(key);
    if (existing) {
      existing.push(row);
    } else {
      groups.set(key, [row]);
    }
  }
  return Array.from(groups.entries()).map(([unitKey, unitRecords]) => ({
    unitKey,
    records: unitRecords,
  }));
}

function isIngressNotRelevant(value: unknown): value is { reason: string } {
  return (
    isJsonObject(value) &&
    typeof value.reason === "string" &&
    !("unit_label" in value)
  );
}

function isIngressUnitSeen(value: unknown): value is { unit_label: string } {
  return isJsonObject(value) && typeof value.unit_label === "string";
}

async function onIngressDispatch(ctx: DispatchRunContext): Promise<HostDispatchAck> {
  const request = ctx.request;
  if (normalizeOptionalString(request.message_type) !== RAW_SOURCE_SCHEMA_VERSION) {
    return {
      accepted: false,
      detail: `${AGENT_NAME} expected message_type ${RAW_SOURCE_SCHEMA_VERSION}`,
    };
  }
  if (normalizeOptionalString(request.routing_key) !== RAW_SOURCE_ROUTING_KEY) {
    return {
      accepted: false,
      detail: `${AGENT_NAME} expected routing_key ${RAW_SOURCE_ROUTING_KEY}`,
    };
  }
  if (request.messages.length !== 1) {
    return {
      accepted: false,
      detail: `${AGENT_NAME} expected exactly one dispatch message`,
    };
  }

  const batch = ctx.batch;
  const records = Array.isArray(batch?.records)
    ? batch.records.filter((row): row is JsonObject => isJsonObject(row))
    : [];
  const units = groupLifecycleUnits(records);
  if (units.length === 0) {
    return { accepted: true, detail: "withTask_units=" };
  }

  const seenKeys: string[] = [];
  try {
    for (const unit of units) {
      await ctx.withTask({ unitKey: unit.unitKey, records: unit.records }, async () => {
        seenKeys.push(unit.unitKey);
        const classified = await ClassifyIngressUnit({});
        if (isIngressNotRelevant(classified)) {
          return;
        }
        if (!isIngressUnitSeen(classified)) {
          throw new Error(`${AGENT_NAME} ClassifyIngressUnit returned unexpected shape`);
        }
      });
    }
    seenKeys.sort();
    return {
      accepted: true,
      detail: `withTask_units=${seenKeys.join(",")}`,
    };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return { accepted: false, detail: `${AGENT_NAME} failed: ${reason}` };
  }
}

__chat_register({
  run: async (): Promise<SessionResult> => ({
    message: `${AGENT_NAME} handles host.source-records.v1 via onDispatch only`,
  }),
  onDispatch: onIngressDispatch,
});

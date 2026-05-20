/// <reference path="./baml-runtime.d.ts" />
import type { DispatchRunContext, HostDispatchAck, JsonObject } from "./baml-runtime";

const AGENT_NAME = "slack-agent";
const RAW_SOURCE_SCHEMA_VERSION = "host.source-records.v1";
const RAW_SOURCE_ROUTING_KEY = "event:intake";

type SlackRawSourceRecord = {
  channel_id?: string;
  ts?: string;
  thread_ts?: string;
  user_id?: string;
  user?: string;
  user_name?: string;
  text?: string;
  subtype?: string;
};

type SlackConversationGroup = {
  conversationKey: string;
  records: SlackRawSourceRecord[];
};

function normalizeOptionalString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function isNotRelevant(value: unknown): value is { reason: string } {
  return (
    value != null &&
    typeof value === "object" &&
    "reason" in value &&
    typeof (value as { reason: unknown }).reason === "string"
  );
}

function isNeedClarification(value: unknown): value is { question: string } {
  return (
    value != null &&
    typeof value === "object" &&
    "question" in value &&
    typeof (value as { question: unknown }).question === "string"
  );
}

function parseSlackRawSourceRecord(value: unknown): SlackRawSourceRecord | null {
  if (value == null || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  return {
    channel_id: normalizeOptionalString(row.channel_id) ?? undefined,
    ts: normalizeOptionalString(row.ts) ?? undefined,
    thread_ts: normalizeOptionalString(row.thread_ts) ?? undefined,
    user_id: normalizeOptionalString(row.user_id) ?? undefined,
    user: normalizeOptionalString(row.user) ?? undefined,
    user_name: normalizeOptionalString(row.user_name) ?? undefined,
    text: normalizeOptionalString(row.text) ?? undefined,
    subtype: normalizeOptionalString(row.subtype) ?? undefined,
  };
}

function isSlackSystemSubtype(subtype: string | undefined): boolean {
  return (
    subtype === "channel_join" ||
    subtype === "channel_leave" ||
    subtype === "channel_topic" ||
    subtype === "channel_purpose" ||
    subtype === "channel_name" ||
    subtype === "channel_archive" ||
    subtype === "channel_unarchive"
  );
}

function isSlackConversationRecord(record: SlackRawSourceRecord): boolean {
  const text = normalizeOptionalString(record.text);
  if (!text) return false;
  return !isSlackSystemSubtype(record.subtype);
}

function slackConversationKey(record: SlackRawSourceRecord): string | null {
  return normalizeOptionalString(record.thread_ts) ?? normalizeOptionalString(record.ts);
}

function groupSlackConversationRecords(records: SlackRawSourceRecord[]): SlackConversationGroup[] {
  const groups = new Map<string, SlackRawSourceRecord[]>();
  for (const record of records) {
    const key = slackConversationKey(record);
    if (!key) continue;
    const existing = groups.get(key);
    if (existing) {
      existing.push(record);
      continue;
    }
    groups.set(key, [record]);
  }
  return Array.from(groups.entries()).map(([conversationKey, groupedRecords]) => ({
    conversationKey,
    records: groupedRecords,
  }));
}

function slackRecordsFromBatch(batch: DispatchRunContext["batch"]): SlackRawSourceRecord[] {
  if (!batch?.records?.length) {
    return [];
  }
  return batch.records
    .map((row) => parseSlackRawSourceRecord(row))
    .filter((record): record is SlackRawSourceRecord => record != null)
    .filter((record) => isSlackConversationRecord(record));
}

export async function onSlackSourceDispatch(ctx: DispatchRunContext): Promise<HostDispatchAck> {
  const request = ctx.request;
  const messageType = normalizeOptionalString(request.message_type);
  if (messageType !== RAW_SOURCE_SCHEMA_VERSION) {
    return {
      accepted: false,
      detail:
        `${AGENT_NAME} expected message_type ${RAW_SOURCE_SCHEMA_VERSION}, ` +
        `got ${messageType ?? "missing"}.`,
    };
  }

  const routingKey = normalizeOptionalString(request.routing_key);
  if (routingKey !== RAW_SOURCE_ROUTING_KEY) {
    return {
      accepted: false,
      detail:
        `${AGENT_NAME} expected routing_key ${RAW_SOURCE_ROUTING_KEY}, ` +
        `got ${routingKey ?? "missing"}.`,
    };
  }

  if (request.messages.length !== 1) {
    return {
      accepted: false,
      detail:
        `${AGENT_NAME} expected exactly one dispatch message, ` +
        `got ${request.messages.length}.`,
    };
  }

  const records = slackRecordsFromBatch(ctx.batch);
  const groups = groupSlackConversationRecords(records);
  if (groups.length === 0) {
    return { accepted: true, detail: "No readable Slack conversation units in batch." };
  }

  let unitsStarted = 0;
  try {
    for (const group of groups) {
      await ctx.withTask(
        {
          unitKey: group.conversationKey,
          records: group.records as unknown as JsonObject[],
        },
        async () => {
          unitsStarted += 1;
          const intentResult = await InferSlackIntent({});
          if (isNotRelevant(intentResult)) {
            return;
          }
          if (isNeedClarification(intentResult)) {
            throw new Error(
              `${AGENT_NAME} cannot clarify during dispatch: ${intentResult.question}`,
            );
          }
        },
      );
    }
    return {
      accepted: true,
      detail: `${AGENT_NAME} processed ${unitsStarted} Slack conversation unit(s) from ${records.length} record(s).`,
    };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return {
      accepted: false,
      detail: `${AGENT_NAME} failed: ${reason}`,
    };
  }
}

import { describe, expect, it } from "vitest";
import {
  buildDispatchRequest,
  buildDispatchRequestPreview,
  EVENT_CONSOLE_ORIGIN,
  mintContextId,
  PREVIEW_MINTED_AT_DISPATCH,
} from "./dispatchRequest";
import { DISPATCH_ECHO_SMOKE, FORD_INCIDENT_RAISED } from "./sampleCatalog";

describe("Event Console dispatch request builder", () => {
  it("stamps origin=operator-eval-console and sample id on every dispatch", () => {
    const body = buildDispatchRequest(
      {
        sample: DISPATCH_ECHO_SMOKE,
        messages: DISPATCH_ECHO_SMOKE.messages,
        scope: { kind: "new_context" },
      },
      1_700_000_000_000,
    );
    expect(body.metadata).toBeDefined();
    expect(body.metadata!.origin).toBe(EVENT_CONSOLE_ORIGIN);
    expect(body.metadata!.sample_id).toBe(DISPATCH_ECHO_SMOKE.id);
    expect(body.metadata!.source_kind).toBe(DISPATCH_ECHO_SMOKE.sourceKind);
  });

  it("mints temporal context_id (ctx-<ms>-<n>) and a message_id under new_context scope", () => {
    const body = buildDispatchRequest(
      {
        sample: DISPATCH_ECHO_SMOKE,
        messages: [],
        scope: { kind: "new_context" },
      },
      1_700_000_000_000,
    );
    expect(body.context_id).toMatch(/^ctx-\d+-\d+$/);
    expect(body.message_id).toMatch(/^evt-\d+-\d+$/);
  });

  it("uses the operator-supplied contextId under existing_context scope", () => {
    const body = buildDispatchRequest({
      sample: DISPATCH_ECHO_SMOKE,
      messages: [],
      scope: { kind: "existing_context", contextId: "ctx-123-1" },
    });
    expect(body.context_id).toBe("ctx-123-1");
    expect(body.message_id).toMatch(/^evt-\d+-\d+$/);
  });

  it("carries routing_key + message_type from the sample, not from the editor body", () => {
    const body = buildDispatchRequest({
      sample: FORD_INCIDENT_RAISED,
      messages: [{ incident_id: "edited" }],
      scope: { kind: "new_context" },
    });
    expect(body.routing_key).toBe(FORD_INCIDENT_RAISED.routingKey);
    expect(body.message_type).toBe(FORD_INCIDENT_RAISED.messageType);
    expect(body.messages).toEqual([{ incident_id: "edited" }]);
  });

  it("merges sample.extraMetadata into the dispatch metadata", () => {
    const body = buildDispatchRequest({
      sample: FORD_INCIDENT_RAISED,
      messages: [],
      scope: { kind: "new_context" },
    });
    expect(body.metadata!.sample).toBe("ford-incident-raised");
    expect(body.metadata!.origin).toBe(EVENT_CONSOLE_ORIGIN);
  });

  it("preserves the operator note when supplied", () => {
    const body = buildDispatchRequest({
      sample: DISPATCH_ECHO_SMOKE,
      messages: [],
      scope: { kind: "new_context" },
      note: "smoke check for staging",
    });
    expect(body.metadata!.operator_note).toBe("smoke check for staging");
  });

  it("omits operator_note when the note is whitespace-only", () => {
    const body = buildDispatchRequest({
      sample: DISPATCH_ECHO_SMOKE,
      messages: [],
      scope: { kind: "new_context" },
      note: "   ",
    });
    expect(body.metadata!.operator_note).toBeUndefined();
  });
});

describe("buildDispatchRequestPreview", () => {
  it("substitutes the sentinel for host-minted fields so the preview is stable across keystrokes", () => {
    const first = buildDispatchRequestPreview({
      sample: DISPATCH_ECHO_SMOKE,
      messages: DISPATCH_ECHO_SMOKE.messages,
      scope: { kind: "new_context" },
    });
    const second = buildDispatchRequestPreview({
      sample: DISPATCH_ECHO_SMOKE,
      messages: DISPATCH_ECHO_SMOKE.messages,
      scope: { kind: "new_context" },
    });
    expect(first.context_id).toBe(PREVIEW_MINTED_AT_DISPATCH);
    expect(first.message_id).toBe(PREVIEW_MINTED_AT_DISPATCH);
    expect(first.metadata!.dispatched_at).toBe(PREVIEW_MINTED_AT_DISPATCH);
    expect(first).toEqual(second);
  });

  it("shows the operator-supplied context id under existing_context scope (no sentinel)", () => {
    const body = buildDispatchRequestPreview({
      sample: DISPATCH_ECHO_SMOKE,
      messages: [],
      scope: { kind: "existing_context", contextId: "ctx-999-1" },
    });
    expect(body.context_id).toBe("ctx-999-1");
    expect(body.message_id).toBe(PREVIEW_MINTED_AT_DISPATCH);
  });
});

describe("mintContextId", () => {
  it("emits parseable temporal context ids (`ctx-<ms>-<counter>`)", () => {
    const id = mintContextId(1_700_000_000_000);
    expect(id).toMatch(/^ctx-1700000000000-\d+$/);
  });
});

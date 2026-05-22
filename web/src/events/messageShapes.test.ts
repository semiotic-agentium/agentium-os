import { describe, expect, it } from "vitest";
import type { AgentDeliverableMessageShape } from "../types/events";
import {
  autofillPayload,
  deriveDispatchEnvelope,
  messageShapesForAgent,
  messageShapesForSubscription,
  subscriptionMatchesShape,
} from "./messageShapes";

const slackSourceShape: AgentDeliverableMessageShape = {
  message_shape_id: "slack-source-records",
  display_name: "Slack raw source records",
  description: "desc",
  origin: "support/slack",
  payload_name: "Source records",
  wire_schema_version: "host.source-records.v1",
  source_kind: "slack",
  payload_schema: {},
  samples: [
    {
      sample_id: "slack-thread-semantic",
      label: "Slack thread",
      source_key: "slack:C012TEST001",
      payload: {
        schema_version: "host.source-records.v1",
        source: {
          source_kind: "slack",
          source_key: "slack:C012TEST001",
          source_label: "#agentium-eng",
        },
        records: [],
      },
    },
  ],
  delivery_defaults: { routing_key: "event:intake" },
};

const clickupSourceShape: AgentDeliverableMessageShape = {
  ...slackSourceShape,
  message_shape_id: "clickup-source-records",
  display_name: "ClickUp source records",
  source_kind: "clickup",
  samples: [
    {
      sample_id: "clickup-window",
      label: "ClickUp poll window",
      source_key: "clickup:list-1",
      payload: {
        schema_version: "host.source-records.v1",
        emitted_at_unix: 1_735_720_000,
        source: {
          source_kind: "clickup",
          source_key: "clickup:list:901325431486",
          source_label: "ClickUp list",
        },
        records: [
          {
            record_kind: "clickup.lifecycle_event",
            key: "clickup-created:task-sample-1:1",
            event: "created",
            task_id: "task-sample-1",
            list_id: "901325431486",
            revision: 1,
            snapshot: {
              id: "task-sample-1",
              list_id: "901325431486",
              name: "Sample task from Event Console",
              status: "in progress",
            },
          },
        ],
      },
    },
  ],
};

describe("messageShapes", () => {
  it("filters shapes by one subscription row", () => {
    const shapes = messageShapesForSubscription(
      [slackSourceShape, clickupSourceShape],
      [
        { schema_versions: ["host.source-records.v1"], source_kinds: ["slack"] },
        { schema_versions: ["host.source-records.v1"], source_kinds: ["clickup"] },
      ],
      1,
    );
    expect(shapes.map((s) => s.message_shape_id)).toEqual(["clickup-source-records"]);
  });

  it("filters shapes by agent subscription", () => {
    const shapes = messageShapesForAgent([slackSourceShape, clickupSourceShape], [
      { schema_versions: ["host.source-records.v1"], source_kinds: ["clickup"] },
    ]);
    expect(shapes.map((s) => s.message_shape_id)).toEqual(["clickup-source-records"]);
  });

  it("derives dispatch envelope from shape and sample", () => {
    const envelope = deriveDispatchEnvelope(
      slackSourceShape,
      slackSourceShape.samples[0],
    );
    expect(envelope).toEqual({
      routingKey: "event:intake",
      messageType: "host.source-records.v1",
      sourceKind: "slack",
      sourceKey: "slack:C012TEST001",
    });
  });

  it("autofills schema_version and source fields", () => {
    const envelope = deriveDispatchEnvelope(
      slackSourceShape,
      slackSourceShape.samples[0],
    );
    const shapeWithConst = {
      ...slackSourceShape,
      payload_schema: {
        type: "object",
        properties: {
          schema_version: { type: "string", const: "host.source-records.v1" },
          source: {
            type: "object",
            properties: {
              source_kind: { type: "string" },
              source_key: { type: "string" },
            },
          },
        },
      },
    };
    const filled = autofillPayload(shapeWithConst, envelope, {
      schema_version: "",
      source: { source_kind: "", source_key: "" },
    });
    expect(filled.schema_version).toBe("host.source-records.v1");
    expect(filled.source).toEqual({
      source_kind: "slack",
      source_key: "slack:C012TEST001",
    });
  });

  it("matches subscription on schema version and source kind", () => {
    expect(
      subscriptionMatchesShape(
        { schema_versions: ["host.source-records.v1"], source_kinds: ["slack"] },
        slackSourceShape,
      ),
    ).toBe(true);
    expect(
      subscriptionMatchesShape(
        { schema_versions: ["host.source-records.v1"], source_kinds: ["clickup"] },
        slackSourceShape,
      ),
    ).toBe(false);
  });
});

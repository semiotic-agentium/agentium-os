// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  defaultPayloadFromSchema,
  fieldsFromSchema,
  getAtPath,
  setAtPath,
} from "./schemaForm";

describe("schemaForm", () => {
  const schema = {
    type: "object",
    required: ["event_id"],
    properties: {
      event_id: { type: "string" },
      messages_scanned: { type: "integer" },
      derived_tasks: { type: "array", items: { type: "object" } },
    },
  };

  it("extracts required fields first", () => {
    const fields = fieldsFromSchema(schema);
    expect(fields[0]?.key).toBe("event_id");
    expect(fields[0]?.required).toBe(true);
  });

  it("sets nested paths", () => {
    const obj: Record<string, unknown> = {};
    setAtPath(obj, "source.source_label", "chan");
    expect(getAtPath(obj, "source.source_label")).toBe("chan");
  });

  it("builds default payload", () => {
    const payload = defaultPayloadFromSchema(schema);
    expect(payload.event_id).toBe("");
    expect(Array.isArray(payload.derived_tasks)).toBe(true);
  });

  it("marks const schema fields", () => {
    const fields = fieldsFromSchema({
      type: "object",
      properties: {
        schema_version: { type: "string", const: "host.source-records.v1" },
        source: {
          type: "object",
          properties: { source_kind: { type: "string" } },
        },
      },
    });
    expect(fields.find((f) => f.key === "schema_version")?.kind).toBe("const");
    expect(fields.find((f) => f.key === "source")?.properties?.length).toBeGreaterThan(0);
  });
});

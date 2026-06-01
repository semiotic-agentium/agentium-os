// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Generic JSON Schema → form field descriptors for the Event Console. */

export type SchemaFieldKind =
  | "string"
  | "number"
  | "integer"
  | "boolean"
  | "enum"
  | "const"
  | "object"
  | "array"
  | "unknown";

export interface SchemaFormField {
  key: string;
  path: string;
  kind: SchemaFieldKind;
  title: string;
  description?: string;
  required: boolean;
  enumValues?: string[];
  constValue?: unknown;
  itemSchema?: JsonSchemaNode;
  properties?: SchemaFormField[];
  objectSchema?: JsonSchemaNode;
}

export interface JsonSchemaNode {
  type?: string | string[];
  properties?: Record<string, JsonSchemaNode>;
  required?: string[];
  items?: JsonSchemaNode;
  enum?: unknown[];
  const?: unknown;
  description?: string;
  format?: string;
}

function primaryType(node: JsonSchemaNode): string {
  const t = node.type;
  if (Array.isArray(t)) {
    return t.find((x) => x !== "null") ?? "unknown";
  }
  return t ?? "unknown";
}

function fieldTitle(key: string, node: JsonSchemaNode, labels?: Record<string, string>): string {
  const path = key;
  if (labels?.[path]) return labels[path]!;
  if (node.description) return node.description;
  return key;
}

export function fieldsFromSchema(
  schema: unknown,
  labels?: Record<string, string>,
  basePath = "",
): SchemaFormField[] {
  if (!schema || typeof schema !== "object") return [];
  const node = schema as JsonSchemaNode;
  if (primaryType(node) !== "object" || !node.properties) return [];

  const required = new Set(node.required ?? []);
  const out: SchemaFormField[] = [];

  for (const [key, child] of Object.entries(node.properties)) {
    const path = basePath ? `${basePath}.${key}` : key;
    const kind = mapKind(child);
    const field: SchemaFormField = {
      key,
      path,
      kind,
      title: fieldTitle(path, child, labels),
      description: child.description,
      required: required.has(key),
      enumValues:
        child.enum?.filter((v): v is string => typeof v === "string") ??
        undefined,
    };
    if (child.const !== undefined) {
      field.kind = "const";
      field.constValue = child.const;
    }
    if (kind === "object" && child.properties) {
      field.properties = fieldsFromSchema(child, labels, path);
      field.objectSchema = child;
    }
    if (kind === "array" && child.items) {
      field.itemSchema = child.items;
    }
    out.push(field);
  }

  return out.sort((a, b) => {
    if (a.required !== b.required) return a.required ? -1 : 1;
    return a.title.localeCompare(b.title);
  });
}

function mapKind(node: JsonSchemaNode): SchemaFieldKind {
  if (node.const !== undefined) return "const";
  const t = primaryType(node);
  if (node.enum?.length) return "enum";
  switch (t) {
    case "string":
      return "string";
    case "number":
      return "number";
    case "integer":
      return "integer";
    case "boolean":
      return "boolean";
    case "object":
      return "object";
    case "array":
      return "array";
    default:
      return "unknown";
  }
}

export function getAtPath(obj: unknown, path: string): unknown {
  if (!path) return obj;
  const parts = path.split(".");
  let cur: unknown = obj;
  for (const part of parts) {
    if (cur == null || typeof cur !== "object") return undefined;
    cur = (cur as Record<string, unknown>)[part];
  }
  return cur;
}

export function setAtPath(obj: Record<string, unknown>, path: string, value: unknown): void {
  const parts = path.split(".");
  let cur: Record<string, unknown> = obj;
  for (let i = 0; i < parts.length - 1; i++) {
    const part = parts[i]!;
    const next = cur[part];
    if (next == null || typeof next !== "object" || Array.isArray(next)) {
      cur[part] = {};
    }
    cur = cur[part] as Record<string, unknown>;
  }
  cur[parts[parts.length - 1]!] = value;
}

export function ensureObjectPayload(base: unknown): Record<string, unknown> {
  if (base && typeof base === "object" && !Array.isArray(base)) {
    return { ...(base as Record<string, unknown>) };
  }
  return {};
}

export function defaultPayloadFromSchema(schema: unknown): Record<string, unknown> {
  const fields = fieldsFromSchema(schema);
  const out: Record<string, unknown> = {};
  for (const f of fields) {
    if (f.kind === "object") {
      out[f.key] = {};
    } else if (f.kind === "array") {
      out[f.key] = [];
    } else if (f.kind === "boolean") {
      out[f.key] = false;
    } else if (f.kind === "number" || f.kind === "integer") {
      out[f.key] = 0;
    } else if (f.kind === "const" && f.constValue !== undefined) {
      out[f.key] = f.constValue;
    } else {
      out[f.key] = "";
    }
  }
  return out;
}

export function parseJsonPointerToPath(pointer?: string): string | null {
  if (!pointer) return null;
  const m = pointer.match(/^\/messages\/(\d+)(\/(.*))?$/);
  if (!m) return null;
  const idx = m[1];
  const rest = m[3];
  if (!rest) return `messages[${idx}]`;
  return rest.replace(/^\//, "").replace(/\//g, ".");
}

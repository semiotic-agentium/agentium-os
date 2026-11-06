<!--
SPDX-FileCopyrightText: 2026 Karakara Labs

SPDX-License-Identifier: Apache-2.0
-->

# Plan: canonical tool schema graph

Status: proposal and prerequisite. This describes work to do. It does not describe current behavior.

This plan must land before [`tool-typescript-codegen.md`](tool-typescript-codegen.md). The TypeScript change must consume the graph defined here and must not revive `ToolTypeSpec.ts_decl` or `extra_ts_decls` as its DTO source.

## Decision

Use each resolved tool's JSON Schemas as the common build-time contract for Open input, Send input, and output. Lower those schemas once into a typed `ToolSchemaGraph`. Render generated BAML tool DTOs from that graph. A later change will render TypeScript DTOs from the same graph.

Static and imported tools differ only in how their schemas enter `ToolFunctionMetadata`:

```text
static Rust tool
  #[derive(BamlType)] or manual JsonSchemaType
             |
             v
ToolFunctionMetadata.{open_input_schema,input_schema,output_schema}
             |
             v
        ToolSchemaGraph
             |
             +--> generated BAML DTOs
             +--> generated TypeScript DTOs, in the dependent plan

external / MCP / sandbox tool
  approved manifest or registry snapshot JSON Schema
             |
             v
ToolFunctionMetadata.{open_input_schema,input_schema,output_schema}
             |
             v
        the same ToolSchemaGraph
```

Backend and origin must not select a different DTO code-generation path. `ToolBackend::Static`, `External`, `Mcp`, `Sandbox`, and `Wasm` all use the same lowering and naming rules once metadata has been resolved.

## Why this is a separate prerequisite

The graph changes how tool DTOs are represented and how generated BAML is produced. It should land and stabilize without simultaneously changing the public `baml-runtime.d.ts` contract.

Keeping this separate gives the work a clear acceptance boundary:

- this plan owns schema normalization and BAML parity;
- the dependent plan owns the TypeScript tool-session API;
- no temporary TypeScript declaration parser is allowed between them.

The graph branch should merge to `master` first. The TypeScript branch should then start from that updated `master`.

## Current state

### Static tools already have schemas

The name `#[derive(BamlType)]` hides the fact that the derive currently generates three representations:

```rust
impl BamlType for T { ... }
impl TsType for T { ... }
impl JsonSchemaType for T { ... }
```

`ToolType` requires both `JsonSchemaType` and `TsType`. `ToolFunctionMetadata::from_types` calls `json_schema_value::<T>()` for Open input, input, and output and stores the resulting JSON Schema documents in metadata.

System One follows the same metadata contract. Its tagged wire types implement `JsonSchemaType` manually because their serde representation is more complex than the current derive supports.

### Imported tools already have schemas

External and MCP metadata also populate the same three schema fields. Their `ToolTypeSpec.ts_decl` values are normally absent. MCP import normalizes unsupported input schemas to the existing `OpaqueJson` marker before the builder sees them.

### BAML generation is currently split

`crates/baml-rt-builder/src/builder/baml_gen/tool_interfaces.rs` chooses between two paths:

1. Static tools with `baml_decl` inject pre-rendered declarations from `BamlType`.
2. Tools without `baml_decl` call `schema_to_baml::generate_baml_types_from_schemas`.

`schema_to_baml.rs` parses JSON Schema while writing BAML strings. Parsing, naming, dependency discovery, and rendering are interleaved. There is no reusable graph for a TypeScript renderer.

### TypeScript generation is also split

Static metadata may contain `ts_decl` and `extra_ts_decls`. Imported tools usually do not. Restoring that path would give static tools precise DTOs while reducing external tools to `Record<string, unknown>`, despite both having schemas.

This plan removes that asymmetry at its source.

## Goals

1. Build one normalized graph for every allowlisted tool, independent of tool origin and backend.
2. Preserve the wire distinctions required by both BAML and TypeScript:
   - omitted versus required fields;
   - nullable values;
   - wire property names;
   - literal values and enum wire values;
   - unions;
   - arrays and maps;
   - named references;
   - opaque JSON.
3. Give every declaration a stable identity, emitted symbol, dependency set, and source location.
4. Detect name and schema conflicts before either renderer emits text.
5. Render current generated BAML DTOs from the graph.
6. Carry the graph through the builder pipeline so later TypeScript generation consumes the exact same object.
7. Preserve runtime validation. The original JSON Schema remains in `ToolFunctionMetadata` and remains authoritative for runtime input validation.
8. Make static and external tools with equivalent schemas generate equivalent graph declarations and target-language types.

## Non-goals

- Changing the tool-session FSM or execution behavior.
- Changing runtime schema validation.
- Generating the imperative TypeScript API. That belongs to `tool-typescript-codegen.md`.
- Supporting arbitrary JSON Schema. Unsupported constructs keep the existing hard-error or explicit `OpaqueJson` policy.
- Converting `coordination_baml` into schema. Coordination fragments contain behavior and authored functions, not DTO definitions.
- Converting tool configuration schemas. This plan covers `open_input_schema`, `input_schema`, and `output_schema`; `ToolConfigMetadata.schema` remains separate.
- Deleting `TsType`, `ToolTypeSpec.ts_decl`, `extra_ts_decls`, or `baml_decl` from serialized metadata in the first change. They may have consumers outside this pipeline. The builder will stop using them for tool DTO generation after migration.

## Required invariants

### One resolved metadata inventory

`WorkspaceReady::materialize` already resolves `Vec<ToolFunctionMetadata>`. The builder must lower the graph from that vector once and carry both forward. Prelude generation and TypeScript generation must not resolve inventory or imported catalogs again.

### One graph per build

The same `Arc<ToolSchemaGraph>` must be used for:

- generated tool DTO BAML;
- tool cards and tool interfaces that reference DTO roots;
- schema symbol ownership during TypeScript generation;
- the later TypeScript DTO renderer.

Re-lowering schemas independently in BAML and TypeScript code would recreate the drift this plan is meant to remove.

### Original schemas remain available

The graph is a code-generation model, not a replacement for the original validation document. Numeric limits, string formats, regex patterns, and other validation-only keywords do not all affect BAML or TypeScript shapes. Keep the original schema on metadata for runtime validation and diagnostics.

### No silent loss of supported structure

If a schema was previously typed by `schema_to_baml`, graph lowering must either represent it or fail with a source-located error. It must not silently replace a supported structure with `JsonValue`.

Opaque fallback is allowed only when metadata already carries the explicit `OpaqueJson` marker or the relevant import policy has recorded an opaque fallback.

### Deterministic output

Graph identity, symbol allocation, declaration ordering, union ordering, and diagnostics must not depend on `HashMap` iteration order. Use `BTreeMap`, sorted inputs, or explicit stable sorting at every externally visible boundary.

## Proposed module layout

Keep the graph in `baml-rt-builder` initially. The builder owns JSON Schema to generated-language lowering, and moving it to `baml-rt-tools` would introduce code-generation concerns into the runtime metadata crate.

```text
crates/baml-rt-builder/src/builder/tool_schema_graph/
  mod.rs          public graph types and build entry point
  lower.rs        JSON Schema traversal and normalization
  names.rs        symbol allocation and collision handling
  baml.rs         graph to BAML declarations
  diagnostics.rs  schema locations and build errors
```

Keep `schema_to_baml.rs` temporarily as a compatibility wrapper around the graph and renderer. Remove or reduce it after all callers migrate.

The dependent TypeScript change adds:

```text
  typescript.rs   graph to TypeScript declarations
```

It must not add a second JSON Schema traversal.

## Core data model

The exact Rust spelling can change during implementation, but the model must preserve these distinctions.

### Build inputs

```rust
#[derive(Debug, Clone)]
pub struct ToolSchemaBuildInput<'a> {
    pub tool_name: &'a ToolName,
    pub class_name: &'a str,
    pub open_input: ToolSchemaRootInput<'a>,
    pub input: ToolSchemaRootInput<'a>,
    pub output: ToolSchemaRootInput<'a>,
}

#[derive(Debug, Clone)]
pub struct ToolSchemaRootInput<'a> {
    pub role: ToolSchemaRole,
    pub requested_name: &'a str,
    pub schema: &'a serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolSchemaRole {
    OpenInput,
    Input,
    Output,
}
```

`ToolSchemaGraph::from_tools(&[ToolFunctionMetadata])` constructs these inputs internally. Callers should not duplicate the extraction logic.

### Graph and per-tool contracts

```rust
#[derive(Debug, Clone)]
pub struct ToolSchemaGraph {
    pub tools: BTreeMap<ToolName, ToolSchemaContract>,
    pub declarations: BTreeMap<SchemaTypeId, SchemaDeclaration>,
    pub symbols: BTreeMap<String, SchemaTypeId>,
}

#[derive(Debug, Clone)]
pub struct ToolSchemaContract {
    pub tool_name: ToolName,
    pub class_name: String,
    pub open_input: OpenInputContract,
    pub input: SchemaTypeRef,
    pub output: SchemaTypeRef,
}

#[derive(Debug, Clone)]
pub enum OpenInputContract {
    Absent,
    Present {
        ty: SchemaTypeRef,
        requirement: OpenInputRequirement,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenInputRequirement {
    Optional,
    Required,
}
```

Open input is `Absent` only for the metadata unit convention, currently `open_input_type.name == "()"`. An empty schema attached to a non-unit input means unconstrained JSON, not absence.

Use the existing `schema_allows_empty_or_optional_open_input` behavior to derive `OpenInputRequirement`. Move or wrap that logic so graph construction and session interface generation cannot disagree.

### Stable identities and source locations

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaTypeId(u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaLocation {
    pub tool_name: ToolName,
    pub role: ToolSchemaRole,
    pub json_pointer: String,
}
```

`SchemaTypeId` is internal graph identity. Renderers refer to IDs, not strings. `SchemaLocation` appears in every unsupported-schema, unresolved-reference, and collision diagnostic.

IDs must be assigned from a sorted traversal or be otherwise hidden from snapshots. Do not expose allocation order as a public contract.

### Declarations

```rust
#[derive(Debug, Clone)]
pub struct SchemaDeclaration {
    pub id: SchemaTypeId,
    pub symbol: String,
    pub preferred_symbol: String,
    pub owner: SchemaOwner,
    pub kind: SchemaDeclarationKind,
    pub dependencies: BTreeSet<SchemaTypeId>,
    pub source: SchemaLocation,
    pub fingerprint: SchemaFingerprint,
}

#[derive(Debug, Clone)]
pub enum SchemaOwner {
    PublicRoot {
        tool_name: ToolName,
        role: ToolSchemaRole,
    },
    Nested {
        tool_name: ToolName,
        path: Vec<String>,
    },
    SharedDefinition {
        original_name: String,
    },
}

#[derive(Debug, Clone)]
pub enum SchemaDeclarationKind {
    Object(SchemaObject),
    Enum(SchemaEnum),
    Alias(SchemaTypeRef),
}
```

Roots are public and keep their metadata names unless those names are invalid or conflict. A conflicting public root is a build error. The generator must not silently rename `SystemOneInput` or an imported tool's advertised root type.

### Type expressions

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaTypeRef {
    String,
    Integer,
    Number,
    Boolean,
    Null,
    JsonValue,
    Never,
    Literal(SchemaLiteral),
    Array(Box<SchemaTypeRef>),
    Map(Box<SchemaTypeRef>),
    Named(SchemaTypeId),
    Union(Vec<SchemaTypeRef>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SchemaLiteral {
    String(String),
    Integer(i64),
    Number(String),
    Boolean(bool),
    Null,
}
```

Use a stable textual representation for non-integral JSON numbers rather than using `f64` as graph identity.

Normalize unions by flattening nested unions, deduplicating structurally equal arms, and ordering them deterministically. Preserve `Null` as an arm. Requiredness and nullability are different properties and must never be collapsed.

### Objects and fields

```rust
#[derive(Debug, Clone)]
pub struct SchemaObject {
    pub fields: Vec<SchemaField>,
    pub additional_properties: AdditionalProperties,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SchemaField {
    pub wire_name: String,
    pub preferred_identifier: String,
    pub ty: SchemaTypeRef,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AdditionalProperties {
    AllowedAny,
    Forbidden,
    Typed(Box<SchemaTypeRef>),
}
```

Renderers decide whether a wire name is a legal identifier. If it is not, BAML uses a safe identifier plus `@alias`, while TypeScript uses a quoted property key.

Do not infer optionality from `null`. A required `string | null` field remains required. A non-required `string` field remains omittable.

Render fixed properties plus `AllowedAny` as a BAML dynamic class using `@@dynamic`. Render `Forbidden` as a normal closed generated class. Pure typed maps remain supported. The first implementation may reject fixed properties plus typed `additionalProperties` if BAML cannot express that mixed shape without changing semantics.

### Enums

```rust
#[derive(Debug, Clone)]
pub struct SchemaEnum {
    pub variants: Vec<SchemaEnumVariant>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SchemaEnumVariant {
    pub wire_value: SchemaLiteral,
    pub preferred_identifier: String,
    pub description: Option<String>,
}
```

BAML enum identifiers are an output concern. The graph preserves the wire literal. TypeScript will render string or numeric literal unions directly.

## Schema lowering rules

### Root collection

For each tool, collect roots in this stable order:

1. Open input, unless unit/absent.
2. Send input.
3. Output.

Sort tools by qualified `ToolName` before lowering. Root names come from `ToolTypeSpec.name`; the DTO shape comes only from the corresponding schema.

Reject duplicate tool names before lowering. Reject a root name that is empty, a primitive spelling such as `string`, or an invalid public identifier unless an existing explicit normalization rule applies.

### Local definitions and references

Support local references through `#/$defs/...` and `#/definitions/...`. Resolve JSON Pointer escaping correctly. Keep references as graph edges after lowering.

External references are out of scope and must produce a source-located build error unless import normalization has already replaced the schema with `OpaqueJson`.

Detect reference cycles. Recursive aliases can remain graph cycles if both target renderers support them. Recursive object references should be accepted. A cycle that cannot be represented in BAML must fail before rendering with the cycle path in the diagnostic.

### Named objects, enums, and aliases

Create a declaration when a schema is used as a public root, appears in `$defs`/`definitions`, has a usable `title`, or is an inline object/enum that needs a reusable target-language name.

Inline naming preference:

1. Explicit and valid schema `title`.
2. `$defs` or `definitions` key.
3. Parent declaration plus PascalCase wire field name.
4. Parent declaration plus `VariantN`, `Item`, or `Value`, depending on the edge.

Do not use traversal counters alone for names when a semantic path is available.

### Objects

Lower `properties` into sorted `SchemaField` entries. Read requiredness from the parent's `required` array. Preserve `description` from each property.

Interpret `additionalProperties` explicitly:

- missing means `AllowedAny` according to JSON Schema;
- `true` means `AllowedAny`;
- `false` means `Forbidden`;
- a schema means `Typed`.

For fixed-shape objects, map `AllowedAny` to a BAML `@@dynamic` class and `Forbidden` to a normal generated class. If BAML cannot preserve a typed additional-property schema alongside fixed fields, reject that mixed form. Do not silently pretend an open object is closed.

A root `{}` means `JsonValue`. An object with neither properties nor an `additionalProperties` schema must no longer produce an ambiguous empty class.

### Primitives and constraints

Map JSON Schema types as follows:

| JSON Schema | Graph |
| --- | --- |
| `string` | `String` |
| `integer` | `Integer` |
| `number` | `Number` |
| `boolean` | `Boolean` |
| `null` | `Null` |
| `array` | `Array(items)` |
| object map | `Map(value)` |

Type renderers do not need to encode validation-only keywords such as `minimum`, `maximum`, `minLength`, or `format`. Runtime validation continues to use the original schema.

If an array has no `items`, lower it to `JsonValue[]` only if that matches existing accepted behavior. Otherwise reject it. Pin the decision in the support matrix test.

### Literals and enums

Lower `const` into `Literal`. Lower homogeneous `enum` arrays into `SchemaEnum` or a literal union. Preserve descriptions on the schemars-style `oneOf` of `const` variants.

Mixed-type enums are accepted only if both BAML and TypeScript can represent the resulting literal union. Otherwise reject them with a clear diagnostic.

### Unions and nullable values

Support:

- `oneOf`;
- `anyOf`;
- `type: ["T", "null"]`;
- the existing `vec_or_one` shape;
- tagged object unions produced by static/manual schemas.

Do not treat `oneOf` and `anyOf` validation semantics as equivalent in the graph without documenting that the graph captures code-generation shape, not complete validator semantics. The original schema remains the validation authority.

`allOf`, `not`, conditional schemas, `patternProperties`, and dependent schemas remain unsupported unless the implementation explicitly adds and tests them. MCP's existing normalizer may convert these to `OpaqueJson`; other origins should fail rather than degrade silently.

### Opaque JSON

Recognize `OPAQUE_JSON_SCHEMA_MARKER_KEY` and lower it to `JsonValue`. Preserve the current BAML spelling `OpaqueJson` where generated BAML expects that alias, while TypeScript later renders it as `JsonValue` or a stable `OpaqueJson` alias according to the dependent plan.

The graph should carry an annotation if the renderer must preserve the public alias name rather than inline `JsonValue`.

### Boolean schemas

Define explicit behavior instead of inheriting the current accidental error:

- `true` may lower to `JsonValue`;
- `false` may lower to `Never` if BAML can represent it, otherwise it is unsupported.

Pin this in the schema support matrix. Do not leave boolean schemas dependent on which recursive function encounters them.

## Symbol allocation and collision rules

Schema identity and emitted symbol names are separate.

Compute a canonical fingerprint from the normalized declaration shape, excluding descriptions but including wire names, requiredness, literals, references after canonical remapping, and additional-property behavior. Use JCS or an equivalent deterministic structural encoding.

Apply these rules:

1. A public root keeps its requested name.
2. Two public roots with the same name and different fingerprints are a hard build error naming both tools and roles.
3. Two declarations with the same preferred symbol and identical fingerprints may share one emitted declaration.
4. Conflicting nested declarations are qualified with their owning tool class, for example `SupportSlackPagination`.
5. If qualification still conflicts, include the stable schema path. Use a short digest only as a final fallback.
6. A nested declaration must never force a public root to be renamed.
7. All references use `SchemaTypeId`, so symbol renaming cannot leave stale string references.

Reserved bootstrap symbols such as `JsonValue`, `StructuredReply`, and `ToolFailure` must be rejected or renamed before rendering. Maintain one reserved-symbol set at the final declaration registry boundary, not separate ad hoc lists in each renderer.

## Static derive correctness work

Using JSON Schema as the shared input makes shortcomings in `JsonSchemaType` visible. Fix or explicitly reject them before declaring the schema canonical.

### Required derive support

For named structs deriving `BamlType`, generated JSON Schema must honor the serde input wire contract for:

- container `rename_all`;
- field `rename`;
- `Option<T>` omission;
- field or container `default` where omission is accepted;
- `deny_unknown_fields` through `additionalProperties: false`;
- skipped fields when serde and BAML agree they are not input fields.

For enums and unions, preserve the current wire values from serde rename rules and add a stable `title` for named schemas. Structs already include a title; unit enums and union schemas should do the same.

Treat BAML and serde skips as different controls, not alternatives. `#[serde(skip)]` and
`#[serde(skip_deserializing)]` describe the wire and must remove a field or unit-enum variant from
the input schema. `#[baml(skip)]` controls model exposure and removes the declaration from BAML,
TypeScript, and the schema even when serde still accepts it. `#[serde(skip_serializing)]` leaves
input capability intact, so it stays in the input schema.

### Unsupported serde forms

Inventory static tool DTOs for:

- `flatten`;
- internally or adjacently tagged enums;
- `untagged` enums not represented through `#[baml(union)]`;
- custom deserializers;
- aliases that accept multiple input spellings;
- asymmetric serialize/deserialize renames.

If the derive cannot represent a form faithfully, require a manual `JsonSchemaType` implementation or fail the derive with a targeted message. Do not emit a plausible but wrong schema.

System One is the reference for a justified manual implementation.

### Conformance evidence

Add a matrix of representative static DTO values that:

1. serializes the Rust value;
2. validates the JSON against `json_schema_value::<T>()`;
3. checks omitted default/optional fields;
4. checks canonical renamed field and enum spellings;
5. rejects disallowed unknown fields where `deny_unknown_fields` applies.

This verifies the source schema before graph rendering is tested.

## BAML rendering

The graph BAML renderer replaces direct schema parsing in `schema_to_baml.rs`.

### Renderer contract

```rust
pub fn render_tool_schema_baml(graph: &ToolSchemaGraph) -> Result<String>;
```

The renderer must:

- emit declarations in dependency-safe, deterministic order;
- emit each graph declaration once;
- preserve field descriptions;
- distinguish optional fields from nullable field types;
- emit safe identifiers and `@alias` for wire names when needed;
- emit `@@dynamic` for fixed objects whose graph permits arbitrary additional properties;
- map integers to `int` and numbers to `float`;
- map arrays, maps, literals, unions, enums, and recursive aliases;
- preserve `OpaqueJson` handling;
- reject graph shapes BAML cannot represent instead of widening them.

Strongly connected declaration components should be emitted in a deterministic order. If the BAML compiler cannot accept a particular recursive component, report all symbols in the component.

### Tool cards and session interfaces

`generate_tool_card_baml` and `generate_tool_baml_interface` should take root references from `ToolSchemaContract`, not reconstruct names from raw metadata independently.

They still use metadata for descriptions, capabilities, tags, access, and session policy.

Open-step generation must use `OpenInputContract` for both type and requiredness. This prevents the BAML interface, future TypeScript signature, and runtime open validation from making three independent decisions.

### Migrating `baml_decl`

Adopt graph rendering in two stages inside the same implementation branch:

1. Dual-build static tools in tests. Render their existing `baml_decl` and graph BAML, compile both, and compare normalized IR signatures for DTOs. Record deliberate differences.
2. Switch production prelude generation to graph BAML for all tool origins.

After the switch, the builder ignores `ToolFunctionMetadata.baml_decl` for Open/Input/Output DTO generation. Keep the field in metadata for wire compatibility until a later cleanup proves no other consumer needs it.

BAML-specific descriptions or aliases that cannot survive the schema path must become schema annotations or graph annotations before the switch. Do not retain a static-only production bypass.

## Builder pipeline changes

Lower the graph immediately after resolving manifest tool metadata:

```rust
pub(super) struct WorkspaceReady {
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub tool_schema_graph: Arc<ToolSchemaGraph>,
    // existing fields
}
```

Proposed flow:

```text
WorkspaceReady::materialize
  load manifest tool names
  resolve ToolFunctionMetadata once
  ToolSchemaGraph::from_tools(&tool_metadata)
  retain Arc<ToolSchemaGraph>

WorkspaceReady::emit_tool_interfaces_prelude
  render prelude from tool_metadata + graph
  append coordination_baml

CompiledFirstPass and later phases
  carry the same Arc<ToolSchemaGraph>

RuntimeFinalized
  expose graph to the dependent TypeScript declaration generator
```

Add a metadata-based prelude entry point:

```rust
render_baml_tool_interfaces_from_metadata(
    tools: &[ToolFunctionMetadata],
    graph: &ToolSchemaGraph,
) -> Result<String>
```

Migrate the pipeline to it. Compatibility helpers that accept names and resolve catalogs may remain for isolated callers temporarily, but the main pipeline must not perform a second catalog lookup.

## Proposed file changes

| File or module | Change |
| --- | --- |
| `crates/baml-rt-builder/src/builder/tool_schema_graph/mod.rs` | Add graph types, `from_tools`, public renderer-neutral queries, and symbol inventory. |
| `tool_schema_graph/lower.rs` | Move JSON Schema traversal out of string rendering; add source-located diagnostics and normalization. |
| `tool_schema_graph/names.rs` | Add deterministic public/nested symbol allocation and collision policy. |
| `tool_schema_graph/baml.rs` | Render BAML from graph nodes. |
| `crates/baml-rt-builder/src/builder/schema_to_baml.rs` | Convert into a compatibility wrapper, then remove duplicated traversal. |
| `crates/baml-rt-builder/src/builder/baml_gen/tool_interfaces.rs` | Consume resolved metadata and graph; remove static `baml_decl` versus schema branching. |
| `crates/baml-rt-builder/src/builder/compiler/codegen_pipeline.rs` | Build graph once in `WorkspaceReady`, carry it through phase structs, and pass it to prelude generation. |
| `crates/baml-derive/src/expand.rs` and attribute parsing | Make generated JSON Schema honor supported serde wire names/defaults/closed objects; add titles to named enums/unions. |
| `crates/baml-derive-core/src/schema.rs` | Update the `JsonSchemaType` contract and document supported wire semantics. |
| `crates/baml-rt-builder/tests/` | Add graph boundary snapshots, static/external parity, collision matrix, and build vertical slices. |

## Implementation checklist

Complete these four tasks in order. Each task should end in a reviewable commit with its own tests passing. Do not begin the TypeScript tool-session work in this branch.

### Task 1: establish the schema boundary and make static schemas wire-accurate

- [ ] Read the files listed under [Handoff notes](#handoff-notes-for-the-implementing-agent) completely and inventory every caller of `generate_baml_types_from_schemas`, `baml_decl`, `JsonSchemaType`, and `schema_allows_empty_or_optional_open_input`.
- [ ] Add one normalized boundary snapshot for the existing JSON Schema to BAML corpus. It must cover nested structs and enums, optional and nullable fields, arrays, maps, a tagged union, `vec_or_one`, local `$defs` references, `OpaqueJson`, and one unsupported construct with its diagnostic.
- [ ] Add a single serde/schema conformance matrix for representative static tool DTOs. Serialize real Rust values and validate them against `json_schema_value::<T>()`.
- [ ] Extend derive attribute parsing and `JsonSchemaType` generation to honor supported input-wire semantics:
  - container `rename_all`;
  - field `rename`;
  - field and container `default` where omission is accepted;
  - `Option<T>` omission;
  - `deny_unknown_fields` as `additionalProperties: false`;
  - skipped fields where serde and BAML agree they are absent from input.
- [ ] Add stable `title` values to named enum and union schemas. Struct schemas already carry their type name.
- [ ] Inventory unsupported serde forms such as `flatten`, custom deserializers, tagged or untagged enums outside the supported derive shape, aliases, and asymmetric names. Make the derive reject forms it cannot describe, or require a manual `JsonSchemaType` implementation.
- [ ] Update existing schema snapshots only where the previous schema disagreed with the serde wire contract. Record those intentional changes in the commit message.
- [ ] Run the derive and schema-focused tests before completing the task.

Task 1 is complete when static tool metadata can be trusted as the same kind of schema input already supplied by external and MCP tools.

### Task 2: introduce `ToolSchemaGraph`, deterministic lowering, and diagnostics

- [ ] Add `builder/tool_schema_graph/` with the renderer-neutral types described in this plan:
  - `ToolSchemaGraph` and `ToolSchemaContract`;
  - `OpenInputContract` and `OpenInputRequirement`;
  - `SchemaTypeId`, `SchemaLocation`, and `SchemaOwner`;
  - declarations for objects, enums, and aliases;
  - `SchemaTypeRef`, literals, fields, and additional-property policy.
- [ ] Implement `ToolSchemaGraph::from_tools(&[ToolFunctionMetadata])`. Sort tools and roots before lowering and consume only resolved metadata passed by the caller.
- [ ] Implement JSON Schema lowering for the currently supported shapes: primitives, literals, homogeneous enums, arrays, maps, fixed objects, dynamic objects, local references, nullable values, `oneOf`, `anyOf`, `vec_or_one`, tagged object unions, and `OpaqueJson`.
- [ ] Preserve requiredness independently from nullability. A required nullable field and an optional non-null field must remain distinct graph shapes.
- [ ] Resolve `#/$defs/...` and `#/definitions/...` with correct JSON Pointer escaping. Reject external references with a diagnostic containing tool name, role, and pointer.
- [ ] Detect recursive declarations and retain graph edges by `SchemaTypeId`. Reject only cycles that the target BAML representation cannot support.
- [ ] Implement deterministic union normalization, declaration dependencies, canonical fingerprints, and source locations.
- [ ] Implement symbol allocation and collision policy:
  - public roots keep their requested names;
  - conflicting public roots fail;
  - identical nested declarations may deduplicate;
  - conflicting nested declarations receive deterministic tool/path qualification;
  - reserved runtime names cannot be claimed silently.
- [ ] Move OpenInput absent, optional, and required classification into the graph. Reuse the existing policy rather than duplicating its logic.
- [ ] Add one support/diagnostic matrix and one collision matrix. Add a normalized graph boundary snapshot containing ownership, fields, requiredness, dependencies, and fingerprints.

Task 2 is complete when the graph can represent every schema currently accepted by the builder and reports unsupported shapes before any target-language text is emitted.

### Task 3: render BAML from the graph and prove static/external parity

- [ ] Add `tool_schema_graph/baml.rs` with `render_tool_schema_baml(&ToolSchemaGraph)`.
- [ ] Render declarations in deterministic dependency order. Handle strongly connected components explicitly and report an unsupported recursive component with all participating symbols.
- [ ] Preserve wire names, descriptions, aliases, requiredness, nullable unions, arrays, maps, enums, literals, and `OpaqueJson`.
- [ ] Render fixed objects with `AllowedAny` as `@@dynamic` classes and `Forbidden` as normal generated classes. Continue to support pure typed maps. Reject an unrepresentable mix of fixed fields and typed additional properties.
- [ ] Route the compatibility `generate_baml_types_from_schemas` API through graph lowering and graph rendering without changing callers yet.
- [ ] Add dual-render comparison tests for static tools that currently provide `baml_decl`. Compile existing and graph-rendered BAML and compare normalized DTO IR signatures, not only text.
- [ ] Resolve every semantic difference in aliases, field names, optionality, unions, descriptions, and nested symbols. Snapshot deliberate text changes.
- [ ] Add a parity fixture in which equivalent static and external schemas produce equivalent normalized graph declarations and BAML after root-name normalization.
- [ ] Regenerate and compile representative Notion, Slack or ClickUp, external-tool, and MCP fixtures.

Task 3 is complete when the graph renderer reproduces the supported BAML contract and equivalent schemas no longer depend on whether the tool was linked statically or imported.

### Task 4: make the graph the production pipeline owner and finish the migration

- [ ] Construct `Arc<ToolSchemaGraph>` in `WorkspaceReady::materialize` immediately after resolving `tool_metadata`.
- [ ] Carry the same graph instance through every codegen phase to `RuntimeFinalized`. Do not lower schemas again in later phases.
- [ ] Add `render_baml_tool_interfaces_from_metadata(tools, graph)` and migrate the main pipeline to it.
- [ ] Make tool cards and generated session interfaces use graph root references and graph OpenInput classification.
- [ ] Remove the main pipeline's second catalog or inventory lookup. Resolved `tool_metadata` from `WorkspaceReady` is the only inventory input.
- [ ] Switch all production tool DTO BAML generation to the graph, including static tools. Stop consuming `baml_decl` for Open/Input/Output DTO output.
- [ ] Keep `baml_decl`, `ToolTypeSpec.ts_decl`, and `extra_ts_decls` serialized for compatibility, but document that builder DTO generation no longer consumes them. Do not remove those fields in this task.
- [ ] Delete the old intertwined schema traversal after all callers use the graph. Keep only a compatibility wrapper if a non-pipeline caller still requires the old function signature.
- [ ] Run the authority vertical slice with static, external, typed or opaque MCP, and BAML-referenced tool types. Regenerate all affected fixtures and review snapshots.
- [ ] Update crate documentation and the host-tool reference with the schema-first contract and explicit unsupported-schema behavior.
- [ ] Move the completed plan to `docs/plans/tool-schema-graph.md`, create the directory if necessary, and add it to `docs/README.md`.
- [ ] Run all commands listed under [Verification strategy](#verification-strategy), including formatting, clippy, builder tests, derive tests, and fixture regeneration.

Task 4 is complete when every tool origin uses one graph instance for generated BAML, `RuntimeFinalized` carries that graph for the dependent TypeScript work, and this branch adds no TypeScript tool-session API.

## Verification strategy

Follow `docs/assertions/testing-handbook.md`. Prefer boundary and vertical evidence over helper-level test shards.

### Authority vertical slice

Build one agent containing:

- one static tool;
- one external tool with an equivalent input/output shape;
- one MCP-style opaque fallback;
- one BAML function that references a generated tool type.

Assert that the build succeeds, the generated prelude compiles, and both typed tools produce equivalent normalized DTO declarations.

### Boundary snapshots

Snapshot a normalized graph projection rather than debug output. Include:

```text
public symbol
owner tool and role
kind
field wire names
requiredness
normalized type expression
dependencies
fingerprint
```

Also snapshot the rendered BAML for the representative corpus.

### Contract matrices

Use table-driven `*_matrix` tests for:

- supported JSON Schema forms;
- unsupported forms and diagnostic paths;
- symbol collision outcomes;
- OpenInput absent, optional, and required classification;
- serde rename/default/deny-unknown derive behavior.

### Regression fixtures

At minimum, regenerate and compile:

- Notion, for nested enums/unions and the current `BlockRenderMode` overlap;
- Slack or ClickUp, for a large static DTO graph;
- an external-tool fixture;
- an MCP fixture with typed input and opaque fallback;
- System One on the integration branch after its tool metadata is present.

### Commands

The implementing agent should run the narrow tests while iterating, then:

```bash
just fmt
just clippy
just test-crate baml-derive
just test-crate baml-rt-builder
just regen-fixtures
```

Run the relevant external/MCP crate tests when their fixtures or normalization contracts change. Use `cargo insta review` for intentional boundary snapshot changes.

## Acceptance criteria

- Every resolved tool origin lowers through `ToolSchemaGraph`.
- The main codegen pipeline resolves tool metadata once and lowers one graph instance.
- Generated BAML tool DTOs come from the graph for static and imported tools.
- Equivalent static and external schemas produce equivalent normalized graph shapes.
- Static schema generation honors the supported serde wire contract or fails explicitly.
- Unsupported schemas never silently become typed empty objects.
- Public-root conflicts fail with both tools, roles, and schema locations in the message.
- Nested collisions are resolved deterministically without renaming public roots.
- Existing supported builder fixtures compile after regeneration.
- `RuntimeFinalized` carries the graph needed by `tool-typescript-codegen.md`.
- The builder no longer needs `ts_decl` or `extra_ts_decls` to describe tool DTOs.
- No TypeScript tool-session API is added in this branch.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Static derive schemas do not match serde | Fix supported serde attributes first; conformance matrix validates serialized values against generated schemas. |
| Static BAML loses descriptions or aliases when `baml_decl` is removed | Preserve them in schema/graph annotations; dual-compile and compare normalized IR before switching. |
| External schemas use unsupported JSON Schema | Keep explicit errors and MCP `OpaqueJson` policy; never widen silently. |
| Nested names collide across tools | Scoped graph identities plus deterministic symbol allocation; public roots never renamed silently. |
| Refactor changes all generated files at once | First route the compatibility API through the graph, then switch pipeline ownership; review boundary snapshots and IR differences. |
| Graph becomes a second validator | Keep runtime validation on the original schema; graph only models code-generation shape. |
| `TsType` consumers outside builder break | Do not remove metadata fields in this branch; stop consuming them only in builder tool DTO generation. |
| Implementation branch grows into TypeScript work | Acceptance explicitly excludes TypeScript session declarations; merge graph first. |

## Handoff notes for the implementing agent

Start by reading these files completely:

- `crates/baml-rt-builder/src/builder/schema_to_baml.rs`
- `crates/baml-rt-builder/src/builder/baml_gen/tool_interfaces.rs`
- `crates/baml-rt-builder/src/builder/compiler/codegen_pipeline.rs`
- `crates/baml-derive/src/expand.rs`
- `crates/baml-derive-core/src/schema.rs`
- `crates/baml-rt-tools/src/tool_schema.rs`
- `crates/baml-rt-tools/src/tools.rs`
- `crates/baml-rt-tools/src/mcp_schema_normalize.rs`
- `crates/baml-rt-tools/src/mcp_builder_catalog.rs`
- `crates/baml-rt-tools/src/external_tools/metadata.rs`

Do not begin by adding a TypeScript renderer. First make the graph capable of reproducing the existing BAML boundary and make the pipeline carry the graph. The TypeScript renderer is a consumer and belongs on the next branch.

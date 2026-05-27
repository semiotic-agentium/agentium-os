// Fixture types are used only by the derive macro; fields are serialised/deserialised
// by generated impls, not read in test code.
#![expect(
    dead_code,
    reason = "derive fixtures define types exercised only by the generated TypeScript output"
)]

use std::collections::HashMap;

use baml_derive::BamlType;
use baml_derive_core::TsType as TsTypeTrait;

// ─── Simple struct → export interface ────────────────────────────

#[derive(BamlType)]
struct TsSimpleInput {
    pub name: String,
    pub count: i32,
    pub active: bool,
    pub score: f64,
}

#[test]
fn simple_struct_ts_decl() {
    insta::assert_snapshot!(TsSimpleInput::ts_decl().unwrap(), @r"
    export interface TsSimpleInput {
      name: string;
      count: number;
      active: boolean;
      score: number;
    }
    ");
}

#[test]
fn simple_struct_ts_name() {
    assert_eq!(TsSimpleInput::ts_type_name(), "TsSimpleInput");
}

// ─── #[baml(vec_or_one)] — T | T[] ───────────────────────────────

#[derive(BamlType)]
struct TsVecOrOne {
    #[baml(vec_or_one)]
    pub items: Option<Vec<String>>,
}

#[test]
fn vec_or_one_ts_decl() {
    insta::assert_snapshot!(TsVecOrOne::ts_decl().unwrap(), @r"
    export interface TsVecOrOne {
      items: (string | string[]) | null;
    }
    ");
}

// ─── Struct with Option, Vec, HashMap ────────────────────────────

#[derive(BamlType)]
struct TsComplexInput {
    pub query: String,
    pub tags: Vec<String>,
    pub limit: Option<i32>,
    pub metadata: HashMap<String, String>,
}

#[test]
fn complex_struct_ts_type_resolution() {
    insta::assert_snapshot!(TsComplexInput::ts_decl().unwrap(), @r"
    export interface TsComplexInput {
      query: string;
      tags: string[];
      limit: number | null;
      metadata: Record<string, string>;
    }
    ");
}

// ─── Struct with nested user type ────────────────────────────────

#[derive(BamlType)]
enum TsPriority {
    Low,
    Medium,
    High,
}

#[derive(BamlType)]
struct TsTaskInput {
    pub title: String,
    pub priority: TsPriority,
    pub assignees: Vec<String>,
}

#[test]
fn nested_user_type_ts_decl() {
    insta::assert_snapshot!(TsTaskInput::ts_decl().unwrap(), @r"
    export interface TsTaskInput {
      title: string;
      priority: TsPriority;
      assignees: string[];
    }
    ");
}

#[test]
fn nested_type_ts_dependencies() {
    let deps = TsTaskInput::ts_dependencies();
    assert_eq!(deps, vec!["TsPriority"]);
}

// ─── #[baml(skip)] omits field from TypeScript ───────────────────

#[derive(BamlType)]
struct TsSkipField {
    pub public_name: String,
    #[baml(skip)]
    pub internal_cache: String,
    pub value: i32,
}

#[test]
fn skip_field_absent_in_ts() {
    let decl = TsSkipField::ts_decl().unwrap();
    assert!(
        !decl.contains("internal_cache"),
        "skipped field must not appear in TS: {decl}"
    );
    insta::assert_snapshot!(decl, @r"
    export interface TsSkipField {
      public_name: string;
      value: number;
    }
    ");
}

// ─── #[baml(alias)] does NOT affect TypeScript field name ────────

#[derive(BamlType)]
struct TsAliasField {
    #[baml(alias = "task_identifier")]
    pub task_id: String,
    pub name: String,
}

#[test]
fn alias_does_not_change_ts_field_name() {
    let decl = TsAliasField::ts_decl().unwrap();
    // TypeScript uses the Rust name, not the BAML alias
    assert!(
        decl.contains("task_id"),
        "TS should use the Rust field name: {decl}"
    );
    assert!(
        !decl.contains("task_identifier"),
        "TS must not contain the BAML alias: {decl}"
    );
    insta::assert_snapshot!(decl, @r"
    export interface TsAliasField {
      task_id: string;
      name: string;
    }
    ");
}

// ─── Unit enum → string literal union ────────────────────────────

#[derive(BamlType)]
enum TsStatus {
    Open,
    InProgress,
    Closed,
}

#[test]
fn unit_enum_ts_decl() {
    insta::assert_snapshot!(TsStatus::ts_decl().unwrap(), @r#"export type TsStatus = "Open" | "InProgress" | "Closed";"#);
}

#[test]
fn unit_enum_ts_name() {
    assert_eq!(TsStatus::ts_type_name(), "TsStatus");
}

// ─── Unit enum with #[baml(skip)] omits variant from TS ──────────

#[derive(BamlType)]
enum TsStatusWithSkip {
    Open,
    InProgress,
    #[baml(skip)]
    Internal,
    Closed,
}

#[test]
fn unit_enum_skip_variant_ts() {
    let decl = TsStatusWithSkip::ts_decl().unwrap();
    assert!(
        !decl.contains("Internal"),
        "skipped variant must not appear in TS: {decl}"
    );
    insta::assert_snapshot!(decl, @r#"export type TsStatusWithSkip = "Open" | "InProgress" | "Closed";"#);
}

// ─── Union enum → TypeScript union type alias ─────────────────────

#[derive(BamlType)]
struct TsWeatherTool {
    pub location: String,
}

#[derive(BamlType)]
struct TsCalculatorTool {
    pub expression: String,
}

#[derive(BamlType)]
#[baml(union)]
enum TsToolChoice {
    Weather(TsWeatherTool),
    Calculator(TsCalculatorTool),
}

#[test]
fn union_enum_ts_decl() {
    insta::assert_snapshot!(
        TsToolChoice::ts_decl().unwrap(),
        @"export type TsToolChoice = TsWeatherTool | TsCalculatorTool;"
    );
}

#[test]
fn union_enum_ts_dependencies() {
    let deps = TsToolChoice::ts_dependencies();
    assert_eq!(deps, vec!["TsWeatherTool", "TsCalculatorTool"]);
}

// ─── Nested Option<Vec<T>> ───────────────────────────────────────

#[derive(BamlType)]
struct TsNestedGenerics {
    pub items: Option<Vec<String>>,
    pub matrix: Vec<Vec<i32>>,
}

#[test]
fn nested_generics_ts_decl() {
    insta::assert_snapshot!(TsNestedGenerics::ts_decl().unwrap(), @r"
    export interface TsNestedGenerics {
      items: string[] | null;
      matrix: number[][];
    }
    ");
}

// ─── Box<T> transparent ──────────────────────────────────────────

#[derive(BamlType)]
struct TsBoxedFields {
    pub data: Box<i32>,
    pub nested: Box<TsPriority>,
}

#[test]
fn box_transparent_ts() {
    insta::assert_snapshot!(TsBoxedFields::ts_decl().unwrap(), @r"
    export interface TsBoxedFields {
      data: number;
      nested: TsPriority;
    }
    ");
}

// ─── All numeric types → number ──────────────────────────────────

#[derive(BamlType)]
struct TsNumericTypes {
    pub a: i8,
    pub b: i16,
    pub c: i64,
    pub d: u8,
    pub e: u16,
    pub f: u32,
    pub g: u64,
    pub h: usize,
    pub i: isize,
    pub j: f32,
    pub k: f64,
}

#[test]
fn all_numeric_types_ts() {
    insta::assert_snapshot!(TsNumericTypes::ts_decl().unwrap(), @r"
    export interface TsNumericTypes {
      a: number;
      b: number;
      c: number;
      d: number;
      e: number;
      f: number;
      g: number;
      h: number;
      i: number;
      j: number;
      k: number;
    }
    ");
}

// ─── Struct with #[baml(type = "...")] override falls back to any ─

struct MyNewtype(String);

#[derive(BamlType)]
struct TsEscapeHatch {
    #[baml(type = "string")]
    pub custom_id: MyNewtype,
    pub name: String,
}

#[test]
fn type_override_ts_fallback_to_any() {
    // The explicit BAML type override has no TS mapping — falls back to `any`
    insta::assert_snapshot!(TsEscapeHatch::ts_decl().unwrap(), @r"
    export interface TsEscapeHatch {
      custom_id: any;
      name: string;
    }
    ");
}

// ─── render_ts_declarations helper ───────────────────────────────

#[test]
fn render_ts_declarations_joins_decls() {
    use baml_derive_core::render_ts_declarations;
    let decls = vec![TsStatus::ts_decl(), TsSimpleInput::ts_decl()];
    let output = render_ts_declarations(&decls);
    assert!(
        output.starts_with("// Auto-generated by baml-derive"),
        "missing header"
    );
    assert!(output.contains("export type TsStatus"), "missing enum decl");
    assert!(
        output.contains("export interface TsSimpleInput"),
        "missing interface decl"
    );
}

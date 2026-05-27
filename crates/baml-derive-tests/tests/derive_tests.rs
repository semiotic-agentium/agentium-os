// Fixture types are used only by the derive macro; fields are serialized/deserialized by generated impls, not read in test code.
#![expect(
    dead_code,
    reason = "derive fixtures define types exercised only by the generated BamlType output"
)]

use std::collections::HashMap;

use baml_derive::BamlType;
use baml_derive_core::{BamlDefinition, BamlType as BamlTypeTrait};

// ─── Simple struct ───────────────────────────────────────────────

#[derive(BamlType)]
struct SimpleInput {
    pub name: String,
    pub count: i32,
    pub active: bool,
    pub score: f64,
}

#[test]
fn simple_struct_type_name() {
    assert_eq!(SimpleInput::baml_type_name(), "SimpleInput");
}

#[test]
fn simple_struct_definition() {
    let def = SimpleInput::baml_definition();
    assert!(matches!(def, BamlDefinition::Class(_)));
    insta::assert_snapshot!(SimpleInput::baml_decl(), @r"
    class SimpleInput {
      name string
      count int
      active bool
      score float
    }
    ");
}

// ─── #[baml(vec_or_one)] — BAML T | T[] ───────────────────────────

#[derive(BamlType)]
struct VecOrOneInput {
    #[baml(vec_or_one)]
    pub items: Option<Vec<String>>,
}

#[test]
fn vec_or_one_baml_decl() {
    insta::assert_snapshot!(VecOrOneInput::baml_decl(), @r"
    class VecOrOneInput {
      items (string | string[])?
    }
    ");
}

// ─── Struct with Optional, Vec, HashMap ──────────────────────────

#[derive(BamlType)]
struct ComplexInput {
    pub query: String,
    pub tags: Vec<String>,
    pub limit: Option<i32>,
    pub metadata: HashMap<String, String>,
}

#[test]
fn complex_struct_type_resolution() {
    insta::assert_snapshot!(ComplexInput::baml_decl(), @r"
    class ComplexInput {
      query string
      tags string[]
      limit int?
      metadata map<string, string>
    }
    ");
}

// ─── Struct with nested user type ────────────────────────────────

#[derive(BamlType)]
enum Priority {
    Low,
    Medium,
    High,
}

#[derive(BamlType)]
struct TaskInput {
    pub title: String,
    pub priority: Priority,
    pub assignees: Vec<String>,
}

#[test]
fn nested_user_type() {
    insta::assert_snapshot!(TaskInput::baml_decl(), @r"
    class TaskInput {
      title string
      priority Priority
      assignees string[]
    }
    ");
}

#[test]
fn nested_type_dependencies() {
    let deps = TaskInput::baml_dependencies();
    assert_eq!(deps, vec!["Priority"]);
}

// ─── Struct with doc comment ─────────────────────────────────────

/// A user profile input
#[derive(BamlType)]
struct UserProfile {
    pub display_name: String,
    pub email: String,
}

#[test]
fn doc_comment_on_struct() {
    insta::assert_snapshot!(UserProfile::baml_decl(), @r"
    /// A user profile input
    class UserProfile {
      display_name string
      email string
    }
    ");
}

// ─── Struct with #[baml(...)] attributes ─────────────────────────

/// A ClickUp-like tool input
#[derive(BamlType)]
#[baml(dynamic)]
struct AnnotatedInput {
    pub action: String,
    #[baml(description = "Required for certain actions.")]
    pub team_id: Option<String>,
    #[baml(alias = "task_identifier")]
    pub task_id: Option<String>,
    #[baml(skip)]
    pub internal_cache: String,
}

#[test]
fn struct_with_attributes() {
    insta::assert_snapshot!(AnnotatedInput::baml_decl(), @r#"
    /// A ClickUp-like tool input
    class AnnotatedInput {
      action string
      team_id string? @description("Required for certain actions.")
      task_id string? @alias("task_identifier")
      @@dynamic
    }
    "#);
}

// ─── Struct with #[baml(type = "...")] escape hatch ──────────────

struct MyNewtype(String);

#[derive(BamlType)]
struct EscapeHatchInput {
    #[baml(type = "string")]
    pub custom_id: MyNewtype,
    pub name: String,
}

#[test]
fn type_override_escape_hatch() {
    insta::assert_snapshot!(EscapeHatchInput::baml_decl(), @r"
    class EscapeHatchInput {
      custom_id string
      name string
    }
    ");
}

// ─── Simple enum ─────────────────────────────────────────────────

#[derive(BamlType)]
enum MathOp {
    #[baml(alias = "+")]
    Add,
    #[baml(alias = "-")]
    Subtract,
    Multiply,
    Divide,
}

#[test]
fn simple_enum_type_name() {
    assert_eq!(MathOp::baml_type_name(), "MathOp");
}

#[test]
fn simple_enum_definition() {
    insta::assert_snapshot!(MathOp::baml_decl(), @r#"
    enum MathOp {
      Add @alias("+")
      Subtract @alias("-")
      Multiply
      Divide
    }
    "#);
}

// ─── Enum with skip and description ──────────────────────────────

/// Status values
#[derive(BamlType)]
enum Status {
    #[baml(description = "Task is open")]
    Open,
    InProgress,
    #[baml(skip)]
    Internal,
    Closed,
}

#[test]
fn enum_with_skip_and_description() {
    insta::assert_snapshot!(Status::baml_decl(), @r#"
    /// Status values
    enum Status {
      Open @description("Task is open")
      InProgress
      Closed
    }
    "#);
}

// ─── Union enum ──────────────────────────────────────────────────

#[derive(BamlType)]
struct WeatherTool {
    pub location: String,
}

#[derive(BamlType)]
struct CalculatorTool {
    pub expression: String,
}

#[derive(BamlType)]
#[baml(union)]
enum ToolChoice {
    Weather(WeatherTool),
    Calculator(CalculatorTool),
}

#[test]
fn union_enum_type_name() {
    assert_eq!(ToolChoice::baml_type_name(), "ToolChoice");
}

#[test]
fn union_enum_definition() {
    insta::assert_snapshot!(ToolChoice::baml_decl(), @"type ToolChoice = WeatherTool | CalculatorTool");
}

#[test]
fn union_enum_dependencies() {
    let deps = ToolChoice::baml_dependencies();
    assert_eq!(deps, vec!["WeatherTool", "CalculatorTool"]);
}

// ─── Nested Option<Vec<T>> ───────────────────────────────────────

#[derive(BamlType)]
struct NestedGenerics {
    pub items: Option<Vec<String>>,
    pub matrix: Vec<Vec<i32>>,
}

#[test]
fn nested_generics() {
    insta::assert_snapshot!(NestedGenerics::baml_decl(), @r"
    class NestedGenerics {
      items string[]?
      matrix int[][]
    }
    ");
}

// ─── Box<T> transparent wrapper ──────────────────────────────────

#[derive(BamlType)]
struct BoxedFields {
    pub data: Box<i32>,
    pub nested: Box<Priority>,
}

#[test]
fn box_transparent() {
    insta::assert_snapshot!(BoxedFields::baml_decl(), @r"
    class BoxedFields {
      data int
      nested Priority
    }
    ");
}

// ─── Various integer and float types ─────────────────────────────

#[derive(BamlType)]
struct NumericTypes {
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
}

#[test]
fn all_numeric_types() {
    insta::assert_snapshot!(NumericTypes::baml_decl(), @r"
    class NumericTypes {
      a int
      b int
      c int
      d int
      e int
      f int
      g int
      h int
      i int
      j float
    }
    ");
}

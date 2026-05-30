// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_derive_core::{
    BamlClassDef, BamlDefinition, BamlEnumDef, BamlFieldDef, BamlUnionDef, BamlVariantDef,
};

#[test]
fn render_simple_class() {
    let def = BamlDefinition::Class(BamlClassDef {
        name: "UserInput",
        doc: None,
        fields: vec![
            BamlFieldDef {
                name: "name",
                baml_type: "string".into(),
                alias: None,
                description: None,
                skip: false,
            },
            BamlFieldDef {
                name: "age",
                baml_type: "int".into(),
                alias: None,
                description: None,
                skip: false,
            },
        ],
        dynamic: false,
    });

    insta::assert_snapshot!(def.render(), @r"
    class UserInput {
      name string
      age int
    }
    ");
}

#[test]
fn render_class_with_doc_and_attributes() {
    let def = BamlDefinition::Class(BamlClassDef {
        name: "ClickUpInput",
        doc: Some("A ClickUp task input"),
        fields: vec![
            BamlFieldDef {
                name: "action",
                baml_type: "ClickUpAction".into(),
                alias: None,
                description: None,
                skip: false,
            },
            BamlFieldDef {
                name: "team_id",
                baml_type: "string?".into(),
                alias: None,
                description: Some("Required for ListSpaces."),
                skip: false,
            },
            BamlFieldDef {
                name: "task_id",
                baml_type: "string?".into(),
                alias: Some("task_identifier"),
                description: None,
                skip: false,
            },
            BamlFieldDef {
                name: "internal_cache",
                baml_type: "string".into(),
                alias: None,
                description: None,
                skip: true,
            },
        ],
        dynamic: true,
    });

    insta::assert_snapshot!(def.render(), @r#"
    /// A ClickUp task input
    class ClickUpInput {
      action ClickUpAction
      team_id string? @description("Required for ListSpaces.")
      task_id string? @alias("task_identifier")
      @@dynamic
    }
    "#);
}

#[test]
fn render_simple_enum() {
    let def = BamlDefinition::Enum(BamlEnumDef {
        name: "MathOperation",
        doc: None,
        variants: vec![
            BamlVariantDef {
                name: "Add",
                alias: Some("+"),
                description: None,
                skip: false,
            },
            BamlVariantDef {
                name: "Subtract",
                alias: Some("-"),
                description: None,
                skip: false,
            },
            BamlVariantDef {
                name: "Multiply",
                alias: None,
                description: None,
                skip: false,
            },
            BamlVariantDef {
                name: "Divide",
                alias: None,
                description: None,
                skip: false,
            },
        ],
    });

    insta::assert_snapshot!(def.render(), @r#"
    enum MathOperation {
      Add @alias("+")
      Subtract @alias("-")
      Multiply
      Divide
    }
    "#);
}

#[test]
fn render_enum_with_doc_and_skip() {
    let def = BamlDefinition::Enum(BamlEnumDef {
        name: "Status",
        doc: Some("Task status values"),
        variants: vec![
            BamlVariantDef {
                name: "Open",
                alias: None,
                description: Some("Task is open"),
                skip: false,
            },
            BamlVariantDef {
                name: "InProgress",
                alias: None,
                description: None,
                skip: false,
            },
            BamlVariantDef {
                name: "Internal",
                alias: None,
                description: None,
                skip: true,
            },
            BamlVariantDef {
                name: "Closed",
                alias: None,
                description: None,
                skip: false,
            },
        ],
    });

    insta::assert_snapshot!(def.render(), @r#"
    /// Task status values
    enum Status {
      Open @description("Task is open")
      InProgress
      Closed
    }
    "#);
}

#[test]
fn render_union_type() {
    let def = BamlDefinition::Union(BamlUnionDef {
        name: "ToolChoice",
        doc: None,
        variants: vec!["WeatherTool", "CalculatorTool"],
    });

    insta::assert_snapshot!(def.render(), @"type ToolChoice = WeatherTool | CalculatorTool");
}

#[test]
fn render_union_with_doc() {
    let def = BamlDefinition::Union(BamlUnionDef {
        name: "ToolChoice",
        doc: Some("Choose a tool"),
        variants: vec!["WeatherTool", "CalculatorTool", "SearchTool"],
    });

    insta::assert_snapshot!(def.render(), @r"
    /// Choose a tool
    type ToolChoice = WeatherTool | CalculatorTool | SearchTool
    ");
}

#[test]
fn render_class_with_complex_types() {
    let def = BamlDefinition::Class(BamlClassDef {
        name: "ComplexInput",
        doc: None,
        fields: vec![
            BamlFieldDef {
                name: "tags",
                baml_type: "string[]".into(),
                alias: None,
                description: None,
                skip: false,
            },
            BamlFieldDef {
                name: "metadata",
                baml_type: "map<string, string>".into(),
                alias: None,
                description: None,
                skip: false,
            },
            BamlFieldDef {
                name: "nested",
                baml_type: "InnerType?".into(),
                alias: None,
                description: None,
                skip: false,
            },
        ],
        dynamic: false,
    });

    insta::assert_snapshot!(def.render(), @r"
    class ComplexInput {
      tags string[]
      metadata map<string, string>
      nested InnerType?
    }
    ");
}

#[test]
fn render_multiple_definitions() {
    let defs = vec![
        BamlDefinition::Enum(BamlEnumDef {
            name: "Action",
            doc: None,
            variants: vec![
                BamlVariantDef {
                    name: "Create",
                    alias: None,
                    description: None,
                    skip: false,
                },
                BamlVariantDef {
                    name: "Delete",
                    alias: None,
                    description: None,
                    skip: false,
                },
            ],
        }),
        BamlDefinition::Class(BamlClassDef {
            name: "TaskInput",
            doc: None,
            fields: vec![BamlFieldDef {
                name: "action",
                baml_type: "Action".into(),
                alias: None,
                description: None,
                skip: false,
            }],
            dynamic: false,
        }),
    ];

    let output = baml_derive_core::render_baml_types(&defs);
    insta::assert_snapshot!(output, @r"
    // Auto-generated by baml-derive — do not edit.

    enum Action {
      Create
      Delete
    }

    class TaskInput {
      action Action
    }
    ");
}

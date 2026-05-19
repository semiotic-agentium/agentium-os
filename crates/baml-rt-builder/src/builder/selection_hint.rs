use baml_types::ir_type::TypeNonStreaming;
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::return_shape::{
    NestedTaggedUnionShape, ReturnShape, TaggedUnionShape, UntaggedUnionShape,
    analyze_named_union_shape, analyze_return_shape,
};

pub(crate) fn default_selection_hint() -> &'static str {
    "Return exactly one output matching the schema below. Do not add extra text before or after it.\n"
}

pub(crate) fn render_selection_hint_for_type(ty: &TypeNonStreaming, ir: &IRSignature) -> String {
    render_selection_hint(&analyze_return_shape(ty, ir))
}

pub(crate) fn render_type_reference_contract_for_named_union(
    legal_type_names: &[String],
) -> String {
    let mut type_names = legal_type_names.to_vec();
    type_names.sort();
    type_names.dedup();
    if type_names.len() == 1 {
        return format!("Return exactly one `{}` JSON object.\n", type_names[0]);
    }
    format!(
        "Return exactly one JSON object of type `{}`.\n",
        type_names.join(" | ")
    )
}

pub(crate) fn render_step_executor_selection_hint_for_named_union(
    legal_type_names: &[String],
    ir: &IRSignature,
) -> String {
    render_step_executor_selection_hint(&analyze_named_union_shape(legal_type_names, ir))
}

pub(crate) fn render_selection_hint(shape: &ReturnShape) -> String {
    match shape {
        ReturnShape::Scalar => default_selection_hint().to_string(),
        ReturnShape::SingleObject(single) => {
            let mut out = format!("Return exactly one `{}` JSON object.\n", single.type_name);
            for nested in &single.nested_tagged_unions {
                push_nested_tagged_union_hint(&mut out, nested);
            }
            out.push_str("Do not add text before or after the JSON object.\n");
            out
        }
        ReturnShape::TaggedUnion(tagged) => render_tagged_union_hint(tagged),
        ReturnShape::UntaggedUnion(union) => render_untagged_union_hint(union),
    }
}

fn render_step_executor_selection_hint(shape: &ReturnShape) -> String {
    match shape {
        ReturnShape::Scalar | ReturnShape::SingleObject(_) => {
            "Do not add text before or after the JSON object.\n".to_string()
        }
        ReturnShape::TaggedUnion(tagged) => format!(
            "{}Do not add text before or after the JSON object.\n",
            render_compact_tagged_union_hint(tagged)
        ),
        ReturnShape::UntaggedUnion(union) => format!(
            "{}Do not add text before or after the JSON object.\n",
            render_compact_untagged_union_hint(union)
        ),
    }
}

fn render_tagged_union_hint(tagged: &TaggedUnionShape) -> String {
    if tagged.discriminator == "op" {
        let mut values: Vec<String> = tagged
            .variants
            .iter()
            .map(|variant| format!("{:?}", variant.literal_value))
            .collect();
        values.sort();
        values.dedup();
        return format!(
            "Return exactly one JSON object.\nUse only the schema above.\nLegal operation discriminator values derived from this return union: `op` in {}.\nDo not add text before or after the JSON object.\n",
            values.join(" | ")
        );
    }
    let mut out = format!(
        "Return exactly one JSON object.\nSelect the object shape with discriminator `{}`:\n",
        tagged.discriminator
    );
    for variant in &tagged.variants {
        out.push_str("- `");
        out.push_str(&tagged.discriminator);
        out.push_str(": ");
        out.push_str(&format!("{:?}", variant.literal_value));
        out.push_str("` -> ");
        out.push_str(&variant.type_name);
        out.push('\n');
    }
    out.push_str("Set `");
    out.push_str(&tagged.discriminator);
    out.push_str("` exactly. Do not mix fields from different object shapes.\n");
    out.push_str("Do not add text before or after the JSON object.\n");
    out
}

fn render_compact_tagged_union_hint(tagged: &TaggedUnionShape) -> String {
    let mut values: Vec<String> = tagged
        .variants
        .iter()
        .map(|variant| format!("{:?}", variant.literal_value))
        .collect();
    values.sort();
    values.dedup();
    let mut out = format!(
        "Use `{}` as the discriminator: {}.\n",
        tagged.discriminator,
        values.join(" | ")
    );
    // Step-executor hops use `op`-discriminated session rows. If the model nests an object
    // where a string enum belongs (e.g. `operation`), jsonish stringifies it and substring enum
    // matching can tie across variants ("Too many matches for MathOperation").
    if tagged.discriminator == "op" {
        out.push_str(
            "Scalar leaves: use JSON primitives only where the schema names a string, number, enum, or boolean — never substitute an object or array for a scalar.\n",
        );
        let has_tool_send = tagged
            .variants
            .iter()
            .any(|v| v.type_name.ends_with("SendStep"));
        if has_tool_send {
            out.push_str(
                "Calculator Send: set input.expression.operation to exactly one JSON string token: \"+\", \"-\", \"*\", \"/\", or Add|Subtract|Multiply|Divide — never an object or sentence.\n",
            );
        }
    }
    out
}

fn render_untagged_union_hint(union: &UntaggedUnionShape) -> String {
    let mut out = "Return exactly one JSON object.\nChoose one object shape:\n".to_string();
    for variant in &union.variants {
        out.push_str("- `");
        out.push_str(&variant.type_name);
        out.push('`');
        if !variant.distinguishing_fields.is_empty() {
            out.push_str(" uses fields like ");
            let fields: Vec<String> = variant
                .distinguishing_fields
                .iter()
                .map(|field| format!("`{field}`"))
                .collect();
            out.push_str(&fields.join(", "));
        }
        out.push('\n');
    }
    out.push_str("Do not mix fields from different object shapes.\n");
    out.push_str("Do not add text before or after the JSON object.\n");
    out
}

fn render_compact_untagged_union_hint(union: &UntaggedUnionShape) -> String {
    let mut out = "Choose the matching object type.\n".to_string();
    for variant in &union.variants {
        out.push_str("- `");
        out.push_str(&variant.type_name);
        out.push('`');
        if !variant.distinguishing_fields.is_empty() {
            out.push_str(" uses fields like ");
            let fields: Vec<String> = variant
                .distinguishing_fields
                .iter()
                .map(|field| format!("`{field}`"))
                .collect();
            out.push_str(&fields.join(", "));
        }
        out.push('\n');
    }
    out.push_str("Do not mix fields from different object shapes.\n");
    out
}

fn push_nested_tagged_union_hint(out: &mut String, nested: &NestedTaggedUnionShape) {
    out.push_str("If `");
    out.push_str(&nested.path);
    out.push_str("` is present, choose each item with discriminator `");
    out.push_str(&nested.discriminator);
    out.push_str("`:\n");
    for variant in &nested.variants {
        out.push_str("- `");
        out.push_str(&nested.discriminator);
        out.push_str(": ");
        out.push_str(&format!("{:?}", variant.literal_value));
        out.push_str("` -> ");
        out.push_str(&variant.type_name);
        out.push('\n');
    }
    out.push_str("Set `");
    out.push_str(&nested.discriminator);
    out.push_str("` exactly for each `");
    out.push_str(&nested.path);
    out.push_str("` item.\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::return_shape::{
        NestedTaggedUnionShape, ObjectVariantShape, SingleObjectShape, TaggedVariantShape,
    };

    #[test]
    fn tagged_union_hint_mentions_discriminator_without_banning_markdown() {
        let hint = render_selection_hint(&ReturnShape::TaggedUnion(TaggedUnionShape {
            discriminator: "kind".to_string(),
            variants: vec![
                TaggedVariantShape {
                    type_name: "CoordinatorReadyIntent".to_string(),
                    literal_value: "ready".to_string(),
                },
                TaggedVariantShape {
                    type_name: "CoordinatorNeedTaskClarification".to_string(),
                    literal_value: "clarify".to_string(),
                },
            ],
        }));
        assert!(hint.contains("Select the object shape with discriminator `kind`"));
        assert!(hint.contains("`kind: \"ready\"` -> CoordinatorReadyIntent"));
        assert!(!hint.contains("No markdown"));
        assert!(!hint.contains("no prose"));
    }

    #[test]
    fn single_object_hint_mentions_nested_tagged_union() {
        let hint = render_selection_hint(&ReturnShape::SingleObject(SingleObjectShape {
            type_name: "StructuredReply".to_string(),
            nested_tagged_unions: vec![NestedTaggedUnionShape {
                path: "parts[]".to_string(),
                discriminator: "type".to_string(),
                variants: vec![
                    TaggedVariantShape {
                        type_name: "TextPart".to_string(),
                        literal_value: "text".to_string(),
                    },
                    TaggedVariantShape {
                        type_name: "DataPart".to_string(),
                        literal_value: "data".to_string(),
                    },
                ],
            }],
        }));
        assert!(hint.contains("Return exactly one `StructuredReply` JSON object."));
        assert!(hint.contains("If `parts[]` is present"));
        assert!(hint.contains("`type: \"text\"` -> TextPart"));
    }

    #[test]
    fn untagged_union_hint_lists_distinguishing_fields() {
        let hint = render_selection_hint(&ReturnShape::UntaggedUnion(UntaggedUnionShape {
            variants: vec![
                ObjectVariantShape {
                    type_name: "NeedClarification".to_string(),
                    distinguishing_fields: vec!["question".to_string()],
                },
                ObjectVariantShape {
                    type_name: "NotRelevant".to_string(),
                    distinguishing_fields: vec!["reason".to_string()],
                },
            ],
        }));
        assert!(hint.contains("`NeedClarification` uses fields like `question`"));
        assert!(hint.contains("`NotRelevant` uses fields like `reason`"));
    }

    #[test]
    fn type_reference_contract_sorts_named_members() {
        let contract = render_type_reference_contract_for_named_union(&[
            "FooSendStep".to_string(),
            "FooAbortStep".to_string(),
            "FooPageReadStep".to_string(),
        ]);
        assert!(contract.contains(
            "Return exactly one JSON object of type `FooAbortStep | FooPageReadStep | FooSendStep`."
        ));
        assert!(!contract.contains("Do not add text before or after the JSON object."));
    }

    #[test]
    fn compact_tagged_union_hint_is_discriminator_only() {
        let hint =
            render_step_executor_selection_hint(&ReturnShape::TaggedUnion(TaggedUnionShape {
                discriminator: "kind".to_string(),
                variants: vec![
                    TaggedVariantShape {
                        type_name: "Ready".to_string(),
                        literal_value: "ready".to_string(),
                    },
                    TaggedVariantShape {
                        type_name: "Clarify".to_string(),
                        literal_value: "clarify".to_string(),
                    },
                ],
            }));
        assert!(hint.contains("Use `kind` as the discriminator: \"clarify\" | \"ready\"."));
        assert!(!hint.contains("Select the object shape"));
        assert!(!hint.contains("Return exactly one JSON object."));
    }

    #[test]
    fn compact_op_discriminator_hint_warns_scalar_enum_for_send_steps() {
        let hint =
            render_step_executor_selection_hint(&ReturnShape::TaggedUnion(TaggedUnionShape {
                discriminator: "op".to_string(),
                variants: vec![
                    TaggedVariantShape {
                        type_name: "SupportCalculateSendStep".to_string(),
                        literal_value: "Send".to_string(),
                    },
                    TaggedVariantShape {
                        type_name: "SupportCalculateFinishStep".to_string(),
                        literal_value: "Finish".to_string(),
                    },
                ],
            }));
        assert!(hint.contains("Use `op` as the discriminator:"));
        assert!(hint.contains("Scalar leaves:"));
        assert!(hint.contains("Calculator Send:"));
        assert!(hint.contains("input.expression.operation"));
    }
}

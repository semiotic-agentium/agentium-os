// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet, HashSet};

use baml_types::ir_type::{LiteralValue, TypeNonStreaming, UnionTypeViewGeneric};
use internal_baml_core::ir::ir_hasher::IRSignature;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReturnShape {
    Scalar,
    SingleObject(SingleObjectShape),
    TaggedUnion(TaggedUnionShape),
    UntaggedUnion(UntaggedUnionShape),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SingleObjectShape {
    pub type_name: String,
    pub nested_tagged_unions: Vec<NestedTaggedUnionShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaggedUnionShape {
    pub discriminator: String,
    pub variants: Vec<TaggedVariantShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NestedTaggedUnionShape {
    pub path: String,
    pub discriminator: String,
    pub variants: Vec<TaggedVariantShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaggedVariantShape {
    pub type_name: String,
    pub literal_value: String,
}

/// Enum scalar field on a `*SendStep` `input` type tree, for compact step-executor selection hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendEnumScalarHint {
    pub send_step_type: String,
    pub json_path: String,
    pub enum_type: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UntaggedUnionShape {
    pub variants: Vec<ObjectVariantShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectVariantShape {
    pub type_name: String,
    pub distinguishing_fields: Vec<String>,
}

#[derive(Debug, Clone)]
struct ObjectVariantSurface {
    type_name: String,
    literal_fields: BTreeMap<String, String>,
    field_names: BTreeSet<String>,
}

pub(crate) fn analyze_return_shape(ty: &TypeNonStreaming, ir: &IRSignature) -> ReturnShape {
    analyze_type(ty, ir, &mut HashSet::new(), None)
}

pub(crate) fn analyze_named_union_shape(type_names: &[String], ir: &IRSignature) -> ReturnShape {
    let mut variants = Vec::new();
    for name in type_names {
        let Some(surface) = resolve_named_object_variant(name, ir, &mut HashSet::new()) else {
            return ReturnShape::Scalar;
        };
        variants.push(surface);
    }
    shape_from_object_variants(variants)
}

fn analyze_type(
    ty: &TypeNonStreaming,
    ir: &IRSignature,
    seen_aliases: &mut HashSet<String>,
    display_name: Option<&str>,
) -> ReturnShape {
    match ty {
        TypeNonStreaming::Class { name, .. } => analyze_named_object(name, display_name, ir),
        TypeNonStreaming::RecursiveTypeAlias { name, .. } => {
            if !seen_aliases.insert(name.clone()) {
                return ReturnShape::Scalar;
            }
            let out = ir
                .type_aliases
                .get(name)
                .map(|alias| analyze_type(alias.field_type.as_ref(), ir, seen_aliases, Some(name)))
                .unwrap_or(ReturnShape::Scalar);
            seen_aliases.remove(name);
            out
        }
        TypeNonStreaming::Union(union, _) => {
            let variants = match union.view() {
                UnionTypeViewGeneric::Null => return ReturnShape::Scalar,
                UnionTypeViewGeneric::Optional(inner) => vec![inner],
                UnionTypeViewGeneric::OneOf(variants)
                | UnionTypeViewGeneric::OneOfOptional(variants) => variants.to_vec(),
            };
            analyze_union_variants(variants, ir, seen_aliases)
        }
        TypeNonStreaming::Primitive(..)
        | TypeNonStreaming::Enum { .. }
        | TypeNonStreaming::Literal(..)
        | TypeNonStreaming::List(..)
        | TypeNonStreaming::Map(..)
        | TypeNonStreaming::Tuple(..)
        | TypeNonStreaming::Arrow(..)
        | TypeNonStreaming::Top(..) => ReturnShape::Scalar,
    }
}

fn analyze_named_object(name: &str, display_name: Option<&str>, ir: &IRSignature) -> ReturnShape {
    let Some(_) = ir.classes.get(name) else {
        return ReturnShape::Scalar;
    };
    ReturnShape::SingleObject(SingleObjectShape {
        type_name: display_name.unwrap_or(name).to_string(),
        nested_tagged_unions: collect_nested_tagged_unions_for_class(
            name,
            ir,
            &mut HashSet::new(),
            &mut HashSet::new(),
            "",
        ),
    })
}

fn analyze_union_variants(
    variants: Vec<&TypeNonStreaming>,
    ir: &IRSignature,
    seen_aliases: &mut HashSet<String>,
) -> ReturnShape {
    if variants.len() == 1 {
        return analyze_type(variants[0], ir, seen_aliases, None);
    }

    let mut object_variants = Vec::new();
    for variant in variants {
        let Some(surface) = resolve_object_variant(variant, ir, seen_aliases, None) else {
            return ReturnShape::Scalar;
        };
        object_variants.push(surface);
    }
    shape_from_object_variants(object_variants)
}

fn shape_from_object_variants(variants: Vec<ObjectVariantSurface>) -> ReturnShape {
    if variants.len() == 1 {
        return ReturnShape::SingleObject(SingleObjectShape {
            type_name: variants[0].type_name.clone(),
            nested_tagged_unions: Vec::new(),
        });
    }

    if let Some(discriminator) = choose_discriminator(&variants) {
        let tagged_variants = variants
            .into_iter()
            .map(|variant| TaggedVariantShape {
                type_name: variant.type_name,
                literal_value: variant
                    .literal_fields
                    .get(&discriminator)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();
        return ReturnShape::TaggedUnion(TaggedUnionShape {
            discriminator,
            variants: tagged_variants,
        });
    }

    let field_counts = build_field_counts(&variants);
    let variants = variants
        .into_iter()
        .map(|variant| {
            let mut distinguishing_fields: Vec<String> = variant
                .field_names
                .iter()
                .filter(|field| {
                    field_counts.get(*field).copied().unwrap_or_default() < field_counts.len()
                })
                .cloned()
                .collect();
            if distinguishing_fields.is_empty() {
                distinguishing_fields = variant
                    .field_names
                    .iter()
                    .filter(|field| !variant.literal_fields.contains_key(*field))
                    .cloned()
                    .collect();
            }
            ObjectVariantShape {
                type_name: variant.type_name,
                distinguishing_fields,
            }
        })
        .collect();
    ReturnShape::UntaggedUnion(UntaggedUnionShape { variants })
}

fn build_field_counts(variants: &[ObjectVariantSurface]) -> BTreeMap<String, usize> {
    let mut field_counts = BTreeMap::new();
    for variant in variants {
        for field in &variant.field_names {
            *field_counts.entry(field.clone()).or_default() += 1;
        }
    }
    field_counts
}

fn choose_discriminator(variants: &[ObjectVariantSurface]) -> Option<String> {
    let first = variants.first()?;
    let mut candidates = Vec::new();
    for field in first.literal_fields.keys() {
        let mut seen_values = BTreeSet::new();
        let mut ok = true;
        for variant in variants {
            let Some(value) = variant.literal_fields.get(field) else {
                ok = false;
                break;
            };
            if !seen_values.insert(value.clone()) {
                ok = false;
                break;
            }
        }
        if ok {
            candidates.push(field.clone());
        }
    }

    for preferred in ["op", "type", "kind"] {
        if let Some(found) = candidates
            .iter()
            .find(|candidate| candidate.as_str() == preferred)
        {
            return Some(found.clone());
        }
    }
    candidates.into_iter().next()
}

fn resolve_named_object_variant(
    name: &str,
    ir: &IRSignature,
    seen_aliases: &mut HashSet<String>,
) -> Option<ObjectVariantSurface> {
    if let Some((_, class_details)) = ir.classes.get(name) {
        let mut literal_fields = BTreeMap::new();
        let mut field_names = BTreeSet::new();
        for (field_name, field_ty) in class_details.fields.iter() {
            field_names.insert(field_name.clone());
            if let Some(literal) = literal_string(field_ty.as_ref()) {
                literal_fields.insert(field_name.clone(), literal);
            }
        }
        return Some(ObjectVariantSurface {
            type_name: name.to_string(),
            literal_fields,
            field_names,
        });
    }

    if !seen_aliases.insert(name.to_string()) {
        return None;
    }
    let out = ir.type_aliases.get(name).and_then(|alias| {
        resolve_object_variant(alias.field_type.as_ref(), ir, seen_aliases, Some(name))
    });
    seen_aliases.remove(name);
    out
}

fn resolve_object_variant(
    ty: &TypeNonStreaming,
    ir: &IRSignature,
    seen_aliases: &mut HashSet<String>,
    display_name: Option<&str>,
) -> Option<ObjectVariantSurface> {
    match ty {
        TypeNonStreaming::Class { name, .. } => {
            let mut surface = resolve_named_object_variant(name, ir, seen_aliases)?;
            if let Some(display_name) = display_name {
                surface.type_name = display_name.to_string();
            }
            Some(surface)
        }
        TypeNonStreaming::RecursiveTypeAlias { name, .. } => {
            if !seen_aliases.insert(name.clone()) {
                return None;
            }
            let out = ir.type_aliases.get(name).and_then(|alias| {
                resolve_object_variant(alias.field_type.as_ref(), ir, seen_aliases, Some(name))
            });
            seen_aliases.remove(name);
            out
        }
        _ => None,
    }
}

fn collect_nested_tagged_unions_for_class(
    class_name: &str,
    ir: &IRSignature,
    seen_classes: &mut HashSet<String>,
    seen_aliases: &mut HashSet<String>,
    prefix: &str,
) -> Vec<NestedTaggedUnionShape> {
    if !seen_classes.insert(class_name.to_string()) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some((_, class_details)) = ir.classes.get(class_name) {
        for (field_name, field_ty) in class_details.fields.iter() {
            let path = join_path(prefix, field_name);
            out.extend(collect_nested_tagged_unions_for_type(
                field_ty.as_ref(),
                ir,
                seen_classes,
                seen_aliases,
                &path,
            ));
        }
    }
    seen_classes.remove(class_name);
    out
}

fn collect_nested_tagged_unions_for_type(
    ty: &TypeNonStreaming,
    ir: &IRSignature,
    seen_classes: &mut HashSet<String>,
    seen_aliases: &mut HashSet<String>,
    path: &str,
) -> Vec<NestedTaggedUnionShape> {
    match ty {
        TypeNonStreaming::Union(union, _) => {
            let variant_types: Vec<&TypeNonStreaming> = match union.view() {
                UnionTypeViewGeneric::Null => return Vec::new(),
                UnionTypeViewGeneric::Optional(inner) => vec![inner],
                UnionTypeViewGeneric::OneOf(variants)
                | UnionTypeViewGeneric::OneOfOptional(variants) => variants.to_vec(),
            };

            if variant_types.len() > 1 {
                let mut variants = Vec::new();
                for variant_ty in &variant_types {
                    let Some(surface) = resolve_object_variant(variant_ty, ir, seen_aliases, None)
                    else {
                        return variant_types
                            .into_iter()
                            .flat_map(|inner| {
                                collect_nested_tagged_unions_for_type(
                                    inner,
                                    ir,
                                    seen_classes,
                                    seen_aliases,
                                    path,
                                )
                            })
                            .collect();
                    };
                    variants.push(surface);
                }
                if let Some(discriminator) = choose_discriminator(&variants) {
                    return vec![NestedTaggedUnionShape {
                        path: path.to_string(),
                        discriminator: discriminator.clone(),
                        variants: variants
                            .into_iter()
                            .map(|variant| TaggedVariantShape {
                                type_name: variant.type_name,
                                literal_value: variant
                                    .literal_fields
                                    .get(&discriminator)
                                    .cloned()
                                    .unwrap_or_default(),
                            })
                            .collect(),
                    }];
                }
            }

            variant_types
                .into_iter()
                .flat_map(|inner| {
                    collect_nested_tagged_unions_for_type(
                        inner,
                        ir,
                        seen_classes,
                        seen_aliases,
                        path,
                    )
                })
                .collect()
        }
        TypeNonStreaming::Class { name, .. } => {
            collect_nested_tagged_unions_for_class(name, ir, seen_classes, seen_aliases, path)
        }
        TypeNonStreaming::RecursiveTypeAlias { name, .. } => {
            if !seen_aliases.insert(name.clone()) {
                return Vec::new();
            }
            let out = ir
                .type_aliases
                .get(name)
                .map(|alias| {
                    collect_nested_tagged_unions_for_type(
                        alias.field_type.as_ref(),
                        ir,
                        seen_classes,
                        seen_aliases,
                        path,
                    )
                })
                .unwrap_or_default();
            seen_aliases.remove(name);
            out
        }
        TypeNonStreaming::List(inner, _) => collect_nested_tagged_unions_for_type(
            inner,
            ir,
            seen_classes,
            seen_aliases,
            &format!("{path}[]"),
        ),
        TypeNonStreaming::Map(_, value, _) => collect_nested_tagged_unions_for_type(
            value,
            ir,
            seen_classes,
            seen_aliases,
            &format!("{path}{{}}"),
        ),
        TypeNonStreaming::Primitive(..)
        | TypeNonStreaming::Enum { .. }
        | TypeNonStreaming::Literal(..)
        | TypeNonStreaming::Tuple(..)
        | TypeNonStreaming::Arrow(..)
        | TypeNonStreaming::Top(..) => Vec::new(),
    }
}

fn join_path(prefix: &str, field_name: &str) -> String {
    if prefix.is_empty() {
        field_name.to_string()
    } else {
        format!("{prefix}.{field_name}")
    }
}

fn literal_string(ty: &TypeNonStreaming) -> Option<String> {
    match ty {
        TypeNonStreaming::Literal(literal, _) => Some(match literal {
            LiteralValue::String(value) => value.clone(),
            LiteralValue::Int(value) => value.to_string(),
            LiteralValue::Bool(value) => value.to_string(),
        }),
        _ => None,
    }
}

/// Collect per-`SendStep` enum scalar reminders from the phase return union and compiled IR.
pub(crate) fn collect_send_enum_scalar_hints(
    variants: &[TaggedVariantShape],
    ir: &IRSignature,
) -> Vec<SendEnumScalarHint> {
    let mut out = Vec::new();
    let mut seen = HashSet::<(String, String)>::new();
    for variant in variants {
        if !variant.type_name.ends_with("SendStep") {
            continue;
        }
        collect_send_enum_hints_for_send_step(&variant.type_name, ir, &mut out, &mut seen);
    }
    out.sort_by(|a, b| {
        a.send_step_type
            .cmp(&b.send_step_type)
            .then_with(|| a.json_path.cmp(&b.json_path))
    });
    out
}

fn collect_send_enum_hints_for_send_step(
    send_step_type: &str,
    ir: &IRSignature,
    out: &mut Vec<SendEnumScalarHint>,
    seen: &mut HashSet<(String, String)>,
) {
    let Some((_, class_details)) = ir.classes.get(send_step_type) else {
        return;
    };
    let Some(input_ty) = class_details
        .fields
        .iter()
        .find(|(name, _)| name == "input")
        .map(|(_, ty)| ty.as_ref())
    else {
        return;
    };
    let mut seen_classes = HashSet::new();
    let mut seen_aliases = HashSet::new();
    let mut ctx = SendEnumWalkCtx {
        ir,
        send_step_type,
        path: "input".to_string(),
        seen_classes: &mut seen_classes,
        seen_aliases: &mut seen_aliases,
        out,
        seen,
    };
    collect_enum_scalar_hints_for_type(input_ty, &mut ctx);
}

struct SendEnumWalkCtx<'a> {
    ir: &'a IRSignature,
    send_step_type: &'a str,
    path: String,
    seen_classes: &'a mut HashSet<String>,
    seen_aliases: &'a mut HashSet<String>,
    out: &'a mut Vec<SendEnumScalarHint>,
    seen: &'a mut HashSet<(String, String)>,
}

fn collect_enum_scalar_hints_for_type(ty: &TypeNonStreaming, ctx: &mut SendEnumWalkCtx<'_>) {
    match ty {
        TypeNonStreaming::Enum { name, .. } => {
            let Some((_, enum_details)) = ctx.ir.enums.get(name) else {
                return;
            };
            let mut values: Vec<String> = enum_details
                .values
                .iter()
                .map(|value| format!("{value:?}"))
                .collect();
            values.sort();
            values.dedup();
            if values.is_empty() {
                return;
            }
            let key = (ctx.send_step_type.to_string(), ctx.path.to_string());
            if ctx.seen.insert(key) {
                ctx.out.push(SendEnumScalarHint {
                    send_step_type: ctx.send_step_type.to_string(),
                    json_path: ctx.path.to_string(),
                    enum_type: name.clone(),
                    values,
                });
            }
        }
        TypeNonStreaming::Class { name, .. } => {
            if !ctx.seen_classes.insert(name.clone()) {
                return;
            }
            if let Some((_, class_details)) = ctx.ir.classes.get(name) {
                for (field_name, field_ty) in class_details.fields.iter() {
                    let field_path = join_path(&ctx.path, field_name);
                    let prev_path = std::mem::replace(&mut ctx.path, field_path);
                    collect_enum_scalar_hints_for_type(field_ty.as_ref(), ctx);
                    ctx.path = prev_path;
                }
            }
            ctx.seen_classes.remove(name);
        }
        TypeNonStreaming::RecursiveTypeAlias { name, .. } => {
            if !ctx.seen_aliases.insert(name.clone()) {
                return;
            }
            if let Some(alias) = ctx.ir.type_aliases.get(name) {
                collect_enum_scalar_hints_for_type(alias.field_type.as_ref(), ctx);
            }
            ctx.seen_aliases.remove(name);
        }
        TypeNonStreaming::Union(union, _) => {
            let variant_types: Vec<&TypeNonStreaming> = match union.view() {
                UnionTypeViewGeneric::Null => return,
                UnionTypeViewGeneric::Optional(inner) => vec![inner],
                UnionTypeViewGeneric::OneOf(variants)
                | UnionTypeViewGeneric::OneOfOptional(variants) => variants.to_vec(),
            };
            for inner in variant_types {
                collect_enum_scalar_hints_for_type(inner, ctx);
            }
        }
        TypeNonStreaming::List(inner, _) => {
            collect_enum_scalar_hints_for_type(inner, ctx);
        }
        TypeNonStreaming::Map(_, value, _) => {
            collect_enum_scalar_hints_for_type(value, ctx);
        }
        TypeNonStreaming::Primitive(..)
        | TypeNonStreaming::Literal(..)
        | TypeNonStreaming::Tuple(..)
        | TypeNonStreaming::Arrow(..)
        | TypeNonStreaming::Top(..) => {}
    }
}

#[cfg(test)]
async fn shape_test_build_agent(prompt_body: &str) -> IRSignature {
    use std::fs;

    use tempfile::TempDir;
    let root = TempDir::new().expect("tempdir");
    let baml_src = root.path().join("baml_src");
    fs::create_dir_all(&baml_src).expect("mkdir baml_src");
    let prompt_path = baml_src.join("shape_agent_prompt.baml");
    let source = format!(
        "class Ready {{\n  kind \"ready\"\n  inferred_intent string\n}}\n\
             class Clarify {{\n  kind \"clarify\"\n  question string\n}}\n\
             class Meta {{\n  kind \"meta\"\n  reason string\n}}\n\
             enum ReplyMediaType {{\n  IMAGE\n}}\n\
             class TextPart {{\n  type \"text\"\n  text string\n}}\n\
             class DataPart {{\n  type \"data\"\n  raw string\n  media_type ReplyMediaType\n}}\n\
             class StructuredReply {{\n  parts (TextPart | DataPart)[]\n  citations string[]?\n}}\n\
             class Expression {{\n  left int\n  operation MathOperation\n  right int\n}}\n\
             enum MathOperation {{\n  Add @alias(\"+\")\n  Subtract @alias(\"-\")\n  Multiply @alias(\"*\")\n  Divide @alias(\"/\")\n}}\n\
             class CalculatorInput {{\n  expression Expression\n}}\n\
             class SupportCalculateSendStep {{\n  op \"Send\"\n  input CalculatorInput\n  citations string[]\n}}\n\
             class SupportCalculateFinishStep {{\n  op \"Finish\"\n}}\n\
             {prompt_body}\n\
             client DefaultClient {{\n  provider openai-generic\n  options {{\n    model \"gpt-4o-mini\"\n    base_url \"https://api.openai.com/v1\"\n    api_key env.OPENAI_API_KEY\n  }}\n}}\n"
    );
    fs::write(&prompt_path, source).expect("write prompt");
    let runtime = baml_runtime::BamlRuntime::from_directory(
        &baml_src,
        std::collections::HashMap::<String, String>::new(),
        internal_baml_core::feature_flags::FeatureFlags::default(),
    )
    .expect("runtime");
    IRSignature::new_from_ir(runtime.ir.as_ref()).expect("signature")
}

#[cfg(test)]
pub(crate) async fn build_minimal_test_ir() -> IRSignature {
    shape_test_build_agent(
        r##"function F(x: string) -> string {
  client DefaultClient
  prompt #"x"#
}"##,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn build_calculator_test_ir() -> IRSignature {
    shape_test_build_agent(
        r##"function CalcStep(input: string) -> SupportCalculateSendStep | SupportCalculateFinishStep {
  client DefaultClient
  prompt #"Step."#
}"##,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detects_root_tagged_union() {
        let ir = shape_test_build_agent(
            r##"function ShapeAgent(input: string) -> Ready | Clarify | Meta {
  client DefaultClient
  prompt #"Classify."#
}"##,
        )
        .await;
        let shape = analyze_return_shape(ir.functions["ShapeAgent"].output.as_ref(), &ir);
        assert!(matches!(
            shape,
            ReturnShape::TaggedUnion(TaggedUnionShape {
                discriminator,
                ..
            }) if discriminator == "kind"
        ));
    }

    #[tokio::test]
    async fn collect_send_enum_scalar_hints_finds_math_operation_path() {
        let ir = build_calculator_test_ir().await;
        let hints = collect_send_enum_scalar_hints(
            &[
                TaggedVariantShape {
                    type_name: "SupportCalculateSendStep".to_string(),
                    literal_value: "Send".to_string(),
                },
                TaggedVariantShape {
                    type_name: "SupportCalculateFinishStep".to_string(),
                    literal_value: "Finish".to_string(),
                },
            ],
            &ir,
        );
        assert_eq!(hints.len(), 1, "hints: {hints:?}");
        assert_eq!(hints[0].send_step_type, "SupportCalculateSendStep");
        assert_eq!(hints[0].json_path, "input.expression.operation");
        assert_eq!(hints[0].enum_type, "MathOperation");
        assert!(hints[0].values.iter().any(|v| v.contains("Add")));
    }

    #[tokio::test]
    async fn detects_nested_tagged_union_inside_single_object() {
        let ir = shape_test_build_agent(
            r##"function ShapeAgent(input: string) -> StructuredReply {
  client DefaultClient
  prompt #"Reply."#
}"##,
        )
        .await;
        let shape = analyze_return_shape(ir.functions["ShapeAgent"].output.as_ref(), &ir);
        match shape {
            ReturnShape::SingleObject(single) => {
                assert_eq!(single.type_name, "StructuredReply");
                assert_eq!(single.nested_tagged_unions.len(), 1);
                assert_eq!(single.nested_tagged_unions[0].path, "parts[]");
                assert_eq!(single.nested_tagged_unions[0].discriminator, "type");
            }
            other => panic!("expected single object, got {other:?}"),
        }
    }
}

//! Print `TypeIR` / union shapes as BAML type text for generated function signatures.
//!
//! Shared between session-plan codegen and any other IR → BAML surface that must walk unions.

use baml_types::ir_type::{TypeGeneric, UnionTypeViewGeneric};

/// Render a single IR input type as BAML source text.
pub(crate) fn type_ir_to_baml(ty: &baml_types::TypeIR) -> String {
    match ty {
        TypeGeneric::Primitive(tv, _) => tv.basename().to_string(),
        TypeGeneric::Class { name, .. } | TypeGeneric::Enum { name, .. } => name.clone(),
        TypeGeneric::RecursiveTypeAlias { name, .. } => name.clone(),
        TypeGeneric::Union(u, _) => match u.view() {
            UnionTypeViewGeneric::Optional(inner) => format!("{}?", type_ir_to_baml(inner)),
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => {
                let parts: Vec<String> = variants.iter().map(|v| type_ir_to_baml(v)).collect();
                format!("({})", parts.join(" | "))
            }
            _ => "string".to_string(),
        },
        TypeGeneric::List(item, _) => format!("{}[]", type_ir_to_baml(item)),
        TypeGeneric::Literal(lv, _) => {
            use baml_types::LiteralValue;
            match lv {
                LiteralValue::String(s) => format!("\"{s}\""),
                LiteralValue::Int(i) => i.to_string(),
                LiteralValue::Bool(b) => b.to_string(),
            }
        }
        _ => "string".to_string(),
    }
}

/// Collect every named class/enum/alias in a union tree (including non-session-plan members).
pub(crate) fn collect_union_type_names<T>(ty: &TypeGeneric<T>) -> Vec<String>
where
    T: Clone + std::fmt::Debug,
{
    match ty {
        TypeGeneric::Class { name, .. }
        | TypeGeneric::Enum { name, .. }
        | TypeGeneric::RecursiveTypeAlias { name, .. } => {
            vec![name.clone()]
        }
        TypeGeneric::Union(u, _) => match u.view() {
            UnionTypeViewGeneric::Optional(inner) => collect_union_type_names(inner),
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => variants
                .iter()
                .flat_map(|v| collect_union_type_names(v))
                .collect(),
            _ => vec![],
        },
        _ => vec![],
    }
}

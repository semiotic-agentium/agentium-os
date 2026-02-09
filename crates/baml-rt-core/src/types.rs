//! Type definitions for BAML runtime integration

use serde::{Deserialize, Serialize};

/// Represents a BAML function signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSignature {
    pub name: String,
    pub input_types: Vec<BamlType>,
    pub output_type: BamlType,
}

/// Represents a BAML type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BamlType {
    String,
    Int,
    Float,
    Bool,
    List(Box<BamlType>),
    Map(Box<BamlType>, Box<BamlType>), // key type, value type
    Object(Vec<ObjectField>),
    Optional(Box<BamlType>),
    // TODO: Add more types as needed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectField {
    pub name: String,
    pub ty: BamlType,
}

//! Sealed-trait module. The `Sealed` trait is intentionally non-public so that
//! external crates cannot grow new `GraphEvent`, `EdgeWitness`,
//! `AllowedPrimaryEdge`, `NodeLabelTy`, `FilterKey`, etc. impls. The closed
//! universe is what makes the metamodel actually enforceable.

pub trait Sealed {}

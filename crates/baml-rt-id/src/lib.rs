//! Identity semantics and enforced constructors.
//!
//! This crate provides the construction tokens and semantic traits.
//! ID types remain in their respective crates and accept only these
//! construction tokens at their public boundaries.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdConstruction {
    External,
    TemporalCounter { prefix: &'static str },
    MonotonicCounter { prefix: &'static str },
    UuidV4,
    Derived,
    Constant,
}

/// Marker traits for enforced construction forms.
pub trait ExternalConstructible {}
pub trait DerivedConstructible {}
pub trait ConstantConstructible {}
pub trait TemporalConstructible {}
pub trait MonotonicConstructible {}
pub trait UuidConstructible {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvKind {
    Entity,
    Activity,
    Agent,
}

pub trait ProvIdSemantics {
    const KIND: ProvKind;
}

pub trait ProvEntitySemantics: ProvIdSemantics {}
pub trait ProvActivitySemantics: ProvIdSemantics {}
pub trait ProvAgentSemantics: ProvIdSemantics {}

pub trait ProvDerivedEntitySemantics: ProvEntitySemantics + DerivedConstructible {}
pub trait ProvDerivedActivitySemantics: ProvActivitySemantics + DerivedConstructible {}
pub trait ProvDerivedAgentSemantics: ProvAgentSemantics + DerivedConstructible {}
pub trait ProvConstantEntitySemantics: ProvEntitySemantics + ConstantConstructible {}
pub trait ProvConstantActivitySemantics: ProvActivitySemantics + ConstantConstructible {}
pub trait ProvConstantAgentSemantics: ProvAgentSemantics + ConstantConstructible {}

/// Trait binding provenance semantics to a stable vocabulary type tag.
pub trait ProvVocabularyType: ProvIdSemantics {
    const VOCAB_TYPE: &'static str;
}

/// Derived ID template with a typed input contract.
pub trait ProvDerivedIdTemplate: ProvIdSemantics + DerivedConstructible {
    type Input<'a>;
    fn build<'a>(input: Self::Input<'a>) -> DerivedId;
}

/// Constant ID template with a fixed identity.
pub trait ProvConstantIdTemplate: ProvIdSemantics + ConstantConstructible {
    fn build() -> ConstantId;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalId(String);

impl ExternalId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstantId(&'static str);

impl ConstantId {
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedId(String);

impl DerivedId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn from_parts<'a>(prefix: &'static str, parts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut id = String::from(prefix);
        for part in parts {
            id.push(':');
            id.push_str(part);
        }
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemporalId {
    prefix: &'static str,
    millis: u64,
    counter: u64,
}

impl TemporalId {
    pub const fn new(prefix: &'static str, millis: u64, counter: u64) -> Self {
        Self {
            prefix,
            millis,
            counter,
        }
    }

    pub fn into_string(self) -> String {
        format!("{}-{}-{}", self.prefix, self.millis, self.counter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonotonicId {
    prefix: &'static str,
    counter: u64,
}

impl MonotonicId {
    pub const fn new(prefix: &'static str, counter: u64) -> Self {
        Self { prefix, counter }
    }

    pub fn into_string(self) -> String {
        format!("{}-{}", self.prefix, self.counter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UuidId(uuid::Uuid);

impl UuidId {
    pub fn new(id: uuid::Uuid) -> Self {
        Self(id)
    }

    pub fn parse_str(raw: &str) -> Result<Self, uuid::Error> {
        uuid::Uuid::parse_str(raw).map(Self)
    }

    pub fn into_uuid(self) -> uuid::Uuid {
        self.0
    }
}

impl std::fmt::Display for UuidId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_tools::tools::ToolAccess;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Tier {
    Read = 0,
    Routine = 1,
    Mutating = 2,
    Irreversible = 3,
}

impl Tier {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Read,
            1 => Self::Routine,
            2 => Self::Mutating,
            3 => Self::Irreversible,
            _ => Self::Mutating,
        }
    }
}

/// Declared metadata for tier classification.
#[derive(Debug, Clone)]
pub struct ToolTierMeta {
    pub access_level: ToolAccess,
    pub tags: Vec<String>,
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
    pub is_delegation: bool,
}

impl Default for ToolTierMeta {
    fn default() -> Self {
        Self {
            access_level: ToolAccess::Write,
            tags: vec![],
            read_only_hint: None,
            destructive_hint: None,
            is_delegation: false,
        }
    }
}

pub fn classify_tier(meta: &ToolTierMeta) -> Tier {
    for tag in &meta.tags {
        if let Some(rest) = tag.strip_prefix("semiotic:tier=")
            && let Ok(n) = rest.parse::<u8>()
        {
            return Tier::from_u8(n);
        }
    }
    if meta.is_delegation {
        return Tier::Mutating;
    }
    if meta.destructive_hint == Some(true) {
        return Tier::Irreversible;
    }
    match meta.access_level {
        ToolAccess::Read => Tier::Read,
        ToolAccess::Write => Tier::Mutating,
        ToolAccess::Delete => Tier::Irreversible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_is_tier_0() {
        assert_eq!(
            classify_tier(&ToolTierMeta {
                access_level: ToolAccess::Read,
                ..Default::default()
            }),
            Tier::Read
        );
    }

    #[test]
    fn tag_override() {
        assert_eq!(
            classify_tier(&ToolTierMeta {
                access_level: ToolAccess::Delete,
                tags: vec!["semiotic:tier=1".into()],
                ..Default::default()
            }),
            Tier::Routine
        );
    }
}

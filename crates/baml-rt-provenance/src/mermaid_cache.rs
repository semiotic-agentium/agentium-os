// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Simple cache for Mermaid sequence output. When the cache is lost (invalidated),
//! restore from the store interface by re-exporting; debounce is just mark-invalid on write.

use std::{collections::HashMap, fmt, sync::RwLock};

/// Cache: context_id -> rendered Mermaid. On invalidate(ctx) the entry is removed so the next
/// get triggers re-export from the store (restore from interface).
#[derive(Default)]
pub struct MermaidCache {
    entries: RwLock<HashMap<String, String>>,
}

impl fmt::Debug for MermaidCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.entries.read().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("MermaidCache")
            .field("entries", &len)
            .finish()
    }
}

impl MermaidCache {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// Get cached Mermaid for a context if present and not invalidated.
    pub fn get(&self, context_id: &str) -> Option<String> {
        self.entries.read().ok()?.get(context_id).cloned()
    }

    /// Store rendered Mermaid for a context (after re-export from store).
    pub fn insert(&self, context_id: &str, mermaid: String) {
        let _ = self
            .entries
            .write()
            .map(|mut g| g.insert(context_id.to_string(), mermaid));
    }

    /// Mark a context as invalid; next get will return None and caller restores from store.
    pub fn invalidate(&self, context_id: &str) {
        let _ = self.entries.write().map(|mut g| g.remove(context_id));
    }
}

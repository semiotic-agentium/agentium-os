//! Pluggable predicate that decides whether a parsed function declaration should have its
//! authored `prompt #"..."#` rewritten by the universal compositor.
//!
//! Centralising this as a trait keeps the rewrite pipeline agnostic about *why* a function is
//! excluded — tests can inject a stub policy, and product code can mix in extra rules (e.g.
//! per-package skip lists) without touching the scanner.

use std::collections::HashSet;

use crate::builder::baml_gen::CATALOG_FUNCTION_NAME;

/// Decides which authored functions get the universal canonical prompt skeleton applied.
pub trait PromptRewritePolicy {
    fn should_rewrite_prompt(&self, fn_name: &str) -> bool;
}

/// Default policy used by the build pipeline. Excludes:
/// - the synthetic catalog function (its prompt is exactly `{{ ctx.output_format }}` so the
///   renderer can produce the catalog text without recursing on itself);
/// - session-plan parents (return type ends with `*SessionPlan`) — their bodies are inlined
///   into generated phase executors that already prepend the canonical prefix;
/// - unified-primary roots — same reason as session-plan parents.
pub struct DefaultPromptRewritePolicy<'a> {
    pub session_plan_parent_names: &'a HashSet<String>,
    pub unified_primary_root_names: &'a HashSet<String>,
}

impl PromptRewritePolicy for DefaultPromptRewritePolicy<'_> {
    fn should_rewrite_prompt(&self, fn_name: &str) -> bool {
        if fn_name == CATALOG_FUNCTION_NAME {
            return false;
        }
        if self.session_plan_parent_names.contains(fn_name) {
            return false;
        }
        if self.unified_primary_root_names.contains(fn_name) {
            return false;
        }
        true
    }
}

#[cfg(test)]
pub(super) struct AllowAllPolicy;
#[cfg(test)]
impl PromptRewritePolicy for AllowAllPolicy {
    fn should_rewrite_prompt(&self, fn_name: &str) -> bool {
        fn_name != CATALOG_FUNCTION_NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_excludes_catalog() {
        let session = HashSet::new();
        let unified = HashSet::new();
        let policy = DefaultPromptRewritePolicy {
            session_plan_parent_names: &session,
            unified_primary_root_names: &unified,
        };
        assert!(!policy.should_rewrite_prompt(CATALOG_FUNCTION_NAME));
        assert!(policy.should_rewrite_prompt("FormatCapabilities"));
    }

    #[test]
    fn default_excludes_session_plan_parents() {
        let mut session = HashSet::new();
        session.insert("GetDiscoverAgentsPlan".to_string());
        let unified = HashSet::new();
        let policy = DefaultPromptRewritePolicy {
            session_plan_parent_names: &session,
            unified_primary_root_names: &unified,
        };
        assert!(!policy.should_rewrite_prompt("GetDiscoverAgentsPlan"));
        assert!(policy.should_rewrite_prompt("MakeStructuredPlan"));
    }

    #[test]
    fn default_excludes_unified_primary_roots() {
        let session = HashSet::new();
        let mut unified = HashSet::new();
        unified.insert("RouteIntent".to_string());
        let policy = DefaultPromptRewritePolicy {
            session_plan_parent_names: &session,
            unified_primary_root_names: &unified,
        };
        assert!(!policy.should_rewrite_prompt("RouteIntent"));
        assert!(policy.should_rewrite_prompt("ClassifyTurn"));
    }
}

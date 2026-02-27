//! JS wrapper and prelude code generation.
//!
//! Invocation context is host-only (no token/context prelude in JS). Natives resolve
//! scope from the active context stack.

use baml_rt_core::Result;

/// No prelude: invocation context is resolved on the host from the active context stack.
/// JS never receives tokens or context ids.
#[allow(dead_code)] // reserved for tests and alternative scope strategies
pub(crate) fn build_scope_prelude_empty() -> Result<String> {
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_scope_prelude_empty_returns_empty() {
        assert_eq!(build_scope_prelude_empty().unwrap(), "");
    }
}

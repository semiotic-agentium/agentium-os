//! Transport-agnostic secret resolution + identity hashing for MCP.
//!
//! The MCP runtime (stdio today, Streamable HTTP in PR4) consumes the
//! canonical `SecretSpec` shape from `mcp_config`. This module supplies two
//! pieces of plumbing both transports share:
//!
//!  1. [`resolve_secret_specs`] — resolves a list of specs against a caller-
//!     supplied lookup closure. Fails fast on the first missing value
//!     **before any I/O the caller intends to perform with the result** (in
//!     particular: before opening an HTTP connection). The error never
//!     carries the resolved value or any value-derived material.
//!
//!  2. [`compute_secret_identity_hash`] — stable hash over a set of specs'
//!     `(identity_token)` tuples, with no dependency on resolved values.
//!     Used by HTTP pool keys so credential *rotation* (new version) and
//!     source rebind invalidate pools without leaking value-derived bits
//!     into the key.
//!
//! Stdio's existing value-hash pool keying (in `baml-rt-mcp::resolver`)
//! stays untouched in this PR; PR4 unifies on top of these primitives.

use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::mcp_config::{SecretSource, SecretSpec};

/// Error returned by [`resolve_secret_specs`].
///
/// The variant intentionally identifies the **source** (e.g. `env:TOKEN`) of
/// the missing value, never the resolved value. Callers may log this error
/// without secret-redaction wrapping.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecretResolutionError {
    #[error("missing secret value for `{id}` (source `{source_identity}`)")]
    Missing {
        id: String,
        source_identity: String,
    },
}

/// Resolved secret value paired with the spec it satisfied. The value is
/// `String` rather than a `SecretString` newtype because the existing
/// stdio child-process env path is already a `BTreeMap<String, String>`;
/// callers should keep the lifetime short and avoid logging.
#[derive(Debug, Clone)]
pub struct ResolvedSecret {
    pub spec: SecretSpec,
    pub value: String,
}

/// Resolve all specs through `lookup`, fail-closed on the first missing.
///
/// `lookup(&SecretSource) -> Option<String>` lets callers plug stdlib env,
/// a `FnoxFileSecretResolver`, or test stubs without forcing a trait
/// import. Phase 2 only emits `SecretSource::Env`, so most callers will
/// only inspect the env-name arm.
///
/// Returns an ordered map keyed by `SecretSpec::id` so caller injection
/// logic stays deterministic.
pub fn resolve_secret_specs<F>(
    specs: &[SecretSpec],
    mut lookup: F,
) -> Result<BTreeMap<String, ResolvedSecret>, SecretResolutionError>
where
    F: FnMut(&SecretSource) -> Option<String>,
{
    let mut out = BTreeMap::new();
    for spec in specs {
        let Some(value) = lookup(&spec.source) else {
            return Err(SecretResolutionError::Missing {
                id: spec.id.clone(),
                source_identity: spec.source.identity(),
            });
        };
        out.insert(
            spec.id.clone(),
            ResolvedSecret {
                spec: spec.clone(),
                value,
            },
        );
    }
    Ok(out)
}

/// Stable identity hash over `(spec.id, spec.identity_token())` pairs,
/// length-prefixed so embedded `=`/`:`/`\0` in names cannot collide pool
/// keys across tenants. Resolved values are *never* mixed in.
///
/// Empty input yields a constant sentinel so callers don't need a
/// separate branch.
pub fn compute_secret_identity_hash(specs: &[SecretSpec]) -> String {
    if specs.is_empty() {
        return "sha256:empty".to_string();
    }
    let mut sorted: Vec<&SecretSpec> = specs.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let mut hasher = Sha256::new();
    for spec in sorted {
        let id = spec.id.as_bytes();
        let identity = spec.identity_token();
        let identity = identity.as_bytes();
        hasher.update((id.len() as u64).to_be_bytes());
        hasher.update(id);
        hasher.update((identity.len() as u64).to_be_bytes());
        hasher.update(identity);
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Convenience env-source lookup helper: forwards to `std::env::var`.
/// Returns `None` for both "not present" and "non-UTF-8".
pub fn env_source_lookup(source: &SecretSource) -> Option<String> {
    match source {
        SecretSource::Env { name } => std::env::var(name).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_config::SecretInjection;

    fn env_spec(id: &str, env_name: &str) -> SecretSpec {
        SecretSpec {
            id: id.into(),
            source: SecretSource::Env {
                name: env_name.into(),
            },
            inject: SecretInjection::Env {
                name: env_name.into(),
            },
            version: None,
        }
    }

    #[test]
    fn resolve_returns_all_values_when_present() {
        let specs = vec![env_spec("a", "A"), env_spec("b", "B")];
        let mut map = std::collections::HashMap::new();
        map.insert("A".to_string(), "alpha".to_string());
        map.insert("B".to_string(), "beta".to_string());
        let resolved = resolve_secret_specs(&specs, |s| match s {
            SecretSource::Env { name } => map.get(name).cloned(),
        })
        .expect("resolve");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved["a"].value, "alpha");
        assert_eq!(resolved["b"].value, "beta");
    }

    #[test]
    fn resolve_fails_fast_on_first_missing_and_does_not_leak_value() {
        let specs = vec![env_spec("a", "A"), env_spec("b", "MISSING")];
        let mut calls = 0;
        let err = resolve_secret_specs(&specs, |s| {
            calls += 1;
            match s {
                SecretSource::Env { name } if name == "A" => Some("supersecret".to_string()),
                _ => None,
            }
        })
        .expect_err("must fail");
        let msg = err.to_string();
        assert!(msg.contains("env:MISSING"), "msg: {msg}");
        assert!(msg.contains("`b`"), "msg: {msg}");
        assert!(!msg.contains("supersecret"), "value leaked: {msg}");
        // Fail-fast: lookup called for first spec, second yielded None.
        assert_eq!(calls, 2);
    }

    #[test]
    fn identity_hash_is_stable_and_independent_of_value() {
        let specs = vec![env_spec("a", "A"), env_spec("b", "B")];
        let h1 = compute_secret_identity_hash(&specs);
        let h2 = compute_secret_identity_hash(&specs);
        assert_eq!(h1, h2);
    }

    #[test]
    fn identity_hash_changes_on_source_rebind() {
        let specs_a = vec![env_spec("a", "A")];
        let specs_b = vec![env_spec("a", "B")];
        assert_ne!(
            compute_secret_identity_hash(&specs_a),
            compute_secret_identity_hash(&specs_b)
        );
    }

    #[test]
    fn identity_hash_changes_on_version_bump() {
        let mut spec = env_spec("a", "A");
        let h_v0 = compute_secret_identity_hash(std::slice::from_ref(&spec));
        spec.version = Some("v2".into());
        let h_v2 = compute_secret_identity_hash(std::slice::from_ref(&spec));
        assert_ne!(h_v0, h_v2);
    }

    #[test]
    fn identity_hash_empty_is_constant() {
        assert_eq!(compute_secret_identity_hash(&[]), "sha256:empty");
    }
}

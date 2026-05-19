//! Transport-agnostic secret resolution + fingerprinting for MCP.
//!
//! The MCP runtime consumes the canonical `SecretSpec` shape from
//! `mcp_config`. This module supplies two pieces of plumbing both transports
//! share:
//!
//!  1. [`resolve_secret_specs`] — resolves a list of specs against a caller-
//!     supplied lookup closure. Fails fast on the first missing value
//!     **before any I/O the caller intends to perform with the result** (in
//!     particular: before opening an HTTP connection). The error never
//!     carries the resolved value or any value-derived material.
//!
//!  2. [`compute_secret_fingerprint`] — keyed, value-derived fingerprint over
//!     resolved secrets. Pool keys use this redacted newtype so secret rotation
//!     forces fresh stdio children and HTTP connections without logging raw
//!     value-derived bytes.

use std::{collections::BTreeMap, fmt, sync::OnceLock};

use blake3::Hasher;
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::mcp_config::{SecretSource, SecretSpec};

/// Error returned by [`resolve_secret_specs`].
///
/// The variant intentionally identifies the **source** (e.g. `env:TOKEN`) of
/// the missing value, never the resolved value. Callers may log this error
/// without secret-redaction wrapping.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecretResolutionError {
    #[error("missing secret value for `{id}` (source `{source_identity}`)")]
    Missing { id: String, source_identity: String },
}

/// Secret material resolved for MCP transport injection.
///
/// Formatting is always redacted. Contents are zeroized on drop via
/// `ZeroizeOnDrop`; callers must still avoid cloning or logging borrowed
/// plaintext.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct McpSecretValue(String);

impl McpSecretValue {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl From<String> for McpSecretValue {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for McpSecretValue {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Debug for McpSecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("McpSecretValue(<redacted>)")
    }
}

impl PartialEq<&str> for McpSecretValue {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Resolved secret value paired with the spec it satisfied.
#[derive(Debug, Clone)]
pub struct ResolvedSecret {
    pub spec: SecretSpec,
    pub value: McpSecretValue,
}

/// Value-derived pool-key component for resolved MCP secrets.
///
/// The bytes are private and all formatting redacts them. Do not add a
/// plaintext `Serialize` implementation; logs/metrics should use
/// non-secret identity labels (`source + id + version`) instead.
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct SecretFingerprint([u8; 32]);

impl fmt::Debug for SecretFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretFingerprint(<redacted>)")
    }
}

impl fmt::Display for SecretFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretFingerprint(<redacted>)")
    }
}

static RUNTIME_FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

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
                value: value.into(),
            },
        );
    }
    Ok(out)
}

/// Keyed, value-derived fingerprint over resolved secret material.
///
/// Canonical material is length-prefixed and sorted by `spec.id`:
/// `spec.id | source.identity() | version | inject_kind_tag | inject_param |
/// value_bytes`. Empty input is a keyed hash over a domain separator, not a
/// string sentinel, so `PoolKey` type stays uniform.
pub fn compute_secret_fingerprint(secrets: &[ResolvedSecret]) -> SecretFingerprint {
    let key = runtime_fingerprint_key();
    let mut sorted: Vec<&ResolvedSecret> = secrets.iter().collect();
    sorted.sort_by(|a, b| a.spec.id.cmp(&b.spec.id));

    let mut hasher = Hasher::new_keyed(key);
    write_len_prefixed(&mut hasher, b"baml-rt-mcp-secret-fingerprint-v1");
    for secret in sorted {
        write_len_prefixed(&mut hasher, secret.spec.id.as_bytes());
        write_secret_source_identity(&mut hasher, &secret.spec.source);
        write_len_prefixed(
            &mut hasher,
            secret.spec.version.as_deref().unwrap_or("").as_bytes(),
        );
        let (kind, param) = injection_fingerprint_parts(&secret.spec.inject);
        write_len_prefixed(&mut hasher, kind.as_bytes());
        write_len_prefixed(&mut hasher, param.as_bytes());
        write_len_prefixed(&mut hasher, secret.value.expose_secret().as_bytes());
    }
    SecretFingerprint(*hasher.finalize().as_bytes())
}

fn runtime_fingerprint_key() -> &'static [u8; 32] {
    RUNTIME_FINGERPRINT_KEY.get_or_init(|| {
        let mut key = [0_u8; 32];
        if let Err(err) = getrandom::fill(&mut key) {
            panic!("OS random source unavailable while seeding MCP secret fingerprint key: {err}");
        }
        key
    })
}

fn write_secret_source_identity(hasher: &mut Hasher, source: &SecretSource) {
    match source {
        SecretSource::Env { name } => {
            write_len_prefixed_parts(hasher, &[b"env:", name.as_bytes()]);
        }
    }
}

fn write_len_prefixed(hasher: &mut Hasher, bytes: &[u8]) {
    write_len_prefixed_parts(hasher, &[bytes]);
}

fn write_len_prefixed_parts(hasher: &mut Hasher, parts: &[&[u8]]) {
    let len = parts.iter().map(|part| part.len() as u64).sum::<u64>();
    hasher.update(&len.to_be_bytes());
    for part in parts {
        hasher.update(part);
    }
}

fn injection_fingerprint_parts(
    inject: &crate::mcp_config::SecretInjection,
) -> (&'static str, String) {
    match inject {
        crate::mcp_config::SecretInjection::Env { name } => ("env", name.clone()),
        crate::mcp_config::SecretInjection::HttpHeader { name } => ("http_header", name.clone()),
        crate::mcp_config::SecretInjection::HttpAuthorizationBearer => {
            ("http_authorization_bearer", "authorization".to_string())
        }
        crate::mcp_config::SecretInjection::HttpBasicPassword { username } => {
            ("http_basic_password", username.clone())
        }
    }
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

    fn bearer_spec(id: &str, env_name: &str) -> SecretSpec {
        SecretSpec {
            id: id.into(),
            source: SecretSource::Env {
                name: env_name.into(),
            },
            inject: SecretInjection::HttpAuthorizationBearer,
            version: None,
        }
    }

    fn resolved(spec: SecretSpec, value: &str) -> ResolvedSecret {
        ResolvedSecret {
            spec,
            value: value.into(),
        }
    }

    #[test]
    fn fingerprint_same_resolved_secrets_match() {
        let secrets = vec![
            resolved(env_spec("a", "A"), "alpha"),
            resolved(env_spec("b", "B"), "beta"),
        ];
        let h1 = compute_secret_fingerprint(&secrets);
        let h2 = compute_secret_fingerprint(&secrets);
        assert_eq!(h1, h2);
    }

    #[test]
    fn fingerprint_order_independent() {
        let a = resolved(env_spec("a", "A"), "alpha");
        let b = resolved(env_spec("b", "B"), "beta");
        let h1 = compute_secret_fingerprint(&[a.clone(), b.clone()]);
        let h2 = compute_secret_fingerprint(&[b, a]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn fingerprint_changes_on_stdio_secret_value_change() {
        let h1 = compute_secret_fingerprint(&[resolved(env_spec("a", "A"), "alpha")]);
        let h2 = compute_secret_fingerprint(&[resolved(env_spec("a", "A"), "bravo")]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn fingerprint_changes_on_http_secret_value_change() {
        let h1 =
            compute_secret_fingerprint(&[resolved(bearer_spec("auth.bearer", "TOKEN"), "alpha")]);
        let h2 =
            compute_secret_fingerprint(&[resolved(bearer_spec("auth.bearer", "TOKEN"), "bravo")]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn fingerprint_changes_when_spec_added() {
        let h1 = compute_secret_fingerprint(&[resolved(env_spec("a", "A"), "alpha")]);
        let h2 = compute_secret_fingerprint(&[
            resolved(env_spec("a", "A"), "alpha"),
            resolved(env_spec("b", "B"), "beta"),
        ]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn fingerprint_changes_on_source_rebind() {
        let h1 = compute_secret_fingerprint(&[resolved(env_spec("a", "A"), "alpha")]);
        let h2 = compute_secret_fingerprint(&[resolved(env_spec("a", "B"), "alpha")]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn fingerprint_changes_on_version_bump() {
        let mut spec = env_spec("a", "A");
        let h_v0 = compute_secret_fingerprint(&[resolved(spec.clone(), "alpha")]);
        spec.version = Some("v2".into());
        let h_v2 = compute_secret_fingerprint(&[resolved(spec, "alpha")]);
        assert_ne!(h_v0, h_v2);
    }

    #[test]
    fn fingerprint_debug_and_display_are_redacted() {
        let fp = compute_secret_fingerprint(&[resolved(env_spec("a", "A"), "supersecret")]);
        assert_eq!(format!("{fp:?}"), "SecretFingerprint(<redacted>)");
        assert_eq!(format!("{fp}"), "SecretFingerprint(<redacted>)");
        assert!(!format!("{fp:?}").contains("supersecret"));
    }
}

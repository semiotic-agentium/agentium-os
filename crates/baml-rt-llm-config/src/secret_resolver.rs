//! Secret resolution for LLM API keys.
//!
//! Production must use the **same secret mechanism as the configuration system** (e.g. vault,
//! fnox, or shared secret store). Do not use environment variables in production.
//! The runner **always** has a secret backend; the default is the **fnox** file provider
//! (fnox.toml with providers/secrets).
//!
//! **Type distinction:** [`StoreKey`] = key that exists in the store (fnox) with a value.
//! [`SecretRequestName`] = name of a secret request (what tools/LLM need). Linking maps
//! request → store key (M:N).

use std::{collections::HashMap, path::Path, sync::Arc};

use fnox::config::Config;
use serde::{Deserialize, Serialize};

/// Key that exists in the secret store (e.g. fnox) with a value. Used for cache keys in the
/// backend, `list_store_keys()`, and `resolve_from_store()`. Not to be confused with
/// secret request names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoreKey(String);

impl StoreKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into().trim().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for StoreKey {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl std::fmt::Display for StoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Name of a secret request (what a tool or LLM config asks for). Used in the overlay map,
/// `set`/`remove`, and in `SecretLinksState.links` / `unlinked`. Linking maps this to a [`StoreKey`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretRequestName(String);

impl SecretRequestName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into().trim().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SecretRequestName {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl std::fmt::Display for SecretRequestName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Normalized key used internally for placeholder→key conversion (same string as store key / request name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretName(String);

impl SecretName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into().trim().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SecretName {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for SecretName {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Resolved secret value (opaque). Never logged or serialized; use only at API boundaries.
#[derive(Clone)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<SecretValue> for String {
    fn from(v: SecretValue) -> String {
        v.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(***)")
    }
}

/// Strip a recognised secret-placeholder prefix (`vault:` or `env.`) from `value`,
/// returning the bare store-key. Returns `None` when no prefix matched. Trims
/// surrounding whitespace before checking.
pub fn strip_placeholder_prefix(value: &str) -> Option<&str> {
    let s = value.trim();
    s.strip_prefix("vault:").or_else(|| s.strip_prefix("env."))
}

/// Map placeholder to secret-store key: `"vault:KEY"` → `KEY`, `"env.VAR"` → `VAR` (for compat),
/// else trimmed value as-is.
pub fn placeholder_to_key(placeholder: &str) -> &str {
    strip_placeholder_prefix(placeholder).unwrap_or_else(|| placeholder.trim())
}

/// Resolves secret placeholders (e.g. `env.OPENROUTER_API_KEY` or vault keys) to actual values.
pub trait SecretResolver: Send + Sync {
    /// Resolve a placeholder to the secret value. Returns None if not found or not applicable.
    fn resolve(&self, placeholder: &str) -> Option<SecretValue>;

    /// Look up the value for this store key in the backend. Always a direct store lookup; no overlay or request-name indirection.
    fn resolve_from_store(&self, key: &StoreKey) -> Option<SecretValue> {
        self.resolve(key.as_str())
    }

    /// Keys in the store that have a value (for UI link dropdown). Only real store keys, never secret request names.
    fn list_store_keys(&self) -> Vec<StoreKey> {
        Vec::new()
    }
}

/// Allows runtime linking/unlinking of secrets (e.g. from the Settings UI).
/// The **model** (which request is linked to which store key, and which are unlinked) is persisted in config.
pub trait RuntimeSecretStore: Send + Sync {
    /// Set a secret for this request (overlay uses request name as key).
    fn set(&self, request: &SecretRequestName, value: SecretValue);
    /// Unlink the secret for this request. Idempotent.
    fn remove(&self, request: &SecretRequestName);
}

/// Whether fnox is the sole credential source or env-var fallback is permitted.
///
/// Determined from `BAML_FNOX_CONFIG`: when the env var is set (and non-empty),
/// fnox is the exclusive source; otherwise integration clients may fall back to
/// process environment variables for missing keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSourcePolicy {
    /// Fnox is the only secret source; integration clients must not consult
    /// process environment variables.
    FnoxOnly,
    /// Fnox first; on miss, integration clients may fall back to process env.
    FnoxWithEnvFallback,
}

impl SecretSourcePolicy {
    /// Determine the policy from a `BAML_FNOX_CONFIG` value (or its absence).
    /// Pure helper — exposed so tests can verify the rule without touching the
    /// process environment.
    pub fn from_env_value(baml_fnox_config: Option<&str>) -> Self {
        match baml_fnox_config {
            Some(v) if !v.trim().is_empty() => Self::FnoxOnly,
            _ => Self::FnoxWithEnvFallback,
        }
    }

    /// Read `BAML_FNOX_CONFIG` from the current process environment and map it
    /// to a policy.
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var("BAML_FNOX_CONFIG").ok().as_deref())
    }

    /// Whether fnox is the exclusive secret source (no env-var fallback).
    pub fn is_exclusive(self) -> bool {
        matches!(self, Self::FnoxOnly)
    }
}

/// Secret resolver backed by the **fnox** crate (fnox.toml). Loads config via fnox's discovery
/// (or `BAML_FNOX_CONFIG` path), resolves all secrets for the profile at construction time
/// using fnox's `resolve_secret`, and serves them synchronously.
/// Placeholders: `vault:KEY` → key `KEY`, `env.VAR_NAME` → key `VAR_NAME`; others → key as-is.
#[derive(Clone)]
pub struct FnoxFileSecretResolver {
    /// Pre-resolved store key → value from fnox.
    cache: Arc<HashMap<StoreKey, SecretValue>>,
    /// Secret-source policy, captured at construction so resolution does not
    /// re-read the environment.
    policy: SecretSourcePolicy,
}

impl std::fmt::Debug for FnoxFileSecretResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnoxFileSecretResolver")
            .field(
                "cache_keys",
                &self.cache.keys().map(StoreKey::as_str).collect::<Vec<_>>(),
            )
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl FnoxFileSecretResolver {
    /// Config path: `BAML_FNOX_CONFIG` env, or `"fnox.toml"` for fnox's recursive discovery.
    fn config_path() -> std::path::PathBuf {
        std::env::var("BAML_FNOX_CONFIG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("fnox.toml"))
    }

    /// Build the default resolver: load fnox config (from env or fnox.toml), resolve all secrets
    /// for the current profile, cache them. Missing/invalid config or resolution errors yield an
    /// empty cache (no secrets) but a valid resolver.
    pub fn default_path_resolver() -> Self {
        let path = Self::config_path();
        Self::from_path(Some(path.as_path()))
    }

    /// Build from an explicit config path. Uses fnox's `Config::load` for a single file, or
    /// pass `"fnox.toml"` to use fnox's recursive discovery via `Config::load_smart`.
    /// Missing file or resolution errors yield empty cache.
    /// When called from inside a tokio runtime, runs fnox resolution on a separate OS thread
    /// (which has no runtime) to avoid "Cannot start a runtime from within a runtime".
    pub fn from_path(path: Option<impl AsRef<Path>>) -> Self {
        let path_buf = path.as_ref().map(|p| p.as_ref().to_path_buf());
        let cache = path_buf
            .as_ref()
            .and_then(|path_buf| {
                if tokio::runtime::Handle::try_current().is_ok() {
                    let path_buf = path_buf.clone();
                    let handle = std::thread::spawn(move || Self::load_and_resolve(&path_buf));
                    handle.join().ok().and_then(|r| r.ok())
                } else {
                    Self::load_and_resolve(path_buf).ok()
                }
            })
            .unwrap_or_default();
        if path_buf.is_some() && cache.is_empty() {
            tracing::debug!(
                fnox_path = ?path_buf.as_ref().map(|p| p.display()),
                "FnoxFileSecretResolver: no secrets resolved (missing file, wrong profile, or resolution failed)"
            );
        }
        Self {
            cache: Arc::new(cache),
            policy: SecretSourcePolicy::from_env(),
        }
    }

    /// Load fnox Config and resolve all secrets for the default profile; return key→value map.
    /// Must be called from a thread that is NOT inside a tokio runtime (e.g. from spawn_blocking).
    fn load_and_resolve(
        path: &Path,
    ) -> Result<HashMap<StoreKey, SecretValue>, fnox::error::FnoxError> {
        let config = if path == Path::new("fnox.toml") {
            Config::load_smart(path)?
        } else {
            Config::load(path)?
        };
        let profile = Config::get_profile(None);
        let secrets = config.get_secrets(&profile)?;
        let age_key_file = config.age_key_file.as_deref();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| fnox::error::FnoxError::Config(e.to_string()))?;

        let mut cache = HashMap::new();
        for (key, secret_config) in secrets.iter() {
            match rt.block_on(fnox::secret_resolver::resolve_secret(
                &config,
                &profile,
                key,
                secret_config,
                age_key_file,
            )) {
                Ok(Some(value)) => {
                    cache.insert(StoreKey::new(key.as_str()), SecretValue::new(value));
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!(key = %key, error = %e, "fnox resolve_secret failed, skipping");
                }
            }
        }
        Ok(cache)
    }

    /// Whether fnox is the exclusive secret source. When true, integration
    /// clients must not fall back to process environment variables for
    /// credential resolution — all secrets come from fnox.toml.
    pub fn is_exclusive(&self) -> bool {
        self.policy.is_exclusive()
    }

    /// Resolve a credential by name from fnox, falling back to the process environment
    /// only when fnox is not the exclusive source (`BAML_FNOX_CONFIG` not set).
    /// Tries both `env.{name}` and `{name}` as fnox keys for compatibility with
    /// BAML placeholder conventions.
    pub fn resolve_or_env(&self, name: &str) -> Option<String> {
        self.resolve_or_env_with(name, self.is_exclusive(), |n| std::env::var(n).ok())
    }

    /// Core resolution logic with injectable exclusivity flag and env lookup.
    /// Exposed for testing without process-environment side effects.
    pub fn resolve_or_env_with(
        &self,
        name: &str,
        exclusive: bool,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let env_prefixed = format!("env.{name}");
        for key in [env_prefixed.as_str(), name] {
            if let Some(v) = self.resolve(key) {
                let t = v.as_str().trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        if !exclusive && let Some(k) = env_lookup(name) {
            let t = k.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        None
    }
}

impl SecretResolver for FnoxFileSecretResolver {
    fn resolve(&self, placeholder: &str) -> Option<SecretValue> {
        let key = StoreKey::from(placeholder_to_key(placeholder));
        self.cache.get(&key).cloned()
    }

    fn resolve_from_store(&self, key: &StoreKey) -> Option<SecretValue> {
        self.cache.get(key).cloned()
    }

    fn list_store_keys(&self) -> Vec<StoreKey> {
        self.cache.keys().cloned().collect()
    }
}

/// Resolver that checks an in-memory overlay first, then falls back to a backend.
/// Implements both [`SecretResolver`] and [`RuntimeSecretStore`]; use when the API can provision
/// secrets at runtime (e.g. Settings UI). The runner wraps its file/vault resolver in this.
/// Overlay key = secret request name; value = Some(linked) or None (unlinked). Absent = use backend.
#[derive(Clone)]
pub struct OverlaySecretResolver {
    backend: Arc<dyn SecretResolver>,
    overlay: Arc<std::sync::RwLock<HashMap<SecretRequestName, Option<SecretValue>>>>,
}

impl OverlaySecretResolver {
    pub fn new(backend: Arc<dyn SecretResolver>) -> Self {
        Self {
            backend,
            overlay: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }
}

impl SecretResolver for OverlaySecretResolver {
    fn resolve(&self, placeholder: &str) -> Option<SecretValue> {
        let request = SecretRequestName::from(placeholder_to_key(placeholder));
        let overlay_result = {
            let guard = match self.overlay.read() {
                Ok(g) => g,
                Err(e) => {
                    tracing::error!(error = %e, "overlay RwLock poisoned; falling back to backend");
                    return self.backend.resolve(placeholder);
                }
            };
            guard.get(&request).cloned()
        };
        match overlay_result {
            Some(Some(v)) => Some(v),
            Some(None) => None,
            None => self.backend.resolve(placeholder),
        }
    }

    fn resolve_from_store(&self, key: &StoreKey) -> Option<SecretValue> {
        self.backend.resolve_from_store(key)
    }

    fn list_store_keys(&self) -> Vec<StoreKey> {
        self.backend.list_store_keys()
    }
}

impl RuntimeSecretStore for OverlaySecretResolver {
    fn set(&self, request: &SecretRequestName, value: SecretValue) {
        match self.overlay.write() {
            Ok(mut g) => {
                g.insert(request.clone(), Some(value));
            }
            Err(e) => {
                tracing::error!(error = %e, "overlay RwLock poisoned; secret link not applied")
            }
        }
    }

    fn remove(&self, request: &SecretRequestName) {
        match self.overlay.write() {
            Ok(mut g) => {
                g.insert(request.clone(), None);
            }
            Err(e) => {
                tracing::error!(error = %e, "overlay RwLock poisoned; secret unlink not applied")
            }
        }
    }
}

/// Secret resolver that never resolves (always returns `None`).
/// For **tests only**; production must use a real backend (e.g. `FnoxFileSecretResolver`).
#[derive(Debug, Default, Clone)]
pub struct EmptySecretResolver;

impl SecretResolver for EmptySecretResolver {
    fn resolve(&self, _placeholder: &str) -> Option<SecretValue> {
        None
    }

    fn resolve_from_store(&self, _key: &StoreKey) -> Option<SecretValue> {
        None
    }

    fn list_store_keys(&self) -> Vec<StoreKey> {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Persistent secret link state (stored in config; applied at startup)
// ---------------------------------------------------------------------------

/// Internal config key under which secret link/unlink state is persisted (via config store's internal key-value mechanism, not a tool bundle).
pub const SECRET_LINKS_CONFIG_KEY: &str = "_secret_links";

/// Persisted model of which secret requests are linked to which store key, and which are unlinked.
/// Stored in config under [`SECRET_LINKS_CONFIG_KEY`]; the API writes this on link/unlink, the runner applies it at startup.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecretLinksState {
    /// Map secret request name → store key to resolve from (e.g. CLICKUP_API_KEY → OPENROUTER_API_KEY).
    #[serde(default)]
    pub links: HashMap<SecretRequestName, StoreKey>,
    /// Request names that are explicitly unlinked (resolution returns None until linked again).
    #[serde(default)]
    pub unlinked: Vec<SecretRequestName>,
}

/// Apply persisted secret link state to the overlay using the backend to resolve store keys.
/// Call at runner startup after creating the overlay so link/unlink state survives restarts.
pub fn apply_secret_links_state(
    state: &SecretLinksState,
    overlay: &dyn RuntimeSecretStore,
    backend: &dyn SecretResolver,
) {
    for (request, store_key) in &state.links {
        if let Some(value) = backend
            .resolve_from_store(store_key)
            .filter(|v| !v.as_str().trim().is_empty())
        {
            overlay.set(request, value);
        }
    }
    for request in &state.unlinked {
        overlay.remove(request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SecretSourcePolicy::from_env_value ──────────────────────────────

    #[test]
    fn fnox_only_when_set_nonempty() {
        assert_eq!(
            SecretSourcePolicy::from_env_value(Some("/config/fnox.toml")),
            SecretSourcePolicy::FnoxOnly,
        );
    }

    #[test]
    fn fallback_when_empty() {
        assert_eq!(
            SecretSourcePolicy::from_env_value(Some("")),
            SecretSourcePolicy::FnoxWithEnvFallback,
        );
    }

    #[test]
    fn fallback_when_whitespace() {
        assert_eq!(
            SecretSourcePolicy::from_env_value(Some("  ")),
            SecretSourcePolicy::FnoxWithEnvFallback,
        );
    }

    #[test]
    fn fallback_when_absent() {
        assert_eq!(
            SecretSourcePolicy::from_env_value(None),
            SecretSourcePolicy::FnoxWithEnvFallback,
        );
    }

    #[test]
    fn is_exclusive_only_for_fnox_only() {
        assert!(SecretSourcePolicy::FnoxOnly.is_exclusive());
        assert!(!SecretSourcePolicy::FnoxWithEnvFallback.is_exclusive());
    }

    // ── resolve_or_env_with ──────────────────────────────────────────────

    #[test]
    fn exclusive_blocks_env_fallback() {
        let resolver = FnoxFileSecretResolver::from_path(None::<&Path>);
        let result = resolver.resolve_or_env_with(
            "SOME_KEY",
            true, // exclusive
            |_| Some("env-value".to_string()),
        );
        assert!(result.is_none(), "exclusive mode must block env fallback");
    }

    #[test]
    fn nonexclusive_allows_env_fallback() {
        let resolver = FnoxFileSecretResolver::from_path(None::<&Path>);
        let result = resolver.resolve_or_env_with(
            "SOME_KEY",
            false, // not exclusive
            |_| Some("env-value".to_string()),
        );
        assert_eq!(result.as_deref(), Some("env-value"));
    }

    #[test]
    fn nonexclusive_skips_empty_env() {
        let resolver = FnoxFileSecretResolver::from_path(None::<&Path>);
        let result = resolver.resolve_or_env_with("SOME_KEY", false, |_| Some("  ".to_string()));
        assert!(
            result.is_none(),
            "whitespace-only env value should be skipped"
        );
    }

    // ── placeholder_to_key ───────────────────────────────────────────────

    #[test]
    fn placeholder_to_key_strips_vault_prefix() {
        assert_eq!(
            placeholder_to_key("vault:OPENROUTER_API_KEY"),
            "OPENROUTER_API_KEY"
        );
    }

    #[test]
    fn placeholder_to_key_strips_env_prefix() {
        assert_eq!(
            placeholder_to_key("env.OPENROUTER_API_KEY"),
            "OPENROUTER_API_KEY"
        );
    }

    #[test]
    fn placeholder_to_key_returns_unprefixed_as_is() {
        assert_eq!(
            placeholder_to_key("OPENROUTER_API_KEY"),
            "OPENROUTER_API_KEY"
        );
    }

    #[test]
    fn placeholder_to_key_trims_before_stripping() {
        assert_eq!(placeholder_to_key("  vault:KEY  "), "KEY");
        assert_eq!(placeholder_to_key("\tenv.KEY\n"), "KEY");
    }

    #[test]
    fn placeholder_to_key_only_strips_first_match() {
        assert_eq!(placeholder_to_key("vault:env.KEY"), "env.KEY");
    }

    // ── strip_placeholder_prefix ─────────────────────────────────────────

    #[test]
    fn strip_placeholder_prefix_returns_some_for_recognised_prefixes() {
        assert_eq!(strip_placeholder_prefix("vault:KEY"), Some("KEY"));
        assert_eq!(strip_placeholder_prefix("env.VAR"), Some("VAR"));
        assert_eq!(strip_placeholder_prefix("  vault:KEY  "), Some("KEY"));
        assert_eq!(strip_placeholder_prefix("\tenv.VAR\n"), Some("VAR"));
    }

    #[test]
    fn strip_placeholder_prefix_returns_none_for_plain_values() {
        assert_eq!(strip_placeholder_prefix("KEY"), None);
        assert_eq!(strip_placeholder_prefix("plain-value"), None);
        assert_eq!(strip_placeholder_prefix(""), None);
    }
}

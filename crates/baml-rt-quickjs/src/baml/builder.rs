//! [`super::BamlRuntimeManagerBuilder`]: optional dependency injection before runtime construction.

use std::{path::Path, sync::Arc};

use baml_rt_core::Result;
use baml_rt_llm_config::FnoxFileSecretResolver;

use super::BamlRuntimeManager;
use crate::{
    llm_client_registry::LlmSecretResolver, llm_resolver_adapter::SecretResolverToLlmAdapter,
};

/// Linearly-typed builder for [`BamlRuntimeManager`]. Inject optional dependencies (e.g. LLM
/// secret resolver from fnox) then call [`build`](Self::build); then
/// [`load_schema`](BamlRuntimeManager::load_schema) and register tools as usual.
#[derive(Default)]
pub struct BamlRuntimeManagerBuilder {
    llm_secret_resolver: Option<Arc<dyn LlmSecretResolver>>,
}

impl BamlRuntimeManagerBuilder {
    pub fn with_llm_secret_resolver(self, resolver: Arc<dyn LlmSecretResolver>) -> Self {
        Self {
            llm_secret_resolver: Some(resolver),
        }
    }

    pub fn with_fnox_llm_resolver(self, path: impl AsRef<Path>) -> Self {
        self.with_fnox_resolver_inner(FnoxFileSecretResolver::from_path(Some(path.as_ref())))
    }

    /// Configure a fnox-backed LLM secret resolver using fnox's default discovery
    /// (`BAML_FNOX_CONFIG` env var, otherwise recursive search for `fnox.toml`).
    pub fn with_default_fnox_llm_resolver(self) -> Self {
        self.with_fnox_resolver_inner(FnoxFileSecretResolver::default_path_resolver())
    }

    fn with_fnox_resolver_inner(self, fnox: FnoxFileSecretResolver) -> Self {
        let resolver = Arc::new(SecretResolverToLlmAdapter::new(Arc::new(fnox)));
        self.with_llm_secret_resolver(resolver)
    }

    pub fn build(self) -> Result<BamlRuntimeManager> {
        let mut manager = BamlRuntimeManager::new()?;
        if let Some(resolver) = self.llm_secret_resolver {
            manager.set_llm_secret_resolver(resolver);
        }
        Ok(manager)
    }
}

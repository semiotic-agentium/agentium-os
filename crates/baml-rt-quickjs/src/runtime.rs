//! Runtime builder and configuration
//!
//! Provides a builder pattern for constructing and configuring the BAML runtime environment.

use std::{path::PathBuf, sync::Arc, time::Duration};

use baml_rt_core::{
    BamlRtError, Result,
    bus::{BusApi, BusWithEffects, EffectEmitter, EffectLiveness},
};
use baml_rt_interceptor::{InterceptorPipeline, LLMInterceptor, ToolInterceptor};
use tokio::sync::Mutex;

use crate::{
    a2a_stream::BridgeHandle, baml::BamlRuntimeManager, quickjs_bridge::QuickJSBridge,
};

/// Configuration for QuickJS runtime options.
///
/// These options map to `quickjs_runtime::builder::QuickJsRuntimeBuilder`. See the
/// crate README section "QuickJS optimization settings" for tuning guidance (GC,
/// memory, stack).
#[derive(Debug, Clone, Default)]
pub struct QuickJSConfig {
    /// Maximum memory limit in bytes (None = no limit)
    pub memory_limit: Option<u64>,

    /// Maximum stack size in bytes (None = default)
    pub max_stack_size: Option<u64>,

    /// Number of allocations before garbage collection runs (None = default)
    pub gc_threshold: Option<u64>,

    /// Garbage collection interval - triggers a full GC every set interval (None = disabled)
    pub gc_interval: Option<Duration>,

    /// Idle timeout in milliseconds when no effects are in-flight (default: 5000ms)
    /// When effects are active, the longer max_attempts_ms timeout applies.
    pub idle_timeout_ms: Option<u64>,

    /// Maximum timeout in milliseconds when effects are in-flight (default: 1,800,000ms = 30 minutes)
    /// This allows long-running I/O operations (tool calls, LLM calls) to complete.
    pub max_attempts_ms: Option<u64>,

    /// Stream collector idle timeout in seconds (default: 60). Tests can set lower.
    pub stream_collector_idle_secs: Option<u64>,

    /// Maximum number of stream invocations that may be active at once.
    /// `None` uses available parallelism.
    pub stream_concurrency: Option<usize>,
}

impl QuickJSConfig {
    /// Create a new QuickJS configuration with defaults
    pub fn new() -> Self {
        Self::default()
    }

    /// Set memory limit in bytes
    pub fn with_memory_limit(mut self, limit: Option<u64>) -> Self {
        self.memory_limit = limit;
        self
    }

    /// Set maximum stack size in bytes
    pub fn with_max_stack_size(mut self, size: Option<u64>) -> Self {
        self.max_stack_size = size;
        self
    }

    /// Set garbage collection threshold (number of allocations before GC runs)
    pub fn with_gc_threshold(mut self, threshold: Option<u64>) -> Self {
        self.gc_threshold = threshold;
        self
    }

    /// Set garbage collection interval
    ///
    /// This will start a timer thread which triggers a full GC every set interval.
    pub fn with_gc_interval(mut self, interval: Option<Duration>) -> Self {
        self.gc_interval = interval;
        self
    }

    /// Set idle timeout in milliseconds (when no effects are in-flight)
    ///
    /// Default is 5000ms. When effects are active, the longer max_attempts_ms timeout applies.
    pub fn with_idle_timeout_ms(mut self, timeout_ms: Option<u64>) -> Self {
        self.idle_timeout_ms = timeout_ms;
        self
    }

    /// Set maximum timeout in milliseconds (when effects are in-flight)
    ///
    /// Default is 1,800,000ms (30 minutes). This allows long-running I/O operations
    /// (tool calls, LLM calls, A2A requests) to complete.
    pub fn with_max_attempts_ms(mut self, timeout_ms: Option<u64>) -> Self {
        self.max_attempts_ms = timeout_ms;
        self
    }

    /// Set stream collector idle timeout in seconds (default: 60). Use lower in tests.
    pub fn with_stream_collector_idle_secs(mut self, secs: Option<u64>) -> Self {
        self.stream_collector_idle_secs = secs;
        self
    }

    /// Set the maximum number of active stream invocations allowed simultaneously.
    /// Defaults to available parallelism when unset.
    pub fn with_stream_concurrency(mut self, stream_concurrency: Option<usize>) -> Self {
        self.stream_concurrency = stream_concurrency;
        self
    }
}

/// Configuration for the BAML runtime environment
#[derive(Default)]
pub struct RuntimeConfig {
    /// Path to the BAML schema directory
    pub schema_path: Option<PathBuf>,

    /// Agent ID - REQUIRED for QuickJS bridge
    pub agent_id: Option<baml_rt_core::ids::AgentId>,

    /// QuickJS-specific configuration
    pub quickjs_config: QuickJSConfig,

    /// Additional environment variables to pass to BAML runtime
    pub env_vars: Vec<(String, String)>,

    /// LLM interceptor pipeline
    pub llm_interceptor_pipeline: Option<InterceptorPipeline<dyn LLMInterceptor>>,

    /// Tool interceptor pipeline
    pub tool_interceptor_pipeline: Option<InterceptorPipeline<dyn ToolInterceptor>>,

    /// Provenance writer (if provided, enables effects-first system)
    pub provenance_writer: Option<Arc<dyn baml_rt_provenance::ProvenanceWriter>>,
}

impl RuntimeConfig {
    /// Create a new default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the BAML schema path
    pub fn with_schema_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.schema_path = Some(path.into());
        self
    }

    /// Configure QuickJS runtime options
    ///
    /// This allows fine-grained control over the QuickJS runtime behavior,
    /// including memory limits, stack size, and module loading.
    pub fn with_quickjs_config(mut self, config: QuickJSConfig) -> Self {
        self.quickjs_config = config;
        self
    }

    /// Add an environment variable
    pub fn with_env_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.push((key.into(), value.into()));
        self
    }
}

/// Built runtime environment
pub struct Runtime {
    /// The BAML runtime manager
    pub baml_manager: Arc<Mutex<BamlRuntimeManager>>,

    /// Per-bridge handover handle (owns the bridge + dedicated OS thread).
    pub bridge_handle: Arc<BridgeHandle>,

    /// Runtime configuration
    pub config: RuntimeConfig,
}

impl Runtime {
    /// Get the BAML runtime manager
    pub fn baml_manager(&self) -> Arc<Mutex<BamlRuntimeManager>> {
        self.baml_manager.clone()
    }

    /// Get the QuickJS bridge (derived from the bridge handle).
    pub fn quickjs_bridge(&self) -> Arc<Mutex<QuickJSBridge>> {
        self.bridge_handle.bridge().clone()
    }

    /// Get the bridge handle for handover dispatch.
    pub fn bridge_handle(&self) -> Arc<BridgeHandle> {
        self.bridge_handle.clone()
    }
}

/// Builder for constructing a runtime environment
pub struct RuntimeBuilder {
    config: RuntimeConfig,
}

impl RuntimeBuilder {
    /// Create a new runtime builder with default configuration
    pub fn new() -> Self {
        Self {
            config: RuntimeConfig::default(),
        }
    }

    /// Set the BAML schema path
    pub fn with_schema_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.schema_path = Some(path.into());
        self
    }

    /// Set agent ID - REQUIRED
    pub fn with_agent_id(mut self, agent_id: baml_rt_core::ids::AgentId) -> Self {
        self.config.agent_id = Some(agent_id);
        self
    }

    /// Configure QuickJS runtime options
    ///
    /// This allows fine-grained control over the QuickJS runtime behavior,
    /// including memory limits, stack size, and garbage collection settings.
    ///
    /// # Example
    /// ```rust,no_run
    /// use baml_rt::{RuntimeBuilder, QuickJSConfig};
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// use baml_rt_core::ids::{AgentId, UuidId};
    /// let runtime = RuntimeBuilder::new()
    ///     .with_agent_id(AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000008").unwrap()))
    ///     .with_quickjs_config(
    ///         QuickJSConfig::new()
    ///             .with_memory_limit(Some(64 * 1024 * 1024)) // 64MB limit
    ///             .with_max_stack_size(Some(1024 * 1024)) // 1MB stack
    ///             .with_gc_interval(Some(Duration::from_secs(30))) // GC every 30 seconds
    ///     )
    ///     .build()
    ///     .await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    pub fn with_quickjs_config(mut self, config: QuickJSConfig) -> Self {
        self.config.quickjs_config = config;
        self
    }

    /// Add an environment variable
    pub fn with_env_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.env_vars.push((key.into(), value.into()));
        self
    }

    /// Add an LLM interceptor to the pipeline
    ///
    /// This allows composing interceptors in a pipeline pattern.
    pub fn with_llm_interceptor<I: LLMInterceptor>(mut self, interceptor: I) -> Self {
        let pipeline = self
            .config
            .llm_interceptor_pipeline
            .take()
            .unwrap_or_default();
        self.config.llm_interceptor_pipeline =
            Some(pipeline.with_interceptor(Arc::new(interceptor) as Arc<dyn LLMInterceptor>));
        self
    }

    /// Add multiple LLM interceptors to the pipeline
    pub fn with_llm_interceptors<I: LLMInterceptor>(mut self, interceptors: Vec<I>) -> Self {
        let mut pipeline = self
            .config
            .llm_interceptor_pipeline
            .take()
            .unwrap_or_default();

        for interceptor in interceptors {
            pipeline = pipeline.with_interceptor(Arc::new(interceptor) as Arc<dyn LLMInterceptor>);
        }

        self.config.llm_interceptor_pipeline = Some(pipeline);
        self
    }

    /// Set the LLM interceptor pipeline
    ///
    /// This replaces any existing LLM interceptor pipeline.
    pub fn with_llm_interceptor_pipeline(
        mut self,
        pipeline: InterceptorPipeline<dyn LLMInterceptor>,
    ) -> Self {
        self.config.llm_interceptor_pipeline = Some(pipeline);
        self
    }

    /// Add a tool interceptor to the pipeline
    ///
    /// This allows composing interceptors in a pipeline pattern.
    pub fn with_tool_interceptor<I: ToolInterceptor>(mut self, interceptor: I) -> Self {
        let pipeline = self
            .config
            .tool_interceptor_pipeline
            .take()
            .unwrap_or_default();
        self.config.tool_interceptor_pipeline =
            Some(pipeline.with_interceptor(Arc::new(interceptor) as Arc<dyn ToolInterceptor>));
        self
    }

    /// Add multiple tool interceptors to the pipeline
    pub fn with_tool_interceptors<I: ToolInterceptor>(mut self, interceptors: Vec<I>) -> Self {
        let mut pipeline = self
            .config
            .tool_interceptor_pipeline
            .take()
            .unwrap_or_default();

        for interceptor in interceptors {
            pipeline = pipeline.with_interceptor(Arc::new(interceptor) as Arc<dyn ToolInterceptor>);
        }

        self.config.tool_interceptor_pipeline = Some(pipeline);
        self
    }

    /// Set the tool interceptor pipeline
    ///
    /// This replaces any existing tool interceptor pipeline.
    pub fn with_tool_interceptor_pipeline(
        mut self,
        pipeline: InterceptorPipeline<dyn ToolInterceptor>,
    ) -> Self {
        self.config.tool_interceptor_pipeline = Some(pipeline);
        self
    }

    /// Set the provenance writer (enables effects-first system)
    ///
    /// When provided, creates a BusWithEffects and wires it to provenance and liveness gating.
    pub fn with_provenance_writer(
        mut self,
        writer: Arc<dyn baml_rt_provenance::ProvenanceWriter>,
    ) -> Self {
        self.config.provenance_writer = Some(writer);
        self
    }

    /// Build the runtime environment
    pub async fn build(mut self) -> Result<Runtime> {
        tracing::info!("Building runtime environment");

        // Create BAML runtime manager
        let mut baml_manager = BamlRuntimeManager::new()?;

        // Load schema if path is provided
        if let Some(schema_path) = &self.config.schema_path {
            let schema_path_str = schema_path.to_str().ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "Schema path contains invalid UTF-8: {:?}",
                    schema_path
                ))
            })?;
            baml_manager.load_schema(schema_path_str)?;
        }

        // Extract pipelines before moving config
        let llm_pipeline = self.config.llm_interceptor_pipeline.take();
        let tool_pipeline = self.config.tool_interceptor_pipeline.take();
        let provenance_writer = self.config.provenance_writer.take();

        // Create BusWithEffects if provenance writer is provided (bus-first system).
        let effect_bus: Option<Arc<BusWithEffects>> =
            if let Some(ref prov_writer) = provenance_writer {
                let bus = Arc::new(BusWithEffects::new());
                let subscriber = Arc::new(baml_rt_provenance::ProvenanceBusSubscriber::new(
                    prov_writer.clone(),
                ));
                bus.subscribe(subscriber).await;
                Some(bus)
            } else {
                None
            };

        // Set effect emitter on BamlRuntimeManager if bus exists
        if let Some(ref bus) = effect_bus {
            baml_manager.set_effect_emitter(bus.clone() as Arc<dyn EffectEmitter>);
        }

        // Inject LLM interceptor pipeline if provided
        if let Some(llm_pipeline) = llm_pipeline {
            let registry = baml_manager.interceptor_registry();
            let mut registry_guard = registry.lock().await;
            registry_guard.merge_llm_pipeline(llm_pipeline);
        }

        // Inject tool interceptor pipeline if provided
        if let Some(tool_pipeline) = tool_pipeline {
            let registry = baml_manager.interceptor_registry();
            let mut registry_guard = registry.lock().await;
            registry_guard.merge_tool_pipeline(tool_pipeline);
        }

        let baml_manager = Arc::new(Mutex::new(baml_manager));

        // Extract config fields before moving - clone quickjs_config to avoid partial move
        let RuntimeConfig {
            agent_id,
            quickjs_config,
            ..
        } = &self.config;
        let quickjs_config = quickjs_config.clone();
        let agent_id = agent_id.clone();
        let config = self.config; // Move config here

        // Create QuickJS bridge (always enabled)
        let agent_id = agent_id.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "agent_id is REQUIRED. Call with_agent_id() before build().".to_string(),
            )
        })?;
        let agent_id_label = agent_id.to_string();
        let mut bridge =
            QuickJSBridge::new_with_config(baml_manager.clone(), agent_id, quickjs_config.clone())
                .await?;

        // Set effect liveness on QuickJSBridge if bus exists
        if let Some(ref bus) = effect_bus {
            bridge.set_effect_liveness(bus.clone() as Arc<dyn EffectLiveness>);
        }

        bridge.register_baml_functions().await?;
        let quickjs_bridge = Arc::new(Mutex::new(bridge));
        let bridge_handle = Arc::new(BridgeHandle::new(
            quickjs_bridge,
            &agent_id_label,
        ));

        Ok(Runtime {
            baml_manager,
            bridge_handle,
            config,
        })
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

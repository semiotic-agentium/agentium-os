// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! BAML Runtime workspace facade.
//!
//! This crate re-exports functionality from the workspace sub-crates.

pub use baml_rt_core::{
    BamlRtError, Result, collect_a2a_stream_one_shot, collect_a2a_stream_until_one_shot,
    context::generate_context_id,
    correlation::{current_correlation_id, generate_correlation_id},
};
pub mod error {
    pub use baml_rt_core::error::*;
}

#[cfg(feature = "tools")]
pub mod tools {
    pub use baml_rt_tools::tools::*;
}
#[cfg(feature = "interceptor")]
pub mod interceptor {
    pub use baml_rt_interceptor::interceptor::*;
}
#[cfg(feature = "interceptor")]
pub mod interceptors {
    pub use baml_rt_interceptor::interceptors::*;
}

#[cfg(feature = "quickjs")]
pub mod baml {
    pub use baml_rt_quickjs::baml::*;
}
#[cfg(feature = "quickjs")]
pub mod baml_execution {
    pub use baml_rt_quickjs::baml_execution::*;
}
#[cfg(feature = "quickjs")]
pub mod baml_collector {
    pub use baml_rt_quickjs::baml_collector::*;
}
#[cfg(feature = "quickjs")]
pub mod baml_pre_execution {
    pub use baml_rt_quickjs::baml_pre_execution::*;
}
#[cfg(feature = "quickjs")]
pub mod quickjs_bridge {
    pub use baml_rt_quickjs::quickjs_bridge::*;
}
#[cfg(feature = "quickjs")]
pub mod js_value_converter {
    pub use baml_rt_quickjs::js_value_converter::*;
}
#[cfg(feature = "quickjs")]
pub mod context {
    pub use baml_rt_quickjs::context::*;
}
#[cfg(feature = "quickjs")]
pub mod runtime {
    pub use baml_rt_quickjs::runtime::*;
}
#[cfg(feature = "quickjs")]
pub mod traits {
    pub use baml_rt_quickjs::traits::*;
}

#[cfg(feature = "a2a")]
pub mod a2a {
    pub use baml_rt_a2a::a2a::*;
}
#[cfg(feature = "a2a")]
pub mod a2a_store {
    pub use baml_rt_a2a::a2a_store::*;
}
#[cfg(feature = "a2a")]
pub mod a2a_types {
    pub use baml_rt_a2a::a2a_types::*;
}
#[cfg(feature = "a2a")]
pub mod a2a_transport {
    pub use baml_rt_a2a::a2a_transport::*;
}

#[cfg(feature = "builder")]
pub mod builder {
    pub use baml_rt_builder::builder::*;
}

#[cfg(feature = "observability")]
pub mod metrics {
    pub use baml_rt_observability::metrics::*;
}
#[cfg(feature = "observability")]
pub mod spans {
    pub use baml_rt_observability::spans::*;
}
#[cfg(feature = "observability")]
pub mod tracing_setup {
    pub use baml_rt_observability::tracing_setup::*;
}

#[cfg(feature = "a2a")]
pub use baml_rt_a2a::{A2aAgent, A2aAgentBuilder, A2aJsChatHost, A2aRequestHandler};
#[cfg(feature = "a2a")]
pub use baml_rt_a2a::{A2aMethod, A2aOutcome, A2aRequest};
#[cfg(feature = "interceptor")]
pub use baml_rt_interceptor::{
    InterceptorDecision, InterceptorRegistry, LLMCallContext, LLMInterceptor, ToolCallContext,
    ToolInterceptor,
};
#[cfg(feature = "interceptor")]
pub use baml_rt_interceptor::{TracingInterceptor, TracingLLMInterceptor, TracingToolInterceptor};
#[cfg(feature = "quickjs")]
pub use baml_rt_quickjs::{BamlContext, BamlRuntimeManager, ContextMetadata};
#[cfg(feature = "quickjs")]
pub use baml_rt_quickjs::{
    QuickJSBridge, QuickJSConfig, Runtime, RuntimeBuilder, RuntimeConfig,
    SecretResolverToLlmAdapter,
};

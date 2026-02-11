//! BAML runtime with QuickJS integration.

#![recursion_limit = "256"]

pub mod a2a_stream;
pub mod baml;
pub mod baml_collector;
pub mod baml_execution;
pub mod baml_pre_execution;
pub mod context;
pub mod js_value_converter;
pub mod quickjs_bridge;
pub mod runtime;
pub mod traits;

pub use a2a_stream::{
    A2aYieldSession, InvocationComplete, YieldBufferReady, begin_a2a_yield_session,
};
pub use baml::{BamlRuntimeManager, ToolSessionExecutionHandle};
pub use context::{BamlContext, ContextMetadata};
pub use quickjs_bridge::QuickJSBridge;
pub use runtime::{QuickJSConfig, Runtime, RuntimeBuilder, RuntimeConfig};
pub use traits::{
    BamlFunctionExecutor, BamlGateway, JsRuntimeHost, SchemaLoader, ToolRegistryTrait,
};

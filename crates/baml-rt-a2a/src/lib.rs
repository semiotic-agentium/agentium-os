//! A2A protocol support.

pub mod a2a;
pub mod a2a_store;
pub mod a2a_transport;
pub mod a2a_types;
pub mod agent_registry;
pub mod error_classifier;
pub mod events;
pub mod handlers;
pub mod request_router;
pub mod response;
pub mod result_deduplicator;
pub mod result_extractor;
pub mod result_pipeline;
pub mod result_processor;
pub mod session_channel;
pub mod stream_normalizer;
pub mod tools;

pub use a2a::{A2aMethod, A2aOutcome, A2aRequest};
pub use a2a_transport::{A2aAgent, A2aAgentBuilder, A2aRequestHandler};
pub use agent_registry::AgentRegistry;
pub use tools::A2aSessionBundle;

//! Observability helpers (metrics, spans, tracing setup).

pub mod metrics;
pub mod otel_env;
pub mod runner_identity;
pub mod scope;
pub mod spans;
pub mod tracing_setup;

#[cfg(test)]
pub(crate) mod test_env;

pub use metrics::*;
pub use otel_env::*;
pub use runner_identity::{
    SERVICE_INSTANCE_ID_KEY, derive_service_instance_id, service_instance_id,
};
pub use scope::*;
pub use spans::*;
pub use tracing_setup::*;

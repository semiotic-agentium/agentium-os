//! BAML runtime core types and shared utilities.

pub mod correlation;
pub mod context;
pub mod effects;
pub mod error;
pub mod ids;
pub mod package;
pub mod types;

pub use context::{InvocationContext, InvocationScope, RuntimeScope, TaskLocalContext, task_local_context};
pub use effects::{EffectBus, EffectEmitter, EffectEvent, EffectKind, EffectLiveness, EffectSubscriber, InFlightCounts};
pub use error::{BamlRtError, Result};
pub use ids::{AgentId, ArtifactId, ContextId, CorrelationId, EventId, MessageId, TaskId};
pub use package::AgentManifest;

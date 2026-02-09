//! BAML runtime core types and shared utilities.

pub mod context;
pub mod correlation;
pub mod effects;
pub mod error;
pub mod ids;
pub mod package;
pub mod types;

pub use context::{
    InvocationContext, InvocationScope, RuntimeScope, Scoped, TaskLocalContext,
    context_id_or_generated, task_local_context,
};
pub use effects::{
    EffectBus, EffectEmitter, EffectEvent, EffectKind, EffectLiveness, EffectSubscriber,
    InFlightCounts,
};
pub use error::{BamlRtError, Result};
pub use ids::{AgentId, ArtifactId, ContextId, CorrelationId, EventId, MessageId, TaskId};
pub use package::AgentManifest;

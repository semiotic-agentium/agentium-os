//! Interpretation-first daemon for turning project-channel discussion into actionable outputs.
//!
//! The crate is organized around three boundaries:
//! - source polling (`daemon::TaskSource`)
//! - interpretation (`extract::TaskExtractor`)
//! - delivery (`sink::TaskSink`)
//!
//! The main payload is [`TaskBatch`], which includes project interpretation,
//! workflow seed data, and derived tasks for downstream systems.

pub mod clickup_source;
pub mod contract;
pub mod daemon;
pub mod extract;
mod llm_extract;
pub mod model;
pub mod sink;
pub mod slack_source;
pub mod state;

pub use clickup_source::{ClickupSourceConfig, ClickupSourceConfigError, ClickupTaskSource};
pub use contract::{
    ContractProvenance, ContractSource, INTERPRETATION_EVENT_SCHEMA_VERSION,
    InterpretationRequestEvent, InterpretationResultEvent,
};
pub use daemon::{
    RoundRobinTaskSource, RoundRobinTaskSourceError, SourcePoll, TaskDaemon, TaskSource,
};
pub use extract::{ExtractionMode, TaskExtractor};
pub use model::{
    ClarificationPrompt, DecisionItem, FollowUpItem, FollowUpKind, InvestigationPrompt,
    InvestigationRunCondition, InvestigationTask, ProjectContext, ProjectInterpretation,
    QuestionItem, RiskItem, SlackMessage, SourceReference, TaskBatch, TaskConfidence,
    TaskSourceKind, WorkflowSeed,
};
pub use sink::{
    A2aSink, ClickUpSink, GithubIssueSink, JsonlFileSink, SinkConstructorError, SinkDeliveryError,
    SinkDeliveryMode, StdoutSink, TaskSink, format_coordinator_prompt,
};
pub use slack_source::{SlackChannelSelector, SlackSourceConfig, SlackTaskSource};
pub use state::{SourceState, StateStore, TaskDaemonState};

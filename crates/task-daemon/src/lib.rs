//! Interpretation-first daemon for turning project-channel discussion into actionable outputs.
//!
//! The crate is organized around three boundaries:
//! - source polling (`daemon::TaskSource`)
//! - interpretation (`extract::TaskExtractor`)
//! - delivery (`sink::TaskSink`)
//!
//! The main payload is [`TaskBatch`], which includes project interpretation,
//! workflow seed data, and derived tasks for downstream systems.

pub mod daemon;
pub mod extract;
mod llm_extract;
pub mod model;
pub mod sink;
pub mod slack_source;
pub mod state;

pub use daemon::{SourcePoll, TaskDaemon, TaskSource};
pub use extract::{ExtractionMode, TaskExtractor};
pub use model::{
    ClarificationPrompt, DecisionItem, FollowUpItem, FollowUpKind, InvestigationPrompt,
    InvestigationRunCondition, InvestigationTask, ProjectContext, ProjectInterpretation,
    QuestionItem, RiskItem, SlackMessage, SourceReference, TaskBatch, TaskConfidence,
    TaskSourceKind, WorkflowSeed,
};
pub use sink::{ClickUpSink, JsonlFileSink, StdoutSink, TaskSink};
pub use slack_source::{SlackChannelSelector, SlackSourceConfig, SlackTaskSource};
pub use state::{SourceState, StateStore, TaskDaemonState};

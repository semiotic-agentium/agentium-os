//! Task-daemon turns source activity such as Slack discussion and ClickUp task
//! changes into structured work for people and agents.
//!
//! The main pieces are:
//! - polling work sources
//! - interpreting what changed
//! - sending the result to the selected output
//!
//! The primary structured output is [`InterpretationResultEvent`]. [`TaskDispatch`]
//! carries that result together with the additional context some outputs still use.

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
    InterpretationRequestEvent, InterpretationResultEvent, TaskDispatch,
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
    ClickUpSink, DispatchSink, GithubIssueSink, JsonlFileSink, SinkConstructorError,
    SinkDeliveryError, SinkDeliveryMode, SourceFilteredSink, StdoutSink, TaskSink,
    format_event_delivery_prompt,
};
pub use slack_source::{SlackChannelSelector, SlackSourceConfig, SlackTaskSource};
pub use state::{SourceState, StateStore, TaskDaemonState};

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

//! Task-daemon polls external sources and publishes `host.source-records.v1` to the runner.

pub mod clickup_source;
pub mod daemon;
pub mod model;
pub mod poll_lineage;
pub mod publish;
pub mod sink;
pub mod slack_source;
pub mod source_records;
pub mod state;

pub use clickup_source::{ClickupSourceConfig, ClickupSourceConfigError, ClickupTaskSource};
pub use daemon::{
    RoundRobinTaskSource, RoundRobinTaskSourceError, SourcePoll, TaskDaemon, TaskSource,
};
pub use model::{
    InvestigationTask, ProjectContext, SlackMessage, SourceReference, TaskConfidence,
    TaskSourceKind,
};
pub use publish::PublishSink;
pub use sink::{
    ClickUpSink, GithubIssueSink, JsonlFileSink, SinkConstructorError, SinkDeliveryError,
    SinkDeliveryMode, SourceFilteredSink, StdoutSink, TaskSink,
};
pub use slack_source::{SlackChannelSelector, SlackSourceConfig, SlackTaskSource};
pub use state::{SourceState, StateStore, TaskDaemonState};

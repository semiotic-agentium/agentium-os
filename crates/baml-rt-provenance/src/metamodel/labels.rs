//! Typed marker for each canonical graph node label. Every variant of
//! [`crate::graph_model::GraphNodeLabel`] has a corresponding ZST here that
//! implements the sealed [`NodeLabelTy`] trait. Subject markers in
//! [`crate::metamodel::query::GraphQuery`] use these types as phantom data so
//! that subject-specific filter keys, projections, and traversals can only be
//! reached on the right node label.

use crate::{graph_model::GraphNodeLabel, metamodel::sealed::Sealed};

/// Sealed trait implemented exactly by the ZST markers in this module. A
/// `T: NodeLabelTy` is a compile-time witness that we are reasoning about a
/// specific persisted node label.
pub trait NodeLabelTy: Sealed {
    /// The canonical [`GraphNodeLabel`] variant this marker represents.
    const LABEL: GraphNodeLabel;
    /// Convenience: the on-disk node label as a stable string. The string
    /// itself is only consumed by SurrealQL emission (`GraphQuery::into_surreal`),
    /// never typed at call sites.
    const LABEL_STR: &'static str = Self::LABEL.as_str();
}

macro_rules! node_label_ty {
    ($name:ident => $variant:ident) => {
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $name;
        impl Sealed for $name {}
        impl NodeLabelTy for $name {
            const LABEL: GraphNodeLabel = GraphNodeLabel::$variant;
        }
    };
}

node_label_ty!(Intent => Intent);
node_label_ty!(Plan => Plan);
node_label_ty!(PlanStep => PlanStep);
node_label_ty!(Message => Message);
node_label_ty!(MessageProcessing => MessageProcessing);
node_label_ty!(LlmCall => LlmCall);
node_label_ty!(ToolCall => ToolCall);
node_label_ty!(LlmPrompt => LlmPrompt);
node_label_ty!(ToolArgs => ToolArgs);
node_label_ty!(TaskExecution => TaskExecution);
node_label_ty!(Task => Task);
node_label_ty!(TaskState => TaskState);
node_label_ty!(Artifact => Artifact);
node_label_ty!(AgentBoot => AgentBoot);
node_label_ty!(AgentStop => AgentStop);
node_label_ty!(AgentArchive => AgentArchive);
node_label_ty!(AgentRuntimeInstance => AgentRuntimeInstance);
node_label_ty!(PromptRejected => PromptRejected);
node_label_ty!(FailureClassificationActivity => FailureClassificationActivity);
node_label_ty!(FailureClassification => FailureClassification);
node_label_ty!(SessionStep => SessionStep);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_str_matches_graph_node_label() {
        assert_eq!(Message::LABEL_STR, "Message");
        assert_eq!(Task::LABEL_STR, "A2ATask");
        assert_eq!(MessageProcessing::LABEL_STR, "A2AMessageProcessing");
        assert_eq!(LlmCall::LABEL_STR, "LlmCall");
        assert_eq!(AgentRuntimeInstance::LABEL_STR, "AgentRuntimeInstance");
    }
}

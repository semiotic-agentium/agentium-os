/// Polymorphic Open step for ExecuteStep: selects a tool and opens a session.
/// See SupportCrmToolCard, SupportEmailToolCard for tool capabilities.
class ExecuteStepOpenStep {
  op "Open"
  tool_name ("support/crm" | "support/email") @description("Which tool to open. See ToolCard classes for capabilities.")
}

/// Session plan for ExecuteStep: polymorphic tool selection via Open.
class ExecuteStepSessionPlan {
  step ExecuteStepOpenStep @description("Select a tool and emit Open. After this, the session auto-narrows to the selected tool's step executor.")
}

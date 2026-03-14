// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

enum SessionStepOp {
  Open
  Send
  Read
  Finish
  Abort
}

enum SyntheticProjectionMode {
  SUMMARY @alias("summary")
  IDENTITY @alias("identity")
  DETAIL @alias("detail")
}

class SessionContext {
  contract_version string
  session_open bool
  status_token string
  allowed_ops string[]
  last_status string?
}

class SessionReadEnvelopeLite {
  refId string
  budgetHint int?
}

class SyntheticPlannerSendInput {
  read SessionReadEnvelopeLite?
  goal_id string?
  limit int?
  projection SyntheticProjectionMode?
}

class SyntheticPlannerOpenInput {
  reason string?
}

class SyntheticPlannerStep {
  op SessionStepOp
  initial_input SyntheticPlannerOpenInput?
  input SyntheticPlannerSendInput?
  reason string?
}

class SyntheticPlannerPlan {
  decision SyntheticPlannerStep
  reason string?
}

class SyntheticPlannerCandidate {
  ref_id string
  failed_count int
  uncached_prompt_tokens int
  severity int
}

class SyntheticProjectionProgress {
  used string[]
  required string[]
}

class SyntheticPlannerState {
  level string
  goal_id string?
  refs string[]
  candidates SyntheticPlannerCandidate[]
  projection_progress SyntheticProjectionProgress
  last_read_ref string?
}

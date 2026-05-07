// Fixture: unified structured step executor (AskUser → WorkflowPlan-shaped terminal).
class UnifiedHarnessAsk {
  action string @description("Always 'AskUser'.")
  question string
}

class UnifiedHarnessPlanNode {
  id string
  kind string
  depends_on string[]
}

class UnifiedHarnessPlan {
  goal string
  nodes UnifiedHarnessPlanNode[]
}

function UnifiedHarnessPick(user_message: string) -> UnifiedHarnessPlan | UnifiedHarnessAsk {
  client DefaultClient
  prompt #"
    Emit JSON matching this hop's narrowed union only.
    User: {{ user_message }}
    {{ ctx.output_format }}
  "#
}

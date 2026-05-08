// claude-agent BAML.
//
// Session-tool coordination (`ChooseDevClaudeExtAction` + `DevClaudeExt*` step
// classes) ships with the dev/claude-ext tool package (`coordination.baml`)
// and is merged into the builder-managed `_baml_runtime.baml` prelude
// automatically. The agent's TS callback simply runs the step executor, then
// extracts Claude Code's terminal_result and returns it verbatim — Claude
// already produces user-ready markdown, no presenter LLM call needed.

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

retry_policy ParseRetry {
  max_retries 2
  strategy {
    type constant_delay
    delay_ms 200
  }
}

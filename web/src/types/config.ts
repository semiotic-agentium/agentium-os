/** Config API DTOs (match backend ToolConfigSchemaDto, ToolConfigDto, etc.) */

export interface ToolConfigSchemaDto {
  tool_name: string;
  schema: Record<string, unknown>;
  default?: Record<string, unknown>;
  has_config: boolean;
}

export interface ToolConfigDto {
  tool_name: string;
  config: Record<string, unknown>;
  version: number;
}

export interface ConfigVersionDto {
  version: number;
  config: Record<string, unknown>;
  created_at_ms: number;
}

export interface SecretRequestDto {
  name: string;
  secret_type: string;
  justification: string;
  descriptor: string;
}

/** One secret in the M:N overview: which tools and LLM clients require it. */
export interface SecretOverviewEntryDto {
  name: string;
  secret_type?: string;
  justification?: string;
  descriptor?: string;
  tool_consumers: string[];
  llm_consumers: string[];
  /** True if provisioned; false when missing or when resolver unavailable. Always present. */
  satisfied: boolean;
  /** When satisfied and linked via UI: the key in the secret store this secret is linked to. */
  linked_to?: string;
}

/** Supported LLM provider IDs (must match backend LlmProvider enum). */
export const LLM_PROVIDERS = [
  "openai",
  "openai-base",
  "openai-generic",
  "openai-responses",
  "azure-openai",
  "ollama",
  "openrouter",
  "anthropic",
  "google-ai",
  "vertex",
  "aws-bedrock",
] as const;

export type LlmProvider = (typeof LLM_PROVIDERS)[number];

/** LLM config shape (bundle `llm`) — matches LlmClientConfig */
export interface LlmClientDef {
  name: string;
  provider: LlmProvider | string;
  options: Record<string, string>;
  retry_policy?: string;
}

export interface LlmOverrides {
  agent?: Record<string, string>;
  agent_function?: Record<string, string>;
}

export interface LlmRetryPolicyDef {
  max_retries?: number;
  strategy?: unknown;
}

export interface LlmClientConfig {
  default: string;
  clients: Record<string, LlmClientDef>;
  overrides: LlmOverrides;
  retry_policies?: Record<string, LlmRetryPolicyDef>;
}

export const LLM_BUNDLE_NAME = "llm";

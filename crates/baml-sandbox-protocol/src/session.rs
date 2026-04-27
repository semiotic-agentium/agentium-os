//! Session-mode wire contract for the sandbox tool protocol.
//!
//! Adds the `tool/session_*` method family alongside the existing single-shot
//! `tool/invoke` path. Sessions are stateful: `open` creates an upstream
//! session, `send` enqueues input, `read` returns one [`StepEnvelope`], and
//! `finish` / `abort` close it.
//!
//! The shapes here mirror [`crate::protocol`]; both are single-source-of-truth
//! types shared by the host runtime and guest adapter SDK.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Method name for opening a session.
pub const METHOD_SESSION_OPEN: &str = "tool/session_open";

/// Method name for sending input into an open session.
pub const METHOD_SESSION_SEND: &str = "tool/session_send";

/// Method name for reading the next [`StepEnvelope`] from a session.
pub const METHOD_SESSION_READ: &str = "tool/session_read";

/// Method name for the normal-close path of a session.
pub const METHOD_SESSION_FINISH: &str = "tool/session_finish";

/// Method name for the abort/teardown path of a session.
pub const METHOD_SESSION_ABORT: &str = "tool/session_abort";

/// Methods a session-mode tool MUST advertise via `tool/describe`.
///
/// Includes the static handshake methods (`tool/describe`, `tool/schema`) and
/// the full session-method family. Strict validators reject session-mode
/// tools that omit any of these.
pub const SUPPORTED_METHODS_SESSION: &[&str] = &[
    crate::protocol::METHOD_DESCRIBE,
    crate::protocol::METHOD_SCHEMA,
    METHOD_SESSION_OPEN,
    METHOD_SESSION_SEND,
    METHOD_SESSION_READ,
    METHOD_SESSION_FINISH,
    METHOD_SESSION_ABORT,
];

/// Stable string code carried on [`StepError::code`].
///
/// These are the canonical platform-level codes referenced in
/// `plans/sandbox_streaming.md` §3.2. Tools may emit additional codes; hosts
/// must treat unknown codes as opaque.
pub mod error_code {
    /// Concurrent `session_read` against an active reader.
    pub const SESSION_BUSY: &str = "session_busy";
    /// Operation referenced an unknown session id.
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    /// `session_send` was issued with the wrong (or missing) resume token.
    pub const RESUME_TOKEN_MISMATCH: &str = "resume_token_mismatch";
    /// Pool checkout could not be satisfied within the configured timeout.
    pub const POOL_EXHAUSTED: &str = "pool_exhausted";
    /// Live session was force-aborted by an operator command.
    pub const EVICTED_BY_OPERATOR: &str = "evicted_by_operator";
    /// `session_read` waited beyond the configured chunk timeout.
    pub const CHUNK_TIMEOUT: &str = "chunk_timeout";
    /// Adapter `on_reset` failed or timed out, preventing reuse.
    pub const RESET_FAILED: &str = "reset_failed";
}

/// How a session-step error should flow back to host policy and the LLM.
///
/// Mirrors `baml_rt_core::semantics::ErrorDisposition`. Kept local so this
/// crate stays usable inside distroless guest images without pulling in
/// host-only deps. Hosts convert via a one-to-one mapping at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDisposition {
    /// Host may retry without involving the LLM.
    HostRetriable,
    /// Definitive failure for this call; surface to model, continue session.
    InformAndContinue,
    /// Bad args / schema; LLM can correct on next turn.
    LlmCorrectable,
    /// Unrecoverable; abort the session/turn.
    Fatal,
}

/// Error payload carried by [`StepEnvelope::Error`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepError {
    pub code: String,
    pub message: String,
    pub disposition: SessionDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

/// Canonical step envelope emitted by `session_read`.
///
/// Wire shape uses an internal tag named `step` with snake_case variants:
/// `streaming`, `suspended`, `done`, `error`. Isomorphic to
/// `baml_rt_tools::ToolStep`; host code maps directly between the two.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StepEnvelope {
    /// More output may follow; session remains open.
    Streaming { output: Value },
    /// Session yielded output and is awaiting a follow-up `session_send`.
    /// `resume_token` MUST be echoed on the next `session_send`.
    Suspended { output: Value, resume_token: String },
    /// Session completed; caller may finish or abort.
    Done {
        #[serde(default)]
        output: Option<Value>,
    },
    /// Session failed; classified by [`StepError::disposition`].
    Error { error: StepError },
}

/// Params for [`METHOD_SESSION_OPEN`].
///
/// `open_input` uses the same JSON shape as
/// [`crate::protocol::ToolInvokeParams::input`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOpenParams {
    pub invocation_id: String,
    pub tool_name: String,
    pub open_input: Value,
    /// Provided when `secret_scope=session`; omitted for `secret_scope=send`.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub secrets: serde_json::Map<String, Value>,
    /// Capability subset effective for this session (policy ∩ tool declaration).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub capabilities: Value,
}

/// Result payload for [`METHOD_SESSION_OPEN`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOpenResult {
    pub session_id: String,
    /// Optional first step the adapter is willing to surface synchronously
    /// at open time (e.g. immediate `Done` for trivial tools).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_step: Option<StepEnvelope>,
}

/// Params for [`METHOD_SESSION_SEND`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSendParams {
    pub session_id: String,
    pub input: Value,
    /// Required only when the last observed step for this session was
    /// [`StepEnvelope::Suspended`]; forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,
    /// Provided when `secret_scope=send` (default).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub secrets: serde_json::Map<String, Value>,
}

/// Ack payload for [`METHOD_SESSION_SEND`]. Per protocol §3.3, `send` returns
/// an ack or an error — never a step. Kept as an explicit struct so future
/// fields (e.g. accepted-byte counters) remain additive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionSendResult {}

/// Params for [`METHOD_SESSION_READ`]. The wire is parameterless beyond the
/// session id; payload-bearing reads must migrate to explicit `send` then
/// `read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReadParams {
    pub session_id: String,
}

/// Result payload for [`METHOD_SESSION_READ`] is exactly one [`StepEnvelope`].
pub type SessionReadResult = StepEnvelope;

/// Params for [`METHOD_SESSION_FINISH`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFinishParams {
    pub session_id: String,
}

/// Ack payload for [`METHOD_SESSION_FINISH`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionFinishResult {}

/// Params for [`METHOD_SESSION_ABORT`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAbortParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Ack payload for [`METHOD_SESSION_ABORT`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionAbortResult {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Round-trip every step variant: confirms the internal `step` tag,
    /// snake_case disposition, optional `output` on Done, and resume_token
    /// presence on Suspended.
    #[test]
    fn step_envelope_round_trips_all_variants() {
        let envs = vec![
            (
                StepEnvelope::Streaming {
                    output: json!({"chunk": 1}),
                },
                json!({"step": "streaming", "output": {"chunk": 1}}),
            ),
            (
                StepEnvelope::Suspended {
                    output: json!({"prompt": "ack?"}),
                    resume_token: "rt-1".into(),
                },
                json!({
                    "step": "suspended",
                    "output": {"prompt": "ack?"},
                    "resume_token": "rt-1"
                }),
            ),
            (
                StepEnvelope::Done { output: None },
                json!({"step": "done", "output": null}),
            ),
            (
                StepEnvelope::Error {
                    error: StepError {
                        code: error_code::SESSION_BUSY.into(),
                        message: "busy".into(),
                        disposition: SessionDisposition::HostRetriable,
                        hint: None,
                        retry_after_ms: Some(50),
                    },
                },
                json!({
                    "step": "error",
                    "error": {
                        "code": "session_busy",
                        "message": "busy",
                        "disposition": "host_retriable",
                        "retry_after_ms": 50
                    }
                }),
            ),
        ];

        for (env, expected) in envs {
            let serialized = serde_json::to_value(&env).unwrap();
            assert_eq!(serialized, expected, "serialize {env:?}");
            let _: StepEnvelope = serde_json::from_value(expected).unwrap();
        }
    }

    /// Open result must accept omitted `initial_step`, and every session
    /// method constant must appear in the advertised set.
    #[test]
    fn open_result_optional_and_method_set_complete() {
        let decoded: SessionOpenResult =
            serde_json::from_value(json!({"session_id": "s-1"})).unwrap();
        assert_eq!(decoded.session_id, "s-1");
        assert!(decoded.initial_step.is_none());

        for m in [
            METHOD_SESSION_OPEN,
            METHOD_SESSION_SEND,
            METHOD_SESSION_READ,
            METHOD_SESSION_FINISH,
            METHOD_SESSION_ABORT,
        ] {
            assert!(SUPPORTED_METHODS_SESSION.contains(&m), "missing {m}");
        }
    }
}

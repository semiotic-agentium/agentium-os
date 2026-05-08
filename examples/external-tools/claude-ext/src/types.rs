use serde_json::{Value, json};

pub const PROTOCOL_VERSION: &str = "1";
pub const TOOL_NAME: &str = "dev/claude-ext";

pub const METHOD_DESCRIBE: &str = "tool/describe";
pub const METHOD_SCHEMA: &str = "tool/schema";
pub const METHOD_SESSION_OPEN: &str = "tool/session_open";
pub const METHOD_SESSION_SEND: &str = "tool/session_send";
pub const METHOD_SESSION_READ: &str = "tool/session_read";
pub const METHOD_SESSION_FINISH: &str = "tool/session_finish";
pub const METHOD_SESSION_ABORT: &str = "tool/session_abort";

pub const ERR_INTERNAL: i32 = -32000;
pub const ERR_FAILED_PRECONDITION: i32 = -32002;
pub const ERR_UNAUTHENTICATED: i32 = -32003;
pub const ERR_UNAVAILABLE: i32 = -32004;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERR_INVALID_PARAMS: i32 = -32602;
pub const ERR_PARSE_ERROR: i32 = -32700;
pub const ERR_NOT_FOUND: i32 = -32006;

pub const SUPPORTED_METHODS: &[&str] = &[
    METHOD_DESCRIBE,
    METHOD_SCHEMA,
    METHOD_SESSION_OPEN,
    METHOD_SESSION_SEND,
    METHOD_SESSION_READ,
    METHOD_SESSION_FINISH,
    METHOD_SESSION_ABORT,
];

pub fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Streaming,
    Done,
    AbortedPendingRead,
    Aborted,
}

pub fn err(id: Value, code: i32, message: impl Into<String>, class: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
            "data": { "error_class": class }
        }
    })
}

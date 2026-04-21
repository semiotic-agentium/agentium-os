//! Stdio E2E suite for the sandbox adapter.
//!
//! Drives the reference `sandbox-echo-adapter` binary as a child
//! process, feeding it request frames on stdin and parsing response
//! frames off stdout. Each test wraps the whole interaction in a
//! timeout so a regression can't hang CI.
//!
//! Binary discovery uses `assert_cmd::cargo::cargo_bin` — the canonical
//! cross-package pattern per spec §18 X4.2, since `CARGO_BIN_EXE_…` is
//! only set for binaries inside the same package as the test. The echo
//! binary must be compiled before tests run; CI handles this with a
//! preceding `cargo build -p sandbox-echo-adapter`, and local dev can
//! run `cargo test -p baml-sandbox-adapter -p sandbox-echo-adapter`.
//!
//! All tests take the same shape: build an input byte stream, hand it
//! to `run_echo`, then `wait_with_output` collects the child's stdout,
//! stderr, and exit status. `parse_frames` reconstructs the framed
//! response stream and panics if it finds trailing unframed bytes —
//! that panic is the stdout-purity oracle used by the pollution test.

use std::{path::PathBuf, process::Stdio, time::Duration};

use baml_sandbox_protocol::{ERR_INTERNAL, JsonRpcRequest, METHOD_DESCRIBE, METHOD_INVOKE};
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn echo_binary() -> PathBuf {
    let path = assert_cmd::cargo::cargo_bin("sandbox-echo-adapter");
    if !path.exists() {
        panic!(
            "sandbox-echo-adapter binary missing at {}. \
             Build it first: `cargo build -p sandbox-echo-adapter`",
            path.display()
        );
    }
    path
}

fn request_envelope(method: &str, id: u64, params: Value) -> Value {
    serde_json::to_value(JsonRpcRequest::new(method, id, params)).unwrap()
}

fn invoke_params(input: Value) -> Value {
    json!({
        "invocation_id": "e2e",
        "tool_name": "sandbox-echo",
        "input": input,
        "secrets": {},
        "capabilities": null,
    })
}

fn frame_stream(requests: &[Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    for req in requests {
        let body = serde_json::to_vec(req).expect("serialize request");
        let len = u32::try_from(body.len()).expect("request fits in u32");
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&body);
    }
    buf
}

/// Feed `stdin_bytes` to a freshly spawned echo adapter, close stdin,
/// and collect the child's full `stdout`/`stderr`/exit status.
async fn run_echo(stdin_bytes: Vec<u8>) -> std::process::Output {
    let mut child = Command::new(echo_binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sandbox-echo-adapter");
    let mut stdin = child.stdin.take().expect("child stdin");
    stdin
        .write_all(&stdin_bytes)
        .await
        .expect("write stdin bytes");
    // Dropping closes the pipe; the adapter's framed `recv` surfaces EOF
    // on the length read, classifies it as clean teardown, and exits 0
    // — unless the input contained a truncated frame (mid-body EOF).
    drop(stdin);
    child
        .wait_with_output()
        .await
        .expect("child wait_with_output")
}

/// Decode a byte stream as consecutive length-prefixed JSON frames.
/// Panics on a truncated tail or trailing unframed bytes — this is the
/// stdout-purity oracle for the pollution test.
fn parse_frames(bytes: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        assert!(
            i + 4 <= bytes.len(),
            "truncated length header at offset {i} (remaining: {:?})",
            &bytes[i..]
        );
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        assert!(
            i + len <= bytes.len(),
            "truncated body at offset {i}: want {len} bytes, have {}",
            bytes.len() - i
        );
        let frame: Value = serde_json::from_slice(&bytes[i..i + len])
            .unwrap_or_else(|e| panic!("frame body is not JSON at offset {i}: {e}"));
        out.push(frame);
        i += len;
    }
    out
}

#[tokio::test]
async fn happy_path_describe_invoke() {
    let out = timeout(
        TEST_TIMEOUT,
        run_echo(frame_stream(&[
            request_envelope(METHOD_DESCRIBE, 1, json!({})),
            request_envelope(METHOD_INVOKE, 2, invoke_params(json!({"message": "ping"}))),
        ])),
    )
    .await
    .expect("happy_path_describe_invoke timed out");

    assert!(
        out.status.success(),
        "expected clean exit; status={:?}; stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let frames = parse_frames(&out.stdout);
    assert_eq!(frames.len(), 2, "frames={frames:?}");

    let describe = &frames[0];
    assert_eq!(describe["id"], 1);
    assert_eq!(describe["result"]["tool_name"], "sandbox-echo");
    assert_eq!(describe["result"]["protocol_version"], "1");

    let invoke = &frames[1];
    assert_eq!(invoke["id"], 2);
    assert_eq!(invoke["result"]["output"]["reply"], "ping");
    assert_eq!(invoke["result"]["done"], true);
}

#[tokio::test]
async fn panic_keepalive_next_invoke_succeeds() {
    let out = timeout(
        TEST_TIMEOUT,
        run_echo(frame_stream(&[
            request_envelope(METHOD_INVOKE, 1, invoke_params(json!({"panic": true}))),
            request_envelope(METHOD_INVOKE, 2, invoke_params(json!({"message": "alive"}))),
        ])),
    )
    .await
    .expect("panic_keepalive timed out");

    assert!(
        out.status.success(),
        "adapter must survive a tool panic; status={:?}",
        out.status
    );
    let frames = parse_frames(&out.stdout);
    assert_eq!(frames.len(), 2, "frames={frames:?}");

    let panic_resp = &frames[0];
    assert_eq!(panic_resp["id"], 1);
    assert_eq!(panic_resp["error"]["code"], ERR_INTERNAL);
    assert_eq!(panic_resp["error"]["data"]["error_class"], "execution");
    assert!(
        panic_resp["error"]["message"]
            .as_str()
            .map(|s| s.contains("panic"))
            .unwrap_or(false),
        "error message should mention the panic; got {panic_resp:?}"
    );

    let next = &frames[1];
    assert_eq!(next["id"], 2);
    assert_eq!(next["result"]["output"]["reply"], "alive");
}

#[tokio::test]
async fn malformed_frame_terminates_with_nonzero_exit() {
    // Length header claims 100 bytes; body provides only 50. The
    // adapter's `read_exact` on the body surfaces UnexpectedEof with
    // op="read body", which the dispatch-loop classifier must treat as
    // a wire desync — exit 1 — not as clean teardown.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&100u32.to_be_bytes());
    bytes.extend_from_slice(&[b'x'; 50]);

    let out = timeout(TEST_TIMEOUT, run_echo(bytes))
        .await
        .expect("malformed_frame_terminates timed out");

    assert!(
        !out.status.success(),
        "truncated body must produce non-zero exit; status={:?}; stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn protocol_version_mismatch_per_request_error_keepalive() {
    let out = timeout(
        TEST_TIMEOUT,
        run_echo(frame_stream(&[
            json!({
                "jsonrpc": "1.0",
                "id": 1,
                "method": METHOD_INVOKE,
                "params": invoke_params(json!({"message": "bad-version"})),
            }),
            request_envelope(
                METHOD_INVOKE,
                2,
                invoke_params(json!({"message": "still-alive"})),
            ),
        ])),
    )
    .await
    .expect("protocol_version_mismatch timed out");

    assert!(
        out.status.success(),
        "adapter should return per-request error and keep serving; status={:?}; stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let frames = parse_frames(&out.stdout);
    assert_eq!(frames.len(), 2, "frames={frames:?}");

    let mismatch = &frames[0];
    assert_eq!(mismatch["id"], 1);
    assert_eq!(mismatch["error"]["code"], ERR_INTERNAL);
    assert_eq!(mismatch["error"]["data"]["error_class"], "invalid_argument");
    assert!(
        mismatch["error"]["message"]
            .as_str()
            .map(|s| s.contains("requires '2.0'"))
            .unwrap_or(false),
        "error should explain protocol-version requirement; got {mismatch:?}"
    );

    let next = &frames[1];
    assert_eq!(next["id"], 2);
    assert_eq!(next["result"]["output"]["reply"], "still-alive");
}

#[tokio::test]
async fn shutdown_flush_complete_frame() {
    let out = timeout(
        TEST_TIMEOUT,
        run_echo(frame_stream(&[request_envelope(
            METHOD_INVOKE,
            1,
            invoke_params(json!({"message": "flush"})),
        )])),
    )
    .await
    .expect("shutdown_flush_complete_frame timed out");

    assert!(
        out.status.success(),
        "expected clean exit after stdin close; status={:?}; stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    // parse_frames would panic on truncated output or trailing garbage,
    // so this asserts the final response frame was fully flushed.
    let frames = parse_frames(&out.stdout);
    assert_eq!(frames.len(), 1, "frames={frames:?}");
    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[0]["result"]["output"]["reply"], "flush");
}

#[tokio::test]
async fn stdout_purity_survives_pollution() {
    let out = timeout(
        TEST_TIMEOUT,
        run_echo(frame_stream(&[request_envelope(
            METHOD_INVOKE,
            1,
            invoke_params(json!({"message": "x", "pollute": true})),
        )])),
    )
    .await
    .expect("stdout_purity timed out");

    assert!(
        out.status.success(),
        "expected clean exit; status={:?}",
        out.status
    );
    // parse_frames panics on trailing unframed bytes, so a successful
    // parse is the stdout-purity oracle — if the `println!` inside the
    // tool had reached fd 1, the frame stream would be corrupted.
    let frames = parse_frames(&out.stdout);
    assert_eq!(frames.len(), 1, "frames={frames:?}");
    assert_eq!(frames[0]["result"]["output"]["reply"], "x");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("echo pollution attempt"),
        "pollution must land on stderr; stderr={stderr}"
    );
}

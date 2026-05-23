//! Centralized error mapping for `tokio::task::spawn_blocking` failures.
//!
//! When a blocking task panics, `JoinError`'s `Display` impl renders only
//! `"task panicked at ..."` — the panic payload is dropped. Operators
//! debugging deploy/publish failures via the HTTP API need the actual
//! panic message in the response body, not the runner stderr.
//!
//! [`join_error_message`] extracts the panic payload via
//! [`tokio::task::JoinError::into_panic`] and downcasts it to the standard
//! `&'static str` / `String` shapes produced by `panic!`.

use tokio::task::JoinError;

const NON_STRING_PANIC_PAYLOAD: &str = "<non-string panic payload>";

/// Render a [`JoinError`] for inclusion in a higher-level error string,
/// extracting the panic payload when the task panicked.
///
/// `operation` is a short label describing what the blocking task was doing
/// (e.g. `"agent package load"`); it appears as the prefix of the returned
/// message.
#[must_use]
pub fn join_error_message(operation: &str, err: JoinError) -> String {
    if err.is_panic() {
        let payload = err.into_panic();
        let msg = payload
            .downcast_ref::<&'static str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| NON_STRING_PANIC_PAYLOAD.to_string());
        format!("{operation} panicked: {msg}")
    } else if err.is_cancelled() {
        format!("{operation} blocking task cancelled")
    } else {
        format!("{operation} blocking task failed: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn spawn_panicking<F>(body: F) -> JoinError
    where
        F: FnOnce() + Send + 'static,
    {
        tokio::task::spawn_blocking(body)
            .await
            .expect_err("blocking task was expected to panic")
    }

    #[tokio::test]
    async fn static_str_panic_payload_surfaces_in_message() {
        let err = spawn_panicking(|| panic!("tsc assertion tripped")).await;

        let msg = join_error_message("artifact build", err);

        assert_eq!(
            msg, "artifact build panicked: tsc assertion tripped",
            "operator-facing message should be the bare panic payload without JoinError boilerplate"
        );
    }

    #[tokio::test]
    async fn owned_string_panic_payload_surfaces_in_message() {
        let err = spawn_panicking(|| {
            let dynamic = format!("malformed manifest at offset {}", 42);
            panic!("{dynamic}");
        })
        .await;

        let msg = join_error_message("manifest parse", err);

        assert_eq!(
            msg, "manifest parse panicked: malformed manifest at offset 42",
            "formatted panic payloads (owned String) should surface verbatim"
        );
    }

    #[tokio::test]
    async fn non_string_panic_payload_is_labeled() {
        let err = spawn_panicking(|| std::panic::panic_any(404_u32)).await;

        let msg = join_error_message("registry lookup", err);

        assert!(msg.starts_with("registry lookup panicked: "), "got: {msg}");
        assert!(
            msg.contains(NON_STRING_PANIC_PAYLOAD),
            "non-string payload should be labeled, got: {msg}"
        );
    }
}

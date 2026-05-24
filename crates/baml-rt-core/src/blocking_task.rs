//! Centralized error mapping for `tokio::task::spawn_blocking` failures.
//!
//! Tokio's `JoinError` `Display` impl renders string panics as
//! `task <id> panicked with message "<msg>"` and non-string panics as
//! `task <id> panicked` — operator-facing surfaces (HTTP 500 detail bodies,
//! `tracing::error!` output) end up with the payload either wrapped in
//! escape-quotes or, for `panic_any(custom_type)`, dropped entirely.
//!
//! [`join_error_message`] strips the boilerplate: it captures the tokio
//! task id, extracts the payload via [`tokio::task::JoinError::into_panic`]
//! with downcasts to `&'static str` / `String`, and falls back to the raw
//! `JoinError` `Display` (which carries the panic location) when the
//! payload is neither.

use tokio::task::JoinError;

/// Render a [`JoinError`] for inclusion in a higher-level error string,
/// extracting the panic payload when the task panicked and including the
/// tokio task id so concurrent failures can be correlated.
///
/// `operation` is a short label describing what the blocking task was doing
/// (e.g. `"agent package load"`); it appears as the prefix of the returned
/// message.
#[must_use]
pub fn join_error_message(operation: &str, err: JoinError) -> String {
    let id = err.id();
    if err.is_panic() {
        // Capture Display before into_panic() consumes `err`, so non-string
        // payloads still have the panic location / type info to surface.
        let join_display = err.to_string();
        let payload = err.into_panic();
        let msg = payload
            .downcast_ref::<&'static str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or(join_display);
        format!("{operation} (task {id}) panicked: {msg}")
    } else if err.is_cancelled() {
        format!("{operation} (task {id}) blocking task cancelled")
    } else {
        // Unreachable in tokio 1.x — `JoinError` has only Panic and
        // Cancelled discriminants — but kept as a defensive default in
        // case a future tokio release adds a variant.
        format!("{operation} (task {id}) blocking task failed: {err}")
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

        assert!(
            msg.starts_with("artifact build (task ") && msg.contains(") panicked: "),
            "expected `operation (task <id>) panicked: ...` prefix, got: {msg}"
        );
        assert!(
            msg.ends_with(": tsc assertion tripped"),
            "panic payload should appear unwrapped, got: {msg}"
        );
        assert!(
            !msg.contains("panicked with message"),
            "JoinError Display boilerplate should be stripped, got: {msg}"
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

        assert!(
            msg.starts_with("manifest parse (task ") && msg.contains(") panicked: "),
            "expected `operation (task <id>) panicked: ...` prefix, got: {msg}"
        );
        assert!(
            msg.ends_with(": malformed manifest at offset 42"),
            "formatted (String) panic payloads should surface verbatim, got: {msg}"
        );
    }

    #[tokio::test]
    async fn non_string_panic_payload_falls_back_to_join_error_display() {
        let err = spawn_panicking(|| std::panic::panic_any(404_u32)).await;

        let msg = join_error_message("registry lookup", err);

        assert!(
            msg.starts_with("registry lookup (task ") && msg.contains(") panicked: "),
            "got: {msg}"
        );
        // The JoinError Display for a non-string panic includes the task id
        // and the substring "panicked" without a message — verifies we
        // surfaced something diagnostic rather than dropping the payload.
        assert!(
            msg.contains("task ") && msg.matches("task ").count() >= 2,
            "fallback should include JoinError Display (which mentions \"task <id>\"), got: {msg}"
        );
    }

    #[tokio::test]
    async fn task_id_is_included_for_cancellation() {
        let handle = tokio::task::spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_secs(60));
        });
        handle.abort();
        let err = handle.await.expect_err("aborted task must yield JoinError");

        let msg = join_error_message("agent package load", err);

        // spawn_blocking tasks ignore abort once they start; only a not-yet
        // started task will surface as cancelled. Accept either outcome —
        // both branches must include the task id breadcrumb.
        assert!(
            msg.starts_with("agent package load (task ")
                && (msg.contains(") blocking task cancelled")
                    || msg.contains(") panicked: ")
                    || msg.contains(") blocking task failed: ")),
            "every JoinError branch must include the task id, got: {msg}"
        );
    }
}

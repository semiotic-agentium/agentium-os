//! Canonical async panic-containment primitive for the adapter dispatch loop.
//!
//! Tool authors write arbitrary async code; a panic in their future must
//! not take down the whole adapter process. The dispatch loop wraps every
//! invocation in [`catch_tool_panic`] so a panic becomes a per-request
//! error frame (with `ErrorClass::Execution`) and the adapter keeps
//! serving subsequent requests.
//!
//! Implementation rides [`futures_util::FutureExt::catch_unwind`] over an
//! [`AssertUnwindSafe`] wrapper. `AssertUnwindSafe` is correct here: tool
//! state lives behind `&self` in the `SandboxTool` trait, and a panic
//! leaves no observable mutation the adapter carries into the next
//! request (the `Tool` itself is borrowed read-only). If a tool later
//! ships with `&mut` state crossing the unwind boundary, revisit.

use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;

/// Run `fut`, capturing any panic payload as a human-readable message.
///
/// - `Ok(inner)` on normal completion.
/// - `Err(message)` on panic — payload downcast to `String` or `&str`
///   when possible; otherwise a `"panic (non-string payload)"` fallback
///   so the caller always has something to surface.
pub(crate) async fn catch_tool_panic<F, T>(fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = T>,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(value) => Ok(value),
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "panic (non-string payload)".to_string());
            Err(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn propagates_successful_future() {
        let out = catch_tool_panic(async { 7u32 }).await.unwrap();
        assert_eq!(out, 7);
    }

    #[tokio::test]
    async fn captures_string_payload() {
        let err = catch_tool_panic(async {
            panic!("{}", String::from("boom-owned"));
        })
        .await
        .unwrap_err();
        assert_eq!(err, "boom-owned");
    }

    #[tokio::test]
    async fn captures_static_str_payload() {
        let err = catch_tool_panic(async { panic!("boom-static") })
            .await
            .unwrap_err();
        assert_eq!(err, "boom-static");
    }

    #[tokio::test]
    async fn falls_back_for_non_string_payload() {
        let err = catch_tool_panic(async {
            std::panic::panic_any(42u32);
            #[expect(unreachable_code, reason = "panic_any above diverges; the unit tail only satisfies the closure return type")]
            ()
        })
        .await
        .unwrap_err();
        assert_eq!(err, "panic (non-string payload)");
    }
}

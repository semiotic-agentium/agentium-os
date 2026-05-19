// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Ephemeral-port reservation helpers for test harnesses.
//!
//! Centralizes the "bind to host:0, capture `local_addr()`, drop the listener so
//! a subprocess/follow-up bind can take the port" pattern. Each helper inherits
//! the kernel's ephemeral port allocation — there is a brief TOCTOU window
//! between releasing the reservation and the caller re-binding, during which
//! another process on the host could grab the port. Callers must re-bind
//! promptly.

use std::net::{SocketAddr, TcpListener};

/// Reserve a free TCP port on `host`, return the bound `SocketAddr`, and
/// release the listener.
///
/// Use this when a subprocess or follow-up bind needs to claim the port (e.g.
/// spawning a runner binary that will bind it itself). Pass `"127.0.0.1"` for
/// loopback or a specific interface IP for cross-host tests.
///
/// # Panics
///
/// Panics if `host:0` cannot be bound (no free ports, malformed `host`).
pub fn reserve_ephemeral_addr(host: &str) -> SocketAddr {
    let listener = TcpListener::bind(format!("{host}:0"))
        .unwrap_or_else(|e| panic!("bind ephemeral port on {host}: {e}"));
    let addr = listener
        .local_addr()
        .expect("local_addr of reserved listener");
    drop(listener);
    addr
}

/// Async/tokio variant for callers that need to keep the listener live —
/// e.g. they will `axum::serve(listener, app)` on the returned listener.
///
/// Returns the bound `tokio::net::TcpListener` and the `SocketAddr` it is
/// listening on. The caller owns the listener; dropping it releases the port.
pub async fn bind_ephemeral_tokio(
    host: &str,
) -> std::io::Result<(tokio::net::TcpListener, SocketAddr)> {
    let listener = tokio::net::TcpListener::bind(format!("{host}:0")).await?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// Fire-and-forget axum server on an ephemeral port (test harness).
pub async fn serve_ephemeral_axum(app: axum::Router, api_suffix: &str) -> String {
    let (listener, addr) = bind_ephemeral_tokio("127.0.0.1")
        .await
        .expect("bind ephemeral listener");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}{api_suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_ephemeral_addr_returns_loopback_port() {
        let addr = reserve_ephemeral_addr("127.0.0.1");
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0);
    }

    #[tokio::test]
    async fn bind_ephemeral_tokio_returns_live_listener() {
        let (listener, addr) = bind_ephemeral_tokio("127.0.0.1").await.expect("bind");
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0);
        // Listener is still alive; binding the same port again must fail.
        assert!(
            tokio::net::TcpListener::bind(addr).await.is_err(),
            "port should still be held"
        );
        drop(listener);
    }
}

//! Token authentication for cluster control-plane endpoints.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::{body::Body, http::Request, response::IntoResponse};
use http_api_problem::HttpApiProblem;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tower::{Layer, Service};

/// Deployment topology governing control-endpoint authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterMode {
    /// Single runner — control endpoints allow unauthenticated access when no
    /// runner token is configured (backwards-compatible).
    Standalone,
    /// Cluster (shared SurrealDB) — control endpoints reject requests when no
    /// runner token is configured (fail-closed).
    Cluster,
}

/// Constant-time token comparison using SHA-256 digests to prevent timing attacks.
pub fn tokens_match(provided: &[u8], expected: &[u8]) -> bool {
    let hp = Sha256::digest(provided);
    let he = Sha256::digest(expected);
    hp.ct_eq(&he).into()
}

/// Authorization decision for control endpoints.
///
/// - **Cluster mode**: rejects when no token is configured (fail-closed).
/// - **Standalone mode**: allows unauthenticated access when no token is
///   configured (backwards-compatible single-runner behaviour).
/// - When a token *is* configured, both modes validate it.
pub fn check_control_auth(
    runner_token: Option<&str>,
    cluster_mode: ClusterMode,
    provided: &str,
) -> Result<(), HttpApiProblem> {
    match runner_token {
        Some(expected) => {
            if !tokens_match(provided.as_bytes(), expected.as_bytes()) {
                return Err(problem(
                    401,
                    "Unauthorized",
                    "missing or invalid X-Runner-Token",
                ));
            }
            Ok(())
        }
        None if cluster_mode == ClusterMode::Cluster => Err(problem(
            401,
            "Unauthorized",
            "runner_token is not configured; control endpoints require authentication in cluster mode",
        )),
        None => Ok(()),
    }
}

fn problem(status: u16, title: &str, detail: impl Into<String>) -> HttpApiProblem {
    HttpApiProblem::try_new(status)
        .expect("valid HTTP status code")
        .title(title.to_string())
        .detail(detail.into())
}

/// Configuration for [`ClusterAuthLayer`].
#[derive(Clone)]
pub struct ClusterAuthConfig {
    pub runner_token: Option<String>,
    pub cluster_mode: ClusterMode,
}

/// Tower layer that enforces token auth on all routes it wraps.
#[derive(Clone)]
pub struct ClusterAuthLayer {
    config: Arc<ClusterAuthConfig>,
}

impl ClusterAuthLayer {
    /// # Panics
    ///
    /// Panics if `runner_token` is `Some("")` — an empty token would silently
    /// pass authentication for any request without an `X-Runner-Token` header.
    pub fn new(config: ClusterAuthConfig) -> Self {
        assert!(
            config.runner_token.as_deref() != Some(""),
            "runner_token must not be an empty string; use None to disable token auth"
        );
        Self {
            config: Arc::new(config),
        }
    }
}

impl<S> Layer<S> for ClusterAuthLayer {
    type Service = ClusterAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ClusterAuthService {
            inner,
            config: self.config.clone(),
        }
    }
}

/// Tower service that checks `X-Runner-Token` before forwarding to the inner service.
#[derive(Clone)]
pub struct ClusterAuthService<S> {
    inner: S,
    config: Arc<ClusterAuthConfig>,
}

impl<S> Service<Request<Body>> for ClusterAuthService<S>
where
    S: Service<Request<Body>, Response = axum::response::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = axum::response::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let raw_header = req.headers().get("X-Runner-Token");
        let provided = match raw_header {
            Some(v) => match v.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => {
                    tracing::warn!(
                        "X-Runner-Token header contains non-UTF8 bytes, treating as absent"
                    );
                    String::new()
                }
            },
            None => String::new(),
        };

        let auth_result = check_control_auth(
            self.config.runner_token.as_deref(),
            self.config.cluster_mode,
            &provided,
        );

        let mut inner = self.inner.clone();
        Box::pin(async move {
            match auth_result {
                Ok(()) => inner.call(req).await,
                Err(problem) => Ok(problem.into_response()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_no_token_allows_access() {
        let result = check_control_auth(None, ClusterMode::Standalone, "");
        assert!(result.is_ok());
    }

    #[test]
    fn standalone_no_token_allows_any_header() {
        let result = check_control_auth(None, ClusterMode::Standalone, "anything");
        assert!(result.is_ok());
    }

    #[test]
    fn standalone_with_token_accepts_match() {
        let result = check_control_auth(Some("secret"), ClusterMode::Standalone, "secret");
        assert!(result.is_ok());
    }

    #[test]
    fn standalone_with_token_rejects_mismatch() {
        let result = check_control_auth(Some("secret"), ClusterMode::Standalone, "wrong");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status.unwrap().as_u16(), 401);
    }

    #[test]
    fn standalone_with_token_rejects_empty() {
        let result = check_control_auth(Some("secret"), ClusterMode::Standalone, "");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status.unwrap().as_u16(), 401);
    }

    #[test]
    fn cluster_no_token_rejects() {
        let result = check_control_auth(None, ClusterMode::Cluster, "");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status.unwrap().as_u16(), 401);
        let detail = err.detail.as_deref().unwrap_or("");
        assert!(
            detail.contains("cluster mode"),
            "detail should mention cluster mode: {detail}"
        );
    }

    #[test]
    fn cluster_with_token_accepts_match() {
        let result = check_control_auth(Some("secret"), ClusterMode::Cluster, "secret");
        assert!(result.is_ok());
    }

    #[test]
    fn cluster_with_token_rejects_mismatch() {
        let result = check_control_auth(Some("secret"), ClusterMode::Cluster, "wrong");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status.unwrap().as_u16(), 401);
    }

    #[test]
    fn tokens_match_identical() {
        assert!(tokens_match(b"secret-token-123", b"secret-token-123"));
    }

    #[test]
    fn tokens_match_different() {
        assert!(!tokens_match(b"secret-token-123", b"wrong-token-456"));
    }

    #[test]
    fn tokens_match_empty() {
        assert!(tokens_match(b"", b""));
    }

    #[test]
    #[should_panic(expected = "runner_token must not be an empty string")]
    fn rejects_empty_configured_token() {
        ClusterAuthLayer::new(ClusterAuthConfig {
            runner_token: Some(String::new()),
            cluster_mode: ClusterMode::Standalone,
        });
    }

    // -- ClusterAuthLayer integration tests --

    use axum::{body::Body, http::StatusCode, routing::get};
    use tower::ServiceExt;

    fn test_router(token: Option<&str>, mode: ClusterMode) -> axum::Router {
        let layer = ClusterAuthLayer::new(ClusterAuthConfig {
            runner_token: token.map(String::from),
            cluster_mode: mode,
        });
        axum::Router::new()
            .route("/control/test", get(|| async { "ok" }))
            .layer(layer)
    }

    #[tokio::test]
    async fn layer_forwards_with_valid_token() {
        let app = test_router(Some("secret"), ClusterMode::Cluster);
        let req = Request::builder()
            .uri("/control/test")
            .header("X-Runner-Token", "secret")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn layer_rejects_missing_token() {
        let app = test_router(Some("secret"), ClusterMode::Cluster);
        let req = Request::builder()
            .uri("/control/test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn layer_rejects_wrong_token() {
        let app = test_router(Some("secret"), ClusterMode::Cluster);
        let req = Request::builder()
            .uri("/control/test")
            .header("X-Runner-Token", "wrong")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn layer_standalone_no_token_allows() {
        let app = test_router(None, ClusterMode::Standalone);
        let req = Request::builder()
            .uri("/control/test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn layer_cluster_no_token_rejects() {
        let app = test_router(None, ClusterMode::Cluster);
        let req = Request::builder()
            .uri("/control/test")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

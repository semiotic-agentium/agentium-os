use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};

pub const HTTP_OP_PUBLISH: &str = "Publish";
pub const HTTP_OP_DEPLOY: &str = "Deploy";
pub const HTTP_OP_UNDEPLOY: &str = "Undeploy";
pub const HTTP_OP_LIST_DEPLOYED_INSTANCES: &str = "List deployed instances";

/// Resolved operator token for authenticated CLI operations.
///
/// Wraps a non-empty, validated token string. Debug impl redacts the value.
pub struct RunnerToken(String);

impl std::fmt::Debug for RunnerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunnerToken(***)")
    }
}

impl RunnerToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Resolve a runner token from explicit sources (pure, testable).
///
/// `flag` is the CLI `--runner-token` value; `env_value` is the `RUNNER_TOKEN`
/// environment variable. Flag takes precedence. Empty or whitespace-only values
/// from either source are rejected.
pub fn resolve_token_from_sources(
    flag: Option<&str>,
    env_value: Option<String>,
) -> Result<Option<RunnerToken>> {
    let trimmed = match flag {
        Some(v) => Some(v.trim().to_owned()),
        None => env_value.map(|v| v.trim().to_owned()),
    };
    match trimmed {
        Some(v) if v.is_empty() => {
            bail!(
                "Runner token is empty or whitespace-only. \
                 Provide a valid token via --runner-token or RUNNER_TOKEN."
            );
        }
        Some(v) => Ok(Some(RunnerToken(v))),
        None => Ok(None),
    }
}

/// Resolve a runner token from CLI flag with `RUNNER_TOKEN` env fallback.
pub fn resolve_runner_token(flag: Option<&str>) -> Result<Option<RunnerToken>> {
    resolve_token_from_sources(flag, std::env::var("RUNNER_TOKEN").ok())
}

pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

pub struct AgentPlatform {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
    runner_token: Option<RunnerToken>,
}

pub fn build_http_client(connect_timeout: Option<Duration>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if let Some(timeout) = connect_timeout {
        builder = builder.connect_timeout(timeout);
    }
    builder.build().context("Failed to build HTTP client")
}

impl AgentPlatform {
    pub fn new(runner_token: Option<RunnerToken>) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
        let client = build_http_client(None)?;
        Ok(Self {
            runtime,
            client,
            runner_token,
        })
    }

    fn check_response(
        &self,
        status: reqwest::StatusCode,
        body: &str,
        url: &str,
        op_name: &str,
    ) -> Result<()> {
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let hint = if self.runner_token.is_some() {
                "Hint: the runner token was rejected \
                 — verify it matches the server's RUNNER_TOKEN."
            } else {
                "Hint: pass --runner-token <token> or set the \
                 RUNNER_TOKEN environment variable."
            };
            bail!("{op_name} failed ({status}) at {url}: {body}. {hint}");
        }
        if !status.is_success() {
            bail!("{op_name} failed ({status}) at {url}: {body}");
        }
        Ok(())
    }

    pub fn post_json<Req, Resp>(&self, url: &str, payload: &Req, op_name: &str) -> Result<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        self.runtime.block_on(async {
            let mut request = self
                .client
                .post(url)
                .header("content-type", "application/json");
            if let Some(token) = &self.runner_token {
                request = request.header("X-Runner-Token", token.as_str());
            }
            let resp = request
                .json(payload)
                .send()
                .await
                .with_context(|| format!("Failed to POST {op_name} to {url}"))?;

            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            self.check_response(status, &body, url, op_name)?;

            serde_json::from_str::<Resp>(&body)
                .with_context(|| format!("Failed to parse {op_name} response: {body}"))
        })
    }

    pub fn get_json<Resp>(&self, url: &str, op_name: &str) -> Result<Resp>
    where
        Resp: DeserializeOwned,
    {
        self.runtime.block_on(async {
            let mut request = self.client.get(url);
            if let Some(token) = &self.runner_token {
                request = request.header("X-Runner-Token", token.as_str());
            }
            let resp = request
                .send()
                .await
                .with_context(|| format!("Failed to GET {op_name} from {url}"))?;

            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            self.check_response(status, &body, url, op_name)?;

            serde_json::from_str::<Resp>(&body)
                .with_context(|| format!("Failed to parse {op_name} response: {body}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{Router, extract::State, http::HeaderMap, routing::post};
    use serde::{Deserialize, Serialize};
    use tokio::sync::Mutex;

    use super::*;

    // --- resolve_token_from_sources tests (pure, no env mutation) ---

    #[test]
    fn test_resolve_flag_takes_precedence_over_env() {
        let result =
            resolve_token_from_sources(Some("flag-value"), Some("env-value".to_string())).unwrap();
        assert_eq!(result.unwrap().as_str(), "flag-value");
    }

    #[test]
    fn test_resolve_env_fallback() {
        let result = resolve_token_from_sources(None, Some("env-value".to_string())).unwrap();
        assert_eq!(result.unwrap().as_str(), "env-value");
    }

    #[test]
    fn test_resolve_none_when_absent() {
        let result = resolve_token_from_sources(None, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_rejects_empty_flag() {
        let err = resolve_token_from_sources(Some(""), None).unwrap_err();
        assert!(
            err.to_string().contains("empty or whitespace-only"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_resolve_rejects_whitespace_flag() {
        let err = resolve_token_from_sources(Some("   "), None).unwrap_err();
        assert!(
            err.to_string().contains("empty or whitespace-only"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_resolve_rejects_empty_env() {
        let err = resolve_token_from_sources(None, Some("".to_string())).unwrap_err();
        assert!(
            err.to_string().contains("empty or whitespace-only"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_resolve_trims_whitespace_padding() {
        let result = resolve_token_from_sources(Some("  abc  "), None).unwrap();
        assert_eq!(result.unwrap().as_str(), "abc");
    }

    // --- Header injection integration tests ---
    //
    // AgentPlatform creates its own tokio runtime internally, so these tests
    // must be sync (#[test]) to avoid nested-runtime panics. The axum test
    // server runs on a dedicated thread with its own runtime.

    #[derive(Clone, Default)]
    struct CapturedHeaders(Arc<Mutex<Option<HeaderMap>>>);

    #[derive(Serialize, Deserialize)]
    struct Echo {
        ok: bool,
    }

    async fn capture_handler(
        State(captured): State<CapturedHeaders>,
        headers: HeaderMap,
    ) -> axum::Json<Echo> {
        *captured.0.lock().await = Some(headers);
        axum::Json(Echo { ok: true })
    }

    /// Start a test server on a background thread, return (url, captured_headers).
    fn start_test_server() -> (String, CapturedHeaders) {
        let captured = CapturedHeaders::default();
        let captured_clone = captured.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let app = Router::new()
                    .route("/test", post(capture_handler))
                    .with_state(captured_clone);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr.port()).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });

        let port = rx.recv().unwrap();
        (format!("http://127.0.0.1:{port}/test"), captured)
    }

    #[test]
    fn test_header_injected_on_operator_request() {
        let (url, captured) = start_test_server();
        let token = RunnerToken("test-secret-token".to_string());
        let platform = AgentPlatform::new(Some(token)).unwrap();

        let payload = serde_json::json!({"hello": "world"});
        let _result: Echo = platform.post_json(&url, &payload, "Test").unwrap();

        // The server thread's runtime owns the Mutex; use a temporary runtime to read it.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let headers = rt.block_on(async { captured.0.lock().await.clone() });
        let headers = headers.expect("headers should be captured");
        assert_eq!(
            headers.get("X-Runner-Token").map(|v| v.to_str().unwrap()),
            Some("test-secret-token")
        );
    }

    #[test]
    fn test_no_header_when_token_absent() {
        let (url, captured) = start_test_server();
        let platform = AgentPlatform::new(None).unwrap();

        let payload = serde_json::json!({"hello": "world"});
        let _result: Echo = platform.post_json(&url, &payload, "Test").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let headers = rt.block_on(async { captured.0.lock().await.clone() });
        let headers = headers.expect("headers should be captured");
        assert!(headers.get("X-Runner-Token").is_none());
    }
}

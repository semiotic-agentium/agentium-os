mod spans;

use baml_rt_llm_config::FnoxFileSecretResolver;
use tracing::Instrument;

/// ClickUp v2 REST API base URL.
pub const BASE_URL: &str = "https://api.clickup.com/api/v2";

#[derive(Debug, thiserror::Error)]
pub enum ClickUpClientError {
    #[error("ClickUp HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("ClickUp API authentication failed (401): {body}")]
    Unauthorized { body: String },

    #[error("ClickUp resource not found (404): {body}")]
    NotFound { body: String },

    #[error("ClickUp rate limit exceeded (429), resets at {reset_at}: {body}")]
    RateLimited { body: String, reset_at: String },

    #[error("ClickUp API returned {status}: {body}")]
    Api { status: u16, body: String },

    #[error(
        "CLICKUP_API_KEY not resolved from fnox (BAML_FNOX_CONFIG / fnox.toml) or process environment"
    )]
    MissingApiKey,
}

#[derive(Clone)]
pub struct ClickUpClient {
    client: reqwest::Client,
    /// When `None`, [`Self::resolve_api_key`] reads fnox / env when needed (deploy-time tool
    /// registration does not require credentials).
    explicit_api_key: Option<String>,
    base_url: String,
}

impl Default for ClickUpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickUpClient {
    /// HTTP client with default production base URL; resolves `CLICKUP_API_KEY` from fnox /
    /// environment on [`Self::resolve_api_key`], unless [`Self::with_credentials`] supplied a key.
    pub fn new() -> Self {
        Self::with_base_url(resolve_base_url())
    }

    /// Same as [`Self::new`] but targets a fixed API base (e.g. local fixture server). Trailing
    /// slashes are stripped.
    pub fn with_base_url(base: impl Into<String>) -> Self {
        let raw = base.into();
        Self {
            client: reqwest::Client::new(),
            explicit_api_key: None,
            base_url: raw.trim().trim_end_matches('/').to_string(),
        }
    }

    /// Builds a client with both an explicit API key and base URL. Skips `fnox.toml` /
    /// environment lookup entirely; useful for fixture tests and callers that resolve
    /// credentials themselves.
    pub fn with_credentials(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            explicit_api_key: Some(api_key.into()),
            base_url: base_url.into(),
        }
    }

    /// Returns the configured API key, or resolves `CLICKUP_API_KEY` from fnox.toml / process env.
    pub fn resolve_api_key(&self) -> std::result::Result<String, ClickUpClientError> {
        if let Some(key) = &self.explicit_api_key {
            return Ok(key.clone());
        }
        FnoxFileSecretResolver::default_path_resolver()
            .resolve_or_env("CLICKUP_API_KEY")
            .ok_or(ClickUpClientError::MissingApiKey)
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub fn get(&self, path: &str, api_key: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{}", self.base_url(), path))
            .header("Authorization", api_key)
    }

    pub fn post(&self, path: &str, api_key: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{}", self.base_url(), path))
            .header("Authorization", api_key)
    }

    pub fn put(&self, path: &str, api_key: &str) -> reqwest::RequestBuilder {
        self.client
            .put(format!("{}{}", self.base_url(), path))
            .header("Authorization", api_key)
    }

    pub fn delete(&self, path: &str, api_key: &str) -> reqwest::RequestBuilder {
        self.client
            .delete(format!("{}{}", self.base_url(), path))
            .header("Authorization", api_key)
    }

    pub async fn send_json(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, ClickUpClientError> {
        let span = spans::send_json();
        if let Some(rb) = request.try_clone()
            && let Ok(req) = rb.build()
        {
            span.record("url", tracing::field::display(req.url().as_str()));
        }

        async move {
            let resp = request.send().await.map_err(ClickUpClientError::Http)?;

            let status = resp.status();
            if !status.is_success() {
                let code = status.as_u16();
                let reset_at = resp
                    .headers()
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown")
                    .to_string();
                let body = resp.text().await.unwrap_or_default();

                let is_fake_auth_error = code == 401
                    && body.contains("OAUTH_0")
                    && (body.contains("token not found") || body.contains("Token not found"));

                return Err(match code {
                    401 if is_fake_auth_error => ClickUpClientError::NotFound {
                        body: format!("Resource not found : {body}"),
                    },
                    401 => ClickUpClientError::Unauthorized { body },
                    404 => ClickUpClientError::NotFound { body },
                    429 => ClickUpClientError::RateLimited { body, reset_at },
                    _ => ClickUpClientError::Api { status: code, body },
                });
            }

            resp.json().await.map_err(ClickUpClientError::Http)
        }
        .instrument(span)
        .await
    }

    pub async fn send_no_content(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<(), ClickUpClientError> {
        let resp = request.send().await.map_err(ClickUpClientError::Http)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let code = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        Err(match code {
            401 => ClickUpClientError::Unauthorized { body },
            404 => ClickUpClientError::NotFound { body },
            429 => ClickUpClientError::RateLimited {
                body,
                reset_at: "unknown".to_string(),
            },
            _ => ClickUpClientError::Api { status: code, body },
        })
    }
}

pub fn resolve_base_url() -> String {
    std::env::var("CLICKUP_API_BASE_URL")
        .ok()
        .map(|raw| raw.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| BASE_URL.to_string())
}

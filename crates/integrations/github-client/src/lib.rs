// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

mod spans;

use baml_rt_llm_config::FnoxFileSecretResolver;
use tracing::Instrument;

/// GitHub REST API v3 base URL.
pub const BASE_URL: &str = "https://api.github.com";

#[derive(Debug, thiserror::Error)]
pub enum GitHubClientError {
    #[error("GitHub HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("GitHub API authentication failed (401): {body}")]
    Unauthorized { body: String },

    #[error("GitHub resource not found (404): {body}")]
    NotFound { body: String },

    #[error("GitHub rate limit exceeded (403), resets at {reset_at}: {body}")]
    RateLimited { body: String, reset_at: String },

    #[error("GitHub validation failed (422): {body}")]
    Unprocessable { body: String },

    #[error("GitHub API returned {status}: {body}")]
    Api { status: u16, body: String },

    #[error(
        "GITHUB_TOKEN not resolved from fnox (BAML_FNOX_CONFIG / fnox.toml) or process environment"
    )]
    MissingToken,
}

#[derive(Clone)]
pub struct GitHubClient {
    client: reqwest::Client,
    token: String,
    base_url: String,
}

impl GitHubClient {
    /// Resolves `GITHUB_TOKEN` from `fnox.toml` / process environment and caches it on the
    /// client so subsequent operations reuse the resolved value.
    pub fn new() -> std::result::Result<Self, GitHubClientError> {
        let token = FnoxFileSecretResolver::default_path_resolver()
            .resolve_or_env("GITHUB_TOKEN")
            .ok_or(GitHubClientError::MissingToken)?;
        Ok(Self::with_credentials(token, resolve_base_url()))
    }

    /// Builds a client with both an explicit token and base URL. Skips `fnox.toml` /
    /// environment lookup entirely; useful for fixture tests and callers that resolve
    /// credentials themselves.
    pub fn with_credentials(token: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.into(),
            base_url: base_url.into(),
        }
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub fn token(&self) -> &str {
        self.token.as_str()
    }

    pub fn get(&self, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{base}{path}", base = self.base_url()))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "baml-agent-platform")
    }

    pub fn post(&self, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{base}{path}", base = self.base_url()))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "baml-agent-platform")
    }

    pub async fn send_json(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, GitHubClientError> {
        let span = spans::send_json();
        if let Some(rb) = request.try_clone()
            && let Ok(req) = rb.build()
        {
            span.record("url", tracing::field::display(req.url().as_str()));
        }

        async move {
            let resp = request.send().await.map_err(GitHubClientError::Http)?;

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

                return Err(match code {
                    401 => GitHubClientError::Unauthorized { body },
                    403 if body.contains("rate limit") => {
                        GitHubClientError::RateLimited { body, reset_at }
                    }
                    404 => GitHubClientError::NotFound { body },
                    422 => GitHubClientError::Unprocessable { body },
                    _ => GitHubClientError::Api { status: code, body },
                });
            }

            resp.json().await.map_err(GitHubClientError::Http)
        }
        .instrument(span)
        .await
    }
}

pub fn resolve_base_url() -> String {
    std::env::var("GITHUB_API_BASE_URL")
        .ok()
        .map(|raw| raw.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| BASE_URL.to_string())
}

mod spans;

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

    #[error("GITHUB_TOKEN environment variable not set")]
    MissingToken(#[source] std::env::VarError),
}

#[derive(Clone)]
pub struct GitHubClient {
    client: reqwest::Client,
    base_url: String,
}

impl Default for GitHubClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: resolve_base_url(),
        }
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub fn token() -> std::result::Result<String, GitHubClientError> {
        std::env::var("GITHUB_TOKEN").map_err(GitHubClientError::MissingToken)
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
        let _guard = span.enter();
        if let Some(rb) = request.try_clone()
            && let Ok(req) = rb.build()
        {
            span.record("url", tracing::field::display(req.url().as_str()));
        }

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
}

pub fn resolve_base_url() -> String {
    std::env::var("GITHUB_API_BASE_URL")
        .ok()
        .map(|raw| raw.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| BASE_URL.to_string())
}

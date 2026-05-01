use baml_rt_core::backoff::{MAX_RATE_LIMIT_RETRIES, rate_limit_backoff_delay};
pub use baml_rt_core::retry_after::{RetryAfter, parse_retry_after};
use baml_rt_llm_config::FnoxFileSecretResolver;

/// Notion REST API base URL.
pub const BASE_URL: &str = "https://api.notion.com/v1";
/// Notion API version header value.
pub const NOTION_VERSION: &str = "2022-06-28";

#[derive(Debug, thiserror::Error)]
pub enum NotionReadError {
    #[error("Notion HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("Notion API authentication failed ({status}): {body}")]
    Unauthorized {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Notion resource not found (404): {body}")]
    NotFound { body: String },

    #[error("Notion rate limit exceeded (429), retry after {retry_after}: {body}")]
    RateLimited {
        body: String,
        retry_after: RetryAfter,
    },

    #[error("Notion API returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Failed to deserialize Notion response")]
    Deserialize(#[source] reqwest::Error),

    #[error(
        "NOTION_API_TOKEN not resolved from fnox (BAML_FNOX_CONFIG / fnox.toml) or process environment"
    )]
    MissingApiKey,

    #[error("Invalid Notion id '{id}'")]
    InvalidId { id: String },

    #[error("Invalid Notion header value: {message}")]
    InvalidHeader { message: String },

    #[error("Failed to clone Notion request")]
    RequestClone,
}

#[derive(Clone)]
pub struct NotionReadClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl NotionReadClient {
    pub fn new() -> std::result::Result<Self, NotionReadError> {
        let api_key = FnoxFileSecretResolver::default_path_resolver()
            .resolve_or_env("NOTION_API_TOKEN")
            .ok_or(NotionReadError::MissingApiKey)?;
        let base_url = resolve_base_url();
        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
            base_url,
        })
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub fn api_key(&self) -> &str {
        self.api_key.as_str()
    }

    pub fn normalize_id(id: &str) -> std::result::Result<String, NotionReadError> {
        let cleaned: String = id.chars().filter(|c| *c != '-').collect();
        if cleaned.len() != 32 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(NotionReadError::InvalidId { id: id.to_string() });
        }
        Ok(format!(
            "{}-{}-{}-{}-{}",
            &cleaned[0..8],
            &cleaned[8..12],
            &cleaned[12..16],
            &cleaned[16..20],
            &cleaned[20..32]
        ))
    }

    pub fn auth_headers(
        &self,
        api_key: &str,
    ) -> std::result::Result<reqwest::header::HeaderMap, NotionReadError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {api_key}")
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|e| NotionReadError::InvalidHeader {
                    message: e.to_string(),
                })?,
        );
        headers.insert(
            "Notion-Version",
            NOTION_VERSION
                .parse::<reqwest::header::HeaderValue>()
                .map_err(|e| NotionReadError::InvalidHeader {
                    message: e.to_string(),
                })?,
        );
        Ok(headers)
    }

    pub fn get(
        &self,
        path: &str,
        api_key: &str,
    ) -> std::result::Result<reqwest::RequestBuilder, NotionReadError> {
        Ok(self
            .client
            .get(format!("{}{}", self.base_url(), path))
            .headers(self.auth_headers(api_key)?))
    }

    pub fn post(
        &self,
        path: &str,
        api_key: &str,
    ) -> std::result::Result<reqwest::RequestBuilder, NotionReadError> {
        Ok(self
            .client
            .post(format!("{}{}", self.base_url(), path))
            .headers(self.auth_headers(api_key)?))
    }

    pub async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, NotionReadError> {
        let request = request.build().map_err(NotionReadError::Http)?;
        let mut retries: u32 = 0;

        loop {
            let req = request.try_clone().ok_or(NotionReadError::RequestClone)?;
            let resp = self
                .client
                .execute(req)
                .await
                .map_err(NotionReadError::Http)?;

            let status = resp.status();
            if !status.is_success() {
                let retry_after =
                    parse_retry_after(resp.headers().get("retry-after").map(|v| v.as_bytes()));
                let body = resp.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    && retries < MAX_RATE_LIMIT_RETRIES
                {
                    let delay = retry_after
                        .as_duration()
                        .unwrap_or_else(|| rate_limit_backoff_delay(retries));
                    tracing::warn!(
                        retries = retries + 1,
                        retry_after = %retry_after,
                        "Notion rate limit hit; backing off"
                    );
                    retries += 1;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(match status {
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                        NotionReadError::Unauthorized { status, body }
                    }
                    reqwest::StatusCode::NOT_FOUND => NotionReadError::NotFound { body },
                    reqwest::StatusCode::TOO_MANY_REQUESTS => {
                        NotionReadError::RateLimited { body, retry_after }
                    }
                    _ => NotionReadError::Api { status, body },
                });
            }

            return resp.json().await.map_err(NotionReadError::Deserialize);
        }
    }
}

pub fn resolve_base_url() -> String {
    let override_url = std::env::var("NOTION_API_BASE_URL")
        .ok()
        .map(|raw| raw.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty());
    if let Some(base_url) = override_url {
        if !cfg!(test) && should_warn_on_insecure_base_url(&base_url) {
            tracing::warn!(
                base_url = %base_url,
                "NOTION_API_BASE_URL is not https; bearer token may be sent to an insecure endpoint"
            );
        }
        return base_url;
    }
    BASE_URL.to_string()
}

pub fn should_warn_on_insecure_base_url(base_url: &str) -> bool {
    if base_url.starts_with("https://") {
        return false;
    }
    if let Ok(parsed) = reqwest::Url::parse(base_url)
        && let Some(host) = parsed.host_str()
    {
        let normalized = host.to_ascii_lowercase();
        if normalized == "localhost" || normalized == "127.0.0.1" || normalized == "::1" {
            return false;
        }
    }
    true
}

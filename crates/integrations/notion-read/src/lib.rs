use std::time::Duration;

/// Notion REST API base URL.
pub const BASE_URL: &str = "https://api.notion.com/v1";
/// Notion API version header value.
pub const NOTION_VERSION: &str = "2022-06-28";

const MAX_RATE_LIMIT_RETRIES: usize = 3;
const RATE_LIMIT_BASE_DELAY_MS: u64 = 500;
const RATE_LIMIT_MAX_DELAY_MS: u64 = 5_000;

fn backoff_delay(retries: usize) -> Duration {
    let shift = u32::try_from(retries).unwrap_or(u32::MAX);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let backoff = RATE_LIMIT_BASE_DELAY_MS.saturating_mul(multiplier);
    Duration::from_millis(backoff.min(RATE_LIMIT_MAX_DELAY_MS))
}

#[derive(Debug, Clone)]
pub enum RetryAfter {
    Seconds(u64),
    Unknown(String),
    Missing,
}

impl RetryAfter {
    pub fn as_duration(&self) -> Option<Duration> {
        match self {
            RetryAfter::Seconds(seconds) => Some(Duration::from_secs(*seconds)),
            RetryAfter::Unknown(_) | RetryAfter::Missing => None,
        }
    }
}

impl std::fmt::Display for RetryAfter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetryAfter::Seconds(seconds) => write!(f, "{seconds}s"),
            RetryAfter::Unknown(raw) => write!(f, "unknown({raw})"),
            RetryAfter::Missing => write!(f, "missing"),
        }
    }
}

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

    #[error("NOTION_API_TOKEN environment variable not set")]
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
    api_key: Option<String>,
    base_url: String,
}

impl Default for NotionReadClient {
    fn default() -> Self {
        Self::new()
    }
}

impl NotionReadClient {
    pub fn new() -> Self {
        let api_key = std::env::var("NOTION_API_TOKEN").ok();
        let base_url = resolve_base_url();
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url,
        }
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub fn api_key(&self) -> std::result::Result<&str, NotionReadError> {
        self.api_key
            .as_deref()
            .ok_or(NotionReadError::MissingApiKey)
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
        let mut retries = 0usize;

        loop {
            let req = request.try_clone().ok_or(NotionReadError::RequestClone)?;
            let resp = self
                .client
                .execute(req)
                .await
                .map_err(NotionReadError::Http)?;

            let status = resp.status();
            if !status.is_success() {
                let retry_after = parse_retry_after(resp.headers().get("retry-after"));
                let body = resp.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    && retries < MAX_RATE_LIMIT_RETRIES
                {
                    let delay = retry_after
                        .as_duration()
                        .unwrap_or_else(|| backoff_delay(retries));
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

pub fn parse_retry_after(value: Option<&reqwest::header::HeaderValue>) -> RetryAfter {
    let Some(value) = value else {
        return RetryAfter::Missing;
    };
    let raw = match value.to_str() {
        Ok(raw) => raw,
        Err(_) => return RetryAfter::Unknown("invalid-utf8".to_string()),
    };
    match raw.trim().parse::<u64>() {
        Ok(seconds) => RetryAfter::Seconds(seconds),
        Err(_) => RetryAfter::Unknown(raw.to_string()),
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

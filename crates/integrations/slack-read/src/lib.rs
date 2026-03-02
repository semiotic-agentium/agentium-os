use std::time::Duration;

pub const BASE_URL: &str = "https://slack.com/api";

const MAX_RATE_LIMIT_RETRIES: usize = 3;
const RATE_LIMIT_BASE_DELAY_MS: u64 = 500;
const RATE_LIMIT_MAX_DELAY_MS: u64 = 5_000;

fn backoff_delay(retries: usize) -> Duration {
    let shift = u32::try_from(retries).unwrap_or(u32::MAX);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let backoff = RATE_LIMIT_BASE_DELAY_MS.saturating_mul(multiplier);
    Duration::from_millis(backoff.min(RATE_LIMIT_MAX_DELAY_MS))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SlackAuthPreference {
    #[default]
    Auto,
    Bot,
    User,
}

#[derive(Debug, Clone, Copy)]
pub enum SlackTokenKind {
    Bot,
    User,
}

#[derive(Debug, Clone, Copy)]
pub enum SlackApiErrorClass {
    Configuration,
    InvalidArgument,
    ToolExecution,
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
pub enum SlackReadError {
    #[error("Slack HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("Slack API authentication failed ({status}): {body}")]
    Unauthorized {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Slack rate limit exceeded (429), retry after {retry_after}: {body}")]
    RateLimited {
        body: String,
        retry_after: RetryAfter,
    },

    #[error("Slack API returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("Slack API {method} failed: {error}")]
    ApiError {
        method: &'static str,
        error: String,
        class: SlackApiErrorClass,
    },

    #[error("Unexpected Slack response shape: {message}")]
    UnexpectedShape { message: String },

    #[error("{message}")]
    MissingToken { message: String },

    #[error("Invalid Slack header value: {message}")]
    InvalidHeader { message: String },

    #[error("Failed to clone Slack request")]
    RequestClone,
}

#[derive(Debug, Clone)]
pub struct SlackAuthConfig {
    pub bot_token: Option<String>,
    pub user_token: Option<String>,
}

impl SlackAuthConfig {
    pub fn from_env() -> Self {
        Self {
            bot_token: std::env::var("SLACK_BOT_TOKEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            user_token: std::env::var("SLACK_USER_TOKEN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        }
    }

    pub fn select_token(
        &self,
        preference: Option<SlackAuthPreference>,
        requires_user: bool,
    ) -> std::result::Result<(&str, SlackTokenKind), SlackReadError> {
        let preference = preference.unwrap_or_default();
        if requires_user {
            if preference == SlackAuthPreference::Bot {
                return Err(SlackReadError::MissingToken {
                    message: "Search requires a user token; auth=bot is not supported".to_string(),
                });
            }
            return self
                .user_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::User))
                .ok_or_else(|| SlackReadError::MissingToken {
                    message: "SLACK_USER_TOKEN environment variable is required for message search"
                        .to_string(),
                });
        }

        match preference {
            SlackAuthPreference::Bot => self
                .bot_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::Bot))
                .ok_or_else(|| SlackReadError::MissingToken {
                    message: "SLACK_BOT_TOKEN environment variable is required when auth=bot"
                        .to_string(),
                }),
            SlackAuthPreference::User => self
                .user_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::User))
                .ok_or_else(|| SlackReadError::MissingToken {
                    message: "SLACK_USER_TOKEN environment variable is required when auth=user"
                        .to_string(),
                }),
            SlackAuthPreference::Auto => self
                .bot_token
                .as_deref()
                .map(|token| (token, SlackTokenKind::Bot))
                .or_else(|| {
                    self.user_token
                        .as_deref()
                        .map(|token| (token, SlackTokenKind::User))
                })
                .ok_or_else(|| SlackReadError::MissingToken {
                    message: "Set SLACK_BOT_TOKEN (preferred) or SLACK_USER_TOKEN".to_string(),
                }),
        }
    }
}

#[derive(Clone)]
pub struct SlackReadClient {
    client: reqwest::Client,
    auth: SlackAuthConfig,
    base_url: String,
}

impl Default for SlackReadClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SlackReadClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            auth: SlackAuthConfig::from_env(),
            base_url: resolve_base_url(),
        }
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub fn auth(&self) -> &SlackAuthConfig {
        &self.auth
    }

    pub fn select_token(
        &self,
        preference: Option<SlackAuthPreference>,
        requires_user: bool,
    ) -> std::result::Result<(&str, SlackTokenKind), SlackReadError> {
        self.auth.select_token(preference, requires_user)
    }

    pub async fn get_json(
        &self,
        method_name: &'static str,
        token: &str,
        query: &[(&str, String)],
    ) -> std::result::Result<serde_json::Value, SlackReadError> {
        let mut request = self.authorized_get(token, method_name)?;
        if !query.is_empty() {
            request = request.query(query);
        }
        self.send_request(method_name, request).await
    }

    pub fn authorized_get(
        &self,
        token: &str,
        method: &'static str,
    ) -> std::result::Result<reqwest::RequestBuilder, SlackReadError> {
        let endpoint = format!("{}/{}", self.base_url(), method);
        let authorization = format!("Bearer {token}")
            .parse::<reqwest::header::HeaderValue>()
            .map_err(|e| SlackReadError::InvalidHeader {
                message: e.to_string(),
            })?;
        Ok(self
            .client
            .get(endpoint)
            .header(reqwest::header::AUTHORIZATION, authorization))
    }

    pub async fn send_request(
        &self,
        method_name: &'static str,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, SlackReadError> {
        let request = request.build().map_err(SlackReadError::Http)?;
        let mut retries = 0usize;
        loop {
            let req = request.try_clone().ok_or(SlackReadError::RequestClone)?;
            let resp = self
                .client
                .execute(req)
                .await
                .map_err(SlackReadError::Http)?;
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = parse_retry_after(resp.headers().get("retry-after"));
                let body = resp.text().await.unwrap_or_default();
                if retries < MAX_RATE_LIMIT_RETRIES {
                    let delay = retry_after
                        .as_duration()
                        .unwrap_or_else(|| backoff_delay(retries));
                    tracing::warn!(
                        method = method_name,
                        retries = retries + 1,
                        retry_after = %retry_after,
                        "Slack rate limit hit; backing off"
                    );
                    retries += 1;
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(SlackReadError::RateLimited { body, retry_after });
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(match status {
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                        SlackReadError::Unauthorized { status, body }
                    }
                    _ => SlackReadError::Api { status, body },
                });
            }

            let json: serde_json::Value = resp.json().await.map_err(SlackReadError::Http)?;
            let ok = json.get("ok").and_then(serde_json::Value::as_bool);
            match ok {
                Some(true) => return Ok(json),
                Some(false) => {
                    let code = json
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown_error");
                    return Err(map_slack_api_error(method_name, code.to_string()));
                }
                None => {
                    return Err(SlackReadError::UnexpectedShape {
                        message: format!("missing 'ok' field for {method_name}"),
                    });
                }
            }
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
    let override_url = std::env::var("SLACK_API_BASE_URL")
        .ok()
        .map(|raw| raw.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty());
    if let Some(base_url) = override_url {
        if !cfg!(test) && should_warn_on_insecure_base_url(&base_url) {
            tracing::warn!(
                base_url = %base_url,
                "SLACK_API_BASE_URL is not https; bearer token may be sent to an insecure endpoint"
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

pub fn map_slack_api_error(method: &'static str, error: String) -> SlackReadError {
    let class = if is_auth_error_code(&error) {
        SlackApiErrorClass::Configuration
    } else if is_invalid_argument_error_code(&error) {
        SlackApiErrorClass::InvalidArgument
    } else {
        SlackApiErrorClass::ToolExecution
    };
    SlackReadError::ApiError {
        method,
        error,
        class,
    }
}

pub fn is_auth_error_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_auth"
            | "not_authed"
            | "account_inactive"
            | "token_revoked"
            | "missing_scope"
            | "not_allowed_token_type"
    )
}

pub fn is_invalid_argument_error_code(code: &str) -> bool {
    matches!(
        code,
        "channel_not_found"
            | "thread_not_found"
            | "user_not_found"
            | "message_not_found"
            | "messages_not_found"
            | "invalid_cursor"
            | "invalid_ts_latest"
            | "invalid_ts_oldest"
            | "invalid_args"
            | "invalid_arguments"
            | "invalid_arg_name"
            | "invalid_array_arg"
            | "invalid_query"
            | "not_in_channel"
    )
}

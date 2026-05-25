//! `support/slack_notify` — write-only Slack notification tool.
//!
//! Posts incident summaries to one configured Slack channel. The model controls
//! only `text` and `context_id`; channel and thread routing are host-owned.

use std::{collections::HashMap, sync::OnceLock};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_llm_config::FnoxFileSecretResolver;
use baml_rt_tools::{ActionIdentity, baml_tool, bundles::Support, tools::BamlTool};
use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

const DEFAULT_BASE_URL: &str = "https://slack.com/api";
const CHANNEL_ENV: &str = "SLACK_NOTIFY_CHANNEL_ID";
const BOT_TOKEN_ENV: &str = "SLACK_BOT_TOKEN";

static THREADS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static CHANNEL_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn thread_map() -> &'static Mutex<HashMap<String, String>> {
    THREADS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn channel_cache() -> &'static Mutex<HashMap<String, String>> {
    CHANNEL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// LLM-visible input. Channel and thread are intentionally absent.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct SlackNotifyInput {
    #[baml(description = "Message body to post to the configured Slack channel.")]
    pub text: String,
    #[baml(description = "Agentium context_id; used by the tool to derive Slack threading.")]
    pub context_id: String,
}

impl baml_rt_tools::DescribeAction for SlackNotifyInput {
    fn describe(&self) -> String {
        format!("posting Slack notification for context {}", self.context_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct SlackNotifyOutput {
    pub channel_id: String,
    pub ts: String,
    pub thread_ts: String,
    pub context_id: String,
    pub posted: bool,
}

#[derive(Debug, Clone)]
struct SlackNotifyConfig {
    channel: String,
    token: String,
    base_url: String,
}

impl SlackNotifyConfig {
    fn from_env() -> Result<Self> {
        let resolver = FnoxFileSecretResolver::default_path_resolver();
        let channel = std::env::var(CHANNEL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "{CHANNEL_ENV} must be set to a Slack channel ID (C…); #name fallback is developer-only"
                ))
            })?;
        let token = resolver.resolve_or_env(BOT_TOKEN_ENV).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "{BOT_TOKEN_ENV} not resolved from fnox (BAML_FNOX_CONFIG / fnox.toml) or process environment"
            ))
        })?;
        let base_url = std::env::var("SLACK_NOTIFY_API_BASE_URL")
            .or_else(|_| std::env::var("SLACK_API_BASE_URL"))
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            channel,
            token,
            base_url,
        })
    }
}

#[derive(Clone)]
pub struct SlackNotifyClient {
    http: reqwest::Client,
}

impl Default for SlackNotifyClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SlackNotifyClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    async fn post(&self, input: SlackNotifyInput) -> Result<SlackNotifyOutput> {
        if input.text.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "Slack notification text must not be empty".to_string(),
            ));
        }
        if input.context_id.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "Slack notification context_id must not be empty".to_string(),
            ));
        }

        let mut config = SlackNotifyConfig::from_env()?;
        if config.channel.starts_with('#') {
            config.channel = self.resolve_channel_name_cached(&config).await?;
        } else if !looks_like_channel_id(&config.channel) {
            return Err(BamlRtError::InvalidArgument(format!(
                "{CHANNEL_ENV} must be a Slack channel ID (C…, G…, D…) or #name for developer fallback"
            )));
        }

        let existing_thread = {
            let threads = thread_map().lock().await;
            threads.get(input.context_id.as_str()).cloned()
        };

        let mut body = json!({
            "channel": config.channel,
            "text": input.text,
        });
        if let Some(thread_ts) = &existing_thread {
            body["thread_ts"] = json!(thread_ts);
        }

        let response = self
            .http
            .post(format!("{}/chat.postMessage", config.base_url))
            .bearer_auth(&config.token)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await
            .map_err(|source| {
                BamlRtError::ToolExecution(format!("Slack HTTP request failed: {source}"))
            })?;

        let value = slack_json_response(response, "chat.postMessage").await?;
        let ts = value
            .get("ts")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BamlRtError::BamlRuntime("Slack chat.postMessage response missing ts".to_string())
            })?
            .to_string();
        let channel_id = value
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or(config.channel.as_str())
            .to_string();
        let thread_ts = existing_thread.unwrap_or_else(|| ts.clone());

        {
            let mut threads = thread_map().lock().await;
            threads
                .entry(input.context_id.clone())
                .or_insert_with(|| thread_ts.clone());
        }

        Ok(SlackNotifyOutput {
            channel_id,
            ts,
            thread_ts,
            context_id: input.context_id,
            posted: true,
        })
    }

    async fn resolve_channel_name_cached(&self, config: &SlackNotifyConfig) -> Result<String> {
        if let Some(channel_id) = channel_cache().lock().await.get(&config.channel).cloned() {
            return Ok(channel_id);
        }
        let channel_id = self.resolve_channel_name(config).await?;
        channel_cache()
            .lock()
            .await
            .insert(config.channel.clone(), channel_id.clone());
        Ok(channel_id)
    }

    async fn resolve_channel_name(&self, config: &SlackNotifyConfig) -> Result<String> {
        let target = config.channel.trim_start_matches('#');
        let response = self
            .http
            .get(format!("{}/conversations.list", config.base_url))
            .bearer_auth(&config.token)
            .query(&[
                ("types", "public_channel,private_channel"),
                ("limit", "1000"),
            ])
            .send()
            .await
            .map_err(|source| {
                BamlRtError::ToolExecution(format!("Slack HTTP request failed: {source}"))
            })?;
        let value = slack_json_response(response, "conversations.list").await?;
        let channels = value
            .get("channels")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BamlRtError::BamlRuntime(
                    "Slack conversations.list response missing channels".to_string(),
                )
            })?;
        channels
            .iter()
            .find_map(|channel| {
                let name = channel.get("name")?.as_str()?;
                (name == target).then(|| channel.get("id")?.as_str().map(ToString::to_string))?
            })
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "failed to resolve {CHANNEL_ENV}={} to a Slack channel ID",
                    config.channel
                ))
            })
    }
}

async fn slack_json_response(response: reqwest::Response, method: &'static str) -> Result<Value> {
    let status = response.status();
    let body = response.text().await.map_err(|source| {
        BamlRtError::ToolExecution(format!("Slack response read failed: {source}"))
    })?;
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(BamlRtError::ToolExecution(format!(
            "Slack API rate limited {method}: {body}"
        )));
    }
    if !status.is_success() {
        return Err(BamlRtError::ToolExecution(format!(
            "Slack API {method} returned {status}: {body}"
        )));
    }
    let value: Value = serde_json::from_str(&body)?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        return Err(BamlRtError::ToolExecution(format!(
            "Slack API {method} failed: {error}"
        )));
    }
    Ok(value)
}

fn looks_like_channel_id(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'C' | b'G' | b'D')) && value.len() >= 2
}

#[derive(Default)]
pub struct SlackNotifyTool {
    client: SlackNotifyClient,
}

impl SlackNotifyTool {
    pub fn new() -> Self {
        Self {
            client: SlackNotifyClient::new(),
        }
    }
}

#[baml_tool(
    name = "support/slack_notify",
    description = "Write-only Slack notification tool. Posts to host-configured channel; input cannot set channel or thread.",
    tags = ["support", "slack", "notify", "write"],
    access = Write,
    secrets = [
        { name = "SLACK_BOT_TOKEN", description = "Slack bot token (xoxb-...) with chat:write scope", reason = "Required to post notification messages" },
        { name = "SLACK_NOTIFY_CHANNEL_ID", description = "Slack channel ID (C..., G..., or D...) used for all notifications", reason = "Host-controlled destination for notifications" },
    ],
    baml_types = [SlackNotifyInput, SlackNotifyOutput],
)]
#[async_trait]
impl BamlTool for SlackNotifyTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "slack_notify";
    type OpenInput = ();
    type Input = SlackNotifyInput;
    type Output = SlackNotifyOutput;

    fn description(&self) -> &'static str {
        "Posts Slack notifications to one configured channel. Channel and thread are host-controlled."
    }

    fn describe_open(&self) -> String {
        "opening Slack notify write tool".to_string()
    }

    fn action_identity(&self, input: &Self::Input) -> Option<ActionIdentity> {
        Some(ActionIdentity::new(
            None,
            vec![
                ("context_id", json!(input.context_id.clone())),
                ("destination", json!("configured Slack channel")),
            ],
        ))
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        self.client.post(args).await
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use axum::{
        Json, Router,
        extract::State,
        routing::{get, post},
    };
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn test_lock() -> &'static Mutex<()> {
        TEST_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[derive(Default)]
    struct Calls(Mutex<Vec<Value>>);

    async fn spawn_mock(calls: Arc<Calls>) -> String {
        async fn post_message(
            State(calls): State<Arc<Calls>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            calls.0.lock().await.push(body.clone());
            let thread_suffix = body
                .get("thread_ts")
                .and_then(Value::as_str)
                .unwrap_or("root");
            Json(
                json!({"ok": true, "channel": "C123ABC", "ts": format!("1700000000.{:06}", calls.0.lock().await.len()), "thread": thread_suffix}),
            )
        }
        async fn conversations() -> Json<Value> {
            Json(json!({"ok": true, "channels": [{"id": "C123ABC", "name": "ops"}]}))
        }
        let app = Router::new()
            .route("/chat.postMessage", post(post_message))
            .route("/conversations.list", get(conversations))
            .with_state(calls);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: String) -> Self {
            let old = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(old) = &self.old {
                    std::env::set_var(self.key, old);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn input_rejects_unknown_fields() {
        let err = serde_json::from_value::<SlackNotifyInput>(json!({
            "text": "hello",
            "context_id": "ctx-1",
            "channel": "COTHER"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[tokio::test]
    async fn posts_to_configured_channel_and_threads_by_context() {
        let _guard = test_lock().lock().await;
        thread_map().lock().await.clear();
        channel_cache().lock().await.clear();
        let calls = Arc::new(Calls::default());
        let base_url = spawn_mock(calls.clone()).await;
        let _base = EnvGuard::set("SLACK_NOTIFY_API_BASE_URL", base_url);
        let _token = EnvGuard::set(BOT_TOKEN_ENV, "xoxb-test".to_string());
        let _channel = EnvGuard::set(CHANNEL_ENV, "C123ABC".to_string());

        let tool = SlackNotifyTool::new();
        let first = tool
            .execute(SlackNotifyInput {
                text: "first".to_string(),
                context_id: "ctx-a".to_string(),
            })
            .await
            .unwrap();
        let second = tool
            .execute(SlackNotifyInput {
                text: "second".to_string(),
                context_id: "ctx-a".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(first.thread_ts, first.ts);
        assert_eq!(second.thread_ts, first.ts);
        let calls = calls.0.lock().await;
        assert_eq!(calls[0]["channel"], "C123ABC");
        assert!(calls[0].get("thread_ts").is_none());
        assert_eq!(calls[1]["channel"], "C123ABC");
        assert_eq!(calls[1]["thread_ts"], first.ts);
    }

    #[tokio::test]
    async fn resolves_channel_name_once_per_call_before_posting() {
        let _guard = test_lock().lock().await;
        thread_map().lock().await.clear();
        channel_cache().lock().await.clear();
        let calls = Arc::new(Calls::default());
        let base_url = spawn_mock(calls.clone()).await;
        let _base = EnvGuard::set("SLACK_NOTIFY_API_BASE_URL", base_url);
        let _token = EnvGuard::set(BOT_TOKEN_ENV, "xoxb-test".to_string());
        let _channel = EnvGuard::set(CHANNEL_ENV, "#ops".to_string());

        SlackNotifyTool::new()
            .execute(SlackNotifyInput {
                text: "hello".to_string(),
                context_id: "ctx-name".to_string(),
            })
            .await
            .unwrap();

        let calls = calls.0.lock().await;
        assert_eq!(calls[0]["channel"], "C123ABC");
    }
}

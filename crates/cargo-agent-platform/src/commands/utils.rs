use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};

pub const HTTP_OP_PUBLISH: &str = "Publish";
pub const HTTP_OP_DEPLOY: &str = "Deploy";
pub const HTTP_OP_UNDEPLOY: &str = "Undeploy";
pub const HTTP_OP_LIST_DEPLOYED_INSTANCES: &str = "List deployed instances";

pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

pub struct AgentPlatform {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}

pub fn build_http_client(connect_timeout: Option<Duration>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if let Some(timeout) = connect_timeout {
        builder = builder.connect_timeout(timeout);
    }
    builder.build().context("Failed to build HTTP client")
}

impl AgentPlatform {
    pub fn new() -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
        let client = build_http_client(None)?;
        Ok(Self { runtime, client })
    }

    pub fn post_json<Req, Resp>(&self, url: &str, payload: &Req, op_name: &str) -> Result<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        self.runtime.block_on(async {
            let resp = self
                .client
                .post(url)
                .header("content-type", "application/json")
                .json(payload)
                .send()
                .await
                .with_context(|| format!("Failed to POST {op_name} to {url}"))?;

            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                bail!("{op_name} failed ({status}) at {url}: {body}");
            }

            serde_json::from_str::<Resp>(&body)
                .with_context(|| format!("Failed to parse {op_name} response: {body}"))
        })
    }

    pub fn get_json<Resp>(&self, url: &str, op_name: &str) -> Result<Resp>
    where
        Resp: DeserializeOwned,
    {
        self.runtime.block_on(async {
            let resp = self
                .client
                .get(url)
                .send()
                .await
                .with_context(|| format!("Failed to GET {op_name} from {url}"))?;

            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                bail!("{op_name} failed ({status}) at {url}: {body}");
            }

            serde_json::from_str::<Resp>(&body)
                .with_context(|| format!("Failed to parse {op_name} response: {body}"))
        })
    }
}

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use baml_rt::a2a_types::{JSONRPCId, JSONRPCRequest, SendMessageRequest};
use baml_rt_core::{A2aRequestHandler, ids::ContextId};
use baml_rt_provenance::{
    GraphqliteProvenanceStore, ProvEvent, ProvenanceContextMessage, ProvenanceContextReader,
    ProvenanceConversationContextItem, ProvenanceWriter,
};
use serde_json::Value;
use tokio::sync::Semaphore;

pub fn init_test_tracing() {
    static TRACING: OnceLock<()> = OnceLock::new();
    TRACING.get_or_init(|| {
        baml_rt_observability::init_tracing();
    });
}

pub fn e2e_serial_gate() -> &'static Semaphore {
    init_test_tracing();
    static GATE: OnceLock<Semaphore> = OnceLock::new();
    GATE.get_or_init(|| Semaphore::new(1))
}

#[derive(Clone)]
pub struct StrictProvenanceWriter {
    inner: Arc<GraphqliteProvenanceStore>,
}

impl StrictProvenanceWriter {
    pub fn new(inner: Arc<GraphqliteProvenanceStore>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl ProvenanceWriter for StrictProvenanceWriter {
    async fn add_event(
        &self,
        event: ProvEvent,
    ) -> std::result::Result<(), baml_rt_provenance::ProvenanceError> {
        match self.inner.add_event(event.clone()).await {
            Ok(()) => Ok(()),
            Err(err) => panic!("strict provenance write failure: {err:?}; event={event:?}"),
        }
    }
}

#[async_trait]
impl ProvenanceContextReader for StrictProvenanceWriter {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> std::result::Result<Vec<ProvenanceContextMessage>, baml_rt_provenance::ProvenanceError>
    {
        self.inner.context_messages(context_id, limit).await
    }

    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> std::result::Result<
        Vec<ProvenanceConversationContextItem>,
        baml_rt_provenance::ProvenanceError,
    > {
        self.inner.conversation_context(context_id, limit).await
    }
}

pub fn build_agent_dir_to_temp(
    agent_dir: &Path,
    package_label: &str,
    builder_features: Option<&str>,
) -> PathBuf {
    if !agent_dir.exists() || !agent_dir.join("baml_src").exists() {
        panic!("Agent directory {} missing or invalid", agent_dir.display());
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tar_path =
        std::env::temp_dir().join(format!("runner-test-{package_label}-{unique}.tar.gz"));
    let extract_dir =
        std::env::temp_dir().join(format!("runner-test-{package_label}-extract-{unique}"));
    let _ = fs::remove_dir_all(&extract_dir);
    fs::create_dir_all(&extract_dir).expect("create extract dir");

    let mut cmd = std::process::Command::new("cargo");
    cmd.current_dir(test_support::common::workspace_root())
        .arg("run")
        .arg("--quiet")
        .arg("-p")
        .arg("baml-rt-builder");
    if let Some(features) = builder_features {
        cmd.arg("--features").arg(features);
    }
    cmd.arg("--bin")
        .arg("baml-agent-builder")
        .arg("--")
        .arg("package")
        .arg("--agent-dir")
        .arg(agent_dir)
        .arg("--output")
        .arg(&tar_path)
        .arg("--skip-lint");

    let output = cmd.output().expect("build agent: run builder");
    if !output.status.success() {
        panic!(
            "build agent {} failed: stdout={}, stderr={}",
            package_label,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let tar_gz = fs::File::open(&tar_path).expect("open built tar");
    let tar_dec = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar_dec);
    archive.unpack(&extract_dir).expect("unpack built tar");
    let _ = fs::remove_file(&tar_path);

    let dist_index = extract_dir.join("dist").join("index.js");
    assert!(
        dist_index.exists(),
        "Built package must contain dist/index.js"
    );
    extract_dir
}

#[allow(dead_code)]
pub fn jsonrpc_request(method: &str, params: serde_json::Value, id: &str) -> JSONRPCRequest {
    JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: method.to_string(),
        params: Some(params),
        id: Some(JSONRPCId::String(id.to_string())),
    }
}

#[allow(dead_code)]
pub fn send_message_request(params: SendMessageRequest, id: &str) -> JSONRPCRequest {
    jsonrpc_request(
        "message.sendStream",
        serde_json::to_value(params).expect("serialize SendMessageRequest"),
        id,
    )
}

#[allow(dead_code)]
pub async fn collect_stream_responses(
    agent: &baml_rt::A2aAgent,
    request: JSONRPCRequest,
) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(serde_json::to_value(request).expect("request json"))
        .await?;
    Ok(baml_rt::collect_a2a_stream_until(stream, |item| {
        let state = item
            .get("result")
            .and_then(|r| r.get("chunk"))
            .and_then(|c| c.get("task"))
            .and_then(|t| t.get("status"))
            .and_then(|s| s.get("state"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                item.get("result")
                    .and_then(|r| r.get("chunk"))
                    .and_then(|c| c.get("statusUpdate"))
                    .and_then(|s| s.get("status"))
                    .and_then(|s| s.get("state"))
                    .and_then(|v| v.as_str())
            });
        let is_final = item
            .get("result")
            .and_then(|r| r.get("final"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        is_final || matches!(state, Some("TASK_STATE_INPUT_REQUIRED"))
    })
    .await)
}

#[allow(dead_code)]
pub async fn build_clickup_agent_to_temp_async() -> PathBuf {
    let clickup_agent_dir = test_support::common::workspace_root()
        .join("agents")
        .join("clickup-agent");
    tokio::task::spawn_blocking(move || {
        build_agent_dir_to_temp(&clickup_agent_dir, "clickup-agent", Some("clickup"))
    })
    .await
    .expect("build clickup agent task join")
}

#[allow(dead_code)]
pub struct TempEnvVar {
    key: String,
    previous: Option<String>,
}

#[allow(dead_code)]
impl TempEnvVar {
    pub fn set(key: &str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key: key.to_string(),
            previous,
        }
    }
}

impl Drop for TempEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                std::env::set_var(&self.key, value);
            },
            None => unsafe {
                std::env::remove_var(&self.key);
            },
        }
    }
}

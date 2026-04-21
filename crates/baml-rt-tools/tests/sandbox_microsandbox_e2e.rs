#![cfg(feature = "sandbox-e2e-kvm")]

use std::{env, fs, path::Path, time::Duration};

use baml_rt_tools::external_tools::{
    ERR_INTERNAL, JsonRpcRequest, JsonRpcResponse, METHOD_INVOKE, SandboxImageRef,
    SandboxRuntimeSpec, ToolInvokeParams, ToolRuntime,
    metadata::ExternalToolMetadata,
    sandbox::{MicrosandboxProvider, SandboxProvider, SandboxSpec},
};
use serde_json::{Value, json};
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(120);
const PLACEHOLDER_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
async fn microsandbox_e2e_happy_teardown_reattach() {
    timeout(TEST_TIMEOUT, run_required_scenarios())
        .await
        .expect("sandbox_microsandbox_e2e timed out")
        .expect("sandbox_microsandbox_e2e failed");
}

async fn run_required_scenarios() -> Result<(), Box<dyn std::error::Error>> {
    if !Path::new("/dev/kvm").exists() {
        return Err(
            "sandbox-e2e-kvm feature enabled but /dev/kvm missing — run on a KVM-enabled host"
                .into(),
        );
    }

    let metadata = load_sandbox_metadata()?;
    let (image, entrypoint) = match metadata.runtime {
        Some(ToolRuntime::Sandbox(SandboxRuntimeSpec {
            image: SandboxImageRef::Oci { r#ref },
            entrypoint,
        })) => (r#ref, entrypoint),
        other => {
            return Err(format!("expected runtime.kind=sandbox in metadata, got {other:?}").into());
        }
    };

    let runtime_digest = metadata
        .runtime_digest
        .as_deref()
        .ok_or("metadata missing runtime_digest")?;
    if runtime_digest == PLACEHOLDER_DIGEST {
        return Err(
            "metadata resolves to placeholder digest; point BAML_SANDBOX_E2E_METADATA at CI-generated artifact"
                .into(),
        );
    }

    let provider = MicrosandboxProvider::new()?;

    // Required scenario 1: happy path describe/invoke through a real microVM.
    let happy_name = format!("baml:kvm-e2e:{}:happy", uuid::Uuid::new_v4());
    let mut happy_spec = SandboxSpec::for_test(happy_name, image.clone());
    happy_spec.entrypoint = entrypoint.clone();
    happy_spec.runtime_digest = Some(runtime_digest.to_string());
    happy_spec.max_duration = Duration::from_secs(300);
    happy_spec.idle_timeout = Duration::from_secs(60);

    let happy_handle = provider.create(happy_spec).await?;
    let first = invoke_echo(&provider, &happy_handle, 1, "ping").await?;
    assert_eq!(first.get("reply").and_then(Value::as_str), Some("ping"));

    // Required scenario 2: teardown removes the sandbox; rpc_channel fails.
    provider.teardown(&happy_handle).await?;
    let rpc_after_teardown = provider.rpc_channel(&happy_handle).await;
    assert!(
        rpc_after_teardown.is_err(),
        "rpc_channel should fail after teardown"
    );

    // Required scenario 3: reattach works and can invoke again.
    let reattach_name = format!("baml:kvm-e2e:{}:reattach", uuid::Uuid::new_v4());
    let mut reattach_spec = SandboxSpec::for_test(reattach_name.clone(), image);
    reattach_spec.entrypoint = entrypoint;
    reattach_spec.runtime_digest = Some(runtime_digest.to_string());
    reattach_spec.max_duration = Duration::from_secs(300);
    reattach_spec.idle_timeout = Duration::from_secs(60);

    let reattach_handle = provider.create(reattach_spec).await?;
    let before = invoke_echo(&provider, &reattach_handle, 2, "before").await?;
    assert_eq!(before.get("reply").and_then(Value::as_str), Some("before"));

    let reattached = provider.reattach(&reattach_name).await?;
    let after = invoke_echo(&provider, &reattached, 3, "after").await?;
    assert_eq!(after.get("reply").and_then(Value::as_str), Some("after"));

    provider.teardown(&reattached).await?;

    Ok(())
}

fn load_sandbox_metadata() -> Result<ExternalToolMetadata, Box<dyn std::error::Error>> {
    let path = env::var("BAML_SANDBOX_E2E_METADATA").unwrap_or_else(|_| {
        "crates/baml-rt-tools/tests/fixtures/external-tools/sandbox_echo/tool-metadata.json"
            .to_string()
    });
    let raw = fs::read_to_string(&path)?;
    let parsed: ExternalToolMetadata = serde_json::from_str(&raw)?;
    Ok(parsed)
}

async fn invoke_echo(
    provider: &dyn SandboxProvider,
    handle: &baml_rt_tools::external_tools::sandbox::SandboxHandle,
    request_id: u64,
    message: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut channel = provider.rpc_channel(handle).await?;

    let params = serde_json::to_value(ToolInvokeParams {
        invocation_id: format!("inv-{request_id}"),
        tool_name: "support/sandbox_echo".to_string(),
        input: json!({ "message": message }),
        secrets: serde_json::Map::new(),
        capabilities: Value::Null,
    })?;

    let request = serde_json::to_value(JsonRpcRequest::new(METHOD_INVOKE, request_id, params))?;
    channel.send(&request).await?;

    let frame = channel.recv().await?;
    let response: JsonRpcResponse = serde_json::from_value(frame)?;

    if let Some(err) = response.error {
        return Err(format!(
            "invoke error code={} message={} data={:?}",
            err.code, err.message, err.data
        )
        .into());
    }

    let result = response.result.ok_or("invoke response missing result")?;
    if result.get("done").and_then(Value::as_bool) != Some(true) {
        return Err("invoke response missing done=true".into());
    }

    let output = result
        .get("output")
        .cloned()
        .ok_or("invoke response missing output")?;

    // Keep a stable check that protocol-level errors did not leak into success path.
    if output.get("code").and_then(Value::as_i64) == Some(i64::from(ERR_INTERNAL)) {
        return Err("invoke output unexpectedly encoded an internal error".into());
    }

    Ok(output)
}

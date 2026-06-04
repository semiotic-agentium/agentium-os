// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Lazy first-invoke schema-drift guard for snapshot-backed external tools.
//!
//! Mirrors the MCP runtime check (`baml-rt-mcp` `runtime::verify_startup_tools_digest`):
//! the approved snapshot froze a `schema_digest` at enable/approval time, but
//! the live tool can drift before the operator refreshes and redeploys. On the
//! **first** invoke through a registry-sourced handler we fetch the live
//! `tool/schema`, recompute the digest, and compare it to the approved one.
//!
//! - **Lazy:** the check rides the first real invoke, so boot stays cheap and
//!   sandbox tools are not materialised eagerly just to read their schema.
//! - **Once, cached:** the verdict is memoised — subsequent invokes skip the
//!   check entirely. A deployed tool cannot change schema without a redeploy,
//!   which constructs a fresh handler (and a fresh guard).
//! - **Fail closed:** on mismatch the invoke errors; we never silently serve a
//!   drifted tool against the approved (codegen-baked) contract.
//!
//! A transient `tool/schema` failure is **not** cached, so the next invoke
//! retries rather than permanently bricking the tool.

use std::{future::Future, time::Duration};

use baml_rt_core::{BamlRtError, ClassifiedToolError, ErrorDisposition, Result};
use tokio::sync::OnceCell;

use super::{ExternalLifecycleEvent, ExternalLifecycleRecorder, metadata::schema_digest_from_io};
use crate::{ToolName, external_tools::ToolSchemaResult};

/// Cached outcome of the one-time live-schema comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// Live schema digest matched the approved snapshot.
    Verified,
    /// Live schema digest diverged; `observed` is what the tool reported.
    Drifted { observed: String },
}

/// Guards a snapshot-backed handler against live schema drift.
///
/// Construct one per resolved tool (sharing it across the handler's sessions
/// via `Arc`), seeded with the approved snapshot's `schema_digest`.
pub struct DriftGuard {
    tool_name: ToolName,
    expected_schema_digest: String,
    schema_timeout: Duration,
    recorder: Option<ExternalLifecycleRecorder>,
    verdict: OnceCell<Verdict>,
}

impl DriftGuard {
    pub fn new(
        tool_name: ToolName,
        expected_schema_digest: String,
        schema_timeout: Duration,
        recorder: Option<ExternalLifecycleRecorder>,
    ) -> Self {
        Self {
            tool_name,
            expected_schema_digest,
            schema_timeout,
            recorder,
            verdict: OnceCell::new(),
        }
    }

    /// Timeout the handler should pass to its `tool/schema` call.
    pub fn schema_timeout(&self) -> Duration {
        self.schema_timeout
    }

    /// Verify the live tool schema against the approved digest exactly once.
    ///
    /// `fetch_schema` performs the live `tool/schema` call and is invoked at
    /// most once across the guard's lifetime. On a confirmed match/drift the
    /// verdict is cached and reused; on a transient fetch failure nothing is
    /// cached and the error propagates so the next invoke retries.
    pub async fn ensure_verified<F, Fut>(&self, fetch_schema: F) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ToolSchemaResult>>,
    {
        let verdict = self
            .verdict
            .get_or_try_init(|| self.run_check(fetch_schema))
            .await?;
        match verdict {
            Verdict::Verified => Ok(()),
            Verdict::Drifted { observed } => Err(self.drift_error(observed)),
        }
    }

    async fn run_check<F, Fut>(&self, fetch_schema: F) -> Result<Verdict>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ToolSchemaResult>>,
    {
        // Propagating this error leaves the OnceCell uninitialised, so a
        // transient schema-fetch failure (e.g. one-off spawn error) is retried
        // on the next invoke instead of permanently failing the tool.
        let schema = fetch_schema().await?;

        // Recompute from the live input/output rather than trusting the tool's
        // self-reported `content_digest`: a drifted tool could return new
        // schemas while echoing a stale digest.
        let observed = schema_digest_from_io(&schema.input, &schema.output);
        if observed == self.expected_schema_digest {
            return Ok(Verdict::Verified);
        }

        if let Some(recorder) = &self.recorder {
            recorder(ExternalLifecycleEvent::SchemaDrift {
                tool_name: self.tool_name.to_string(),
                expected_schema_digest: self.expected_schema_digest.clone(),
                observed_schema_digest: observed.clone(),
            });
        }
        tracing::error!(
            target: "external_tools.drift",
            tool = %self.tool_name,
            expected = %self.expected_schema_digest,
            observed = %observed,
            event = "external_tool.schema_drift",
            "live external tool schema does not match approved snapshot; refusing to invoke. \
             Run `agent-platform external-tool refresh <name>` to re-approve, then redeploy."
        );
        Ok(Verdict::Drifted { observed })
    }

    fn drift_error(&self, observed: &str) -> BamlRtError {
        BamlRtError::ToolClassified(ClassifiedToolError {
            code: format!("external_{}_schema_drift", self.tool_name),
            disposition: ErrorDisposition::Fatal,
            message: format!(
                "external tool '{}' schema drift: approved snapshot digest {} but live tool reports {}; \
                 operator must refresh and re-approve the snapshot",
                self.tool_name, self.expected_schema_digest, observed
            ),
            hint: Some(
                "The deployed tool's input/output schema no longer matches its approved snapshot. \
                 Run `agent-platform external-tool refresh <name>` and redeploy."
                    .to_string(),
            ),
            retry_after_ms: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use serde_json::json;

    use super::*;

    fn schema_result(input: serde_json::Value, output: serde_json::Value) -> ToolSchemaResult {
        ToolSchemaResult {
            schema_version: 1,
            tool_name: "support/echo".to_string(),
            content_type: "application/schema+json".to_string(),
            // Deliberately bogus self-reported digest: the guard must recompute
            // from input/output and ignore this field.
            content_digest: "sha256:bogus".to_string(),
            input,
            output,
        }
    }

    fn approved_digest(input: &serde_json::Value, output: &serde_json::Value) -> String {
        schema_digest_from_io(input, output)
    }

    #[tokio::test]
    async fn verifies_once_and_caches() {
        let input = json!({"type": "object", "properties": {"q": {"type": "string"}}});
        let output = json!({"type": "object"});
        let guard = DriftGuard::new(
            ToolName::parse("support/echo").unwrap(),
            approved_digest(&input, &output),
            Duration::from_secs(5),
            None,
        );

        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let calls = calls.clone();
            let input = input.clone();
            let output = output.clone();
            guard
                .ensure_verified(move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(schema_result(input, output))
                })
                .await
                .expect("matching schema must verify");
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "tool/schema must be fetched exactly once, then cached"
        );
    }

    #[tokio::test]
    async fn drift_fails_closed_and_emits_event_once() {
        let approved_in = json!({"type": "object", "properties": {"q": {"type": "string"}}});
        let approved_out = json!({"type": "object"});

        let events = Arc::new(std::sync::Mutex::new(Vec::<ExternalLifecycleEvent>::new()));
        let recorder: ExternalLifecycleRecorder = {
            let events = events.clone();
            Arc::new(move |e| events.lock().unwrap().push(e))
        };

        let guard = DriftGuard::new(
            ToolName::parse("support/echo").unwrap(),
            approved_digest(&approved_in, &approved_out),
            Duration::from_secs(5),
            Some(recorder),
        );

        // Live tool returns a different input schema → drift.
        let drifted_in = json!({"type": "object", "properties": {"location": {"type": "string"}}});
        for _ in 0..2 {
            let drifted_in = drifted_in.clone();
            let approved_out = approved_out.clone();
            let err = guard
                .ensure_verified(move || async move {
                    Ok(schema_result(drifted_in, approved_out))
                })
                .await
                .expect_err("drift must fail closed");
            assert!(err.to_string().contains("schema drift"), "got: {err}");
        }

        let events = events.lock().unwrap();
        let drift_events = events
            .iter()
            .filter(|e| matches!(e, ExternalLifecycleEvent::SchemaDrift { .. }))
            .count();
        assert_eq!(drift_events, 1, "drift event must be emitted exactly once");
    }

    #[tokio::test]
    async fn transient_fetch_error_is_not_cached() {
        let input = json!({"type": "object"});
        let output = json!({"type": "object"});
        let guard = DriftGuard::new(
            ToolName::parse("support/echo").unwrap(),
            approved_digest(&input, &output),
            Duration::from_secs(5),
            None,
        );

        // First attempt: schema fetch fails transiently.
        let err = guard
            .ensure_verified(|| async {
                Err(BamlRtError::InvalidArgument("temporary spawn failure".into()))
            })
            .await
            .expect_err("transient fetch failure must surface");
        assert!(err.to_string().contains("temporary spawn failure"));

        // Second attempt succeeds: the failed verdict must not have been cached.
        let input2 = input.clone();
        let output2 = output.clone();
        guard
            .ensure_verified(move || async move { Ok(schema_result(input2, output2)) })
            .await
            .expect("retry after transient failure must verify");
    }
}

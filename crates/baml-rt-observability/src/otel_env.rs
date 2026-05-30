// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OTLP exporter wiring driven by OpenTelemetry environment variables.
//!
//! Behaviour matches what `opentelemetry-otlp` **0.26** reads at exporter build time
//! (endpoints, timeouts, compression, headers, etc.). See the upstream checklist:
//! <https://opentelemetry.io/docs/languages/sdk-configuration/otlp-exporter/>
//!
//! **Call site:** [`install_otel_collectors_from_env`] must run on a **Tokio** runtime
//! (same as [`opentelemetry_otlp`] batch span export). If there is no current runtime,
//! OTLP installation is skipped without error.
//!
//! **Gating:** When `OTEL_SDK_DISABLED` is `true`, nothing is installed. Per signal,
//! `OTEL_TRACES_EXPORTER` / `OTEL_METRICS_EXPORTER` value `none` disables that signal.
//! Value `otlp` enables that signal using spec default endpoints when no URL is set.
//! When those variables are **unset**, that signal is enabled only if a matching OTLP
//! endpoint variable is non-empty (`OTEL_EXPORTER_OTLP_ENDPOINT` and/or the signal-specific
//! `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` / `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`), so unit
//! tests and local runs do not open collector connections by default. Unsupported exporter
//! names disable that signal and emit a bootstrap warning on stderr.

use std::time::Duration;

use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::{
    MetricsExporterBuilder, OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_EXPORTER_OTLP_METRICS_ENDPOINT,
    OTEL_EXPORTER_OTLP_TRACES_ENDPOINT, SpanExporterBuilder,
};
use opentelemetry_sdk::{
    Resource,
    resource::{EnvResourceDetector, TelemetryResourceDetector},
    runtime::Tokio,
    trace,
};

use crate::runner_identity::{
    SERVICE_INSTANCE_ID_KEY, parse_otel_resource_attr, service_instance_id,
};

/// OTEL semantic convention keys we set on the runner resource. Kept as module-local
/// constants so each key is spelled once for both the `KeyValue::new` write and the
/// `parse_otel_resource_attr` override lookup.
const SERVICE_NAME_KEY: &str = "service.name";
const SERVICE_NAMESPACE_KEY: &str = "service.namespace";
const K8S_NAMESPACE_NAME_KEY: &str = "k8s.namespace.name";
const DEPLOYMENT_ENVIRONMENT_KEY: &str = "deployment.environment";

/// Default `service.name` for a runner when no env override is provided.
const DEFAULT_SERVICE_NAME: &str = "agentium-runner";
/// Default `service.namespace` for the pilot contract.
const DEFAULT_SERVICE_NAMESPACE: &str = "agentium";
/// Default `deployment.environment` when neither the chart nor the env supplies a value.
const DEFAULT_DEPLOYMENT_ENVIRONMENT: &str = "pilot";

/// Canonical OTEL `Resource` for a runner in the K8s pilot.
///
/// Composes:
/// - `service.name` (default `agentium-runner`, override via `OTEL_SERVICE_NAME` or
///   `OTEL_RESOURCE_ATTRIBUTES.service.name`).
/// - `service.namespace` (default `agentium`, override via `OTEL_RESOURCE_ATTRIBUTES`).
/// - `service.instance.id` via [`service_instance_id`] — same derivation used by every
///   `*_service_instance_id` span/metric dimension, so the two cannot drift.
/// - `k8s.namespace.name` from `POD_NAMESPACE` (downward API) when set.
/// - `deployment.environment` from `OTEL_DEPLOYMENT_ENVIRONMENT` → default `pilot`.
///
/// Merges in `TelemetryResourceDetector` (telemetry SDK name/version) and
/// `EnvResourceDetector` (other `OTEL_RESOURCE_ATTRIBUTES` keys the caller supplies) so
/// unrelated operator-supplied attributes still flow through.
pub fn build_runner_resource() -> Resource {
    build_runner_resource_with_instance_id(service_instance_id())
}

/// Compose the canonical runner [`Resource`] with an explicit `service.instance.id`.
///
/// Exposed for tests that need to inject a deterministic id without touching the
/// process-global [`service_instance_id`] cache. Production callers should use
/// [`build_runner_resource`].
pub(crate) fn build_runner_resource_with_instance_id(service_instance_id: &str) -> Resource {
    let mut kvs: Vec<KeyValue> = Vec::with_capacity(5);
    kvs.push(KeyValue::new(SERVICE_NAME_KEY, resolve_service_name()));
    kvs.push(KeyValue::new(
        SERVICE_NAMESPACE_KEY,
        resolve_service_namespace(),
    ));
    kvs.push(KeyValue::new(
        SERVICE_INSTANCE_ID_KEY,
        service_instance_id.to_string(),
    ));
    if let Some(ns) = resolve_k8s_namespace() {
        kvs.push(KeyValue::new(K8S_NAMESPACE_NAME_KEY, ns));
    }
    kvs.push(KeyValue::new(
        DEPLOYMENT_ENVIRONMENT_KEY,
        resolve_deployment_environment(),
    ));

    let base = Resource::new(kvs);
    let auxiliary = Resource::from_detectors(
        Duration::from_secs(0),
        vec![
            Box::new(TelemetryResourceDetector),
            Box::new(EnvResourceDetector::new()),
        ],
    );
    // `auxiliary` wins on conflict — per OTEL spec `OTEL_RESOURCE_ATTRIBUTES` overrides
    // programmatic defaults. Our helper-derived `service.instance.id` already honored that
    // env override, so both sides agree for that key regardless of merge direction.
    base.merge(&auxiliary)
}

fn resolve_service_name() -> String {
    if let Ok(v) = std::env::var("OTEL_SERVICE_NAME")
        && !v.trim().is_empty()
    {
        return v;
    }
    if let Some(v) = parse_otel_resource_attr(SERVICE_NAME_KEY) {
        return v;
    }
    DEFAULT_SERVICE_NAME.to_string()
}

fn resolve_service_namespace() -> String {
    if let Some(v) = parse_otel_resource_attr(SERVICE_NAMESPACE_KEY) {
        return v;
    }
    DEFAULT_SERVICE_NAMESPACE.to_string()
}

fn resolve_k8s_namespace() -> Option<String> {
    if let Some(v) = parse_otel_resource_attr(K8S_NAMESPACE_NAME_KEY) {
        return Some(v);
    }
    std::env::var("POD_NAMESPACE")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn resolve_deployment_environment() -> String {
    if let Ok(v) = std::env::var("OTEL_DEPLOYMENT_ENVIRONMENT")
        && !v.trim().is_empty()
    {
        return v;
    }
    if let Some(v) = parse_otel_resource_attr(DEPLOYMENT_ENVIRONMENT_KEY) {
        return v;
    }
    DEFAULT_DEPLOYMENT_ENVIRONMENT.to_string()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OtlpWireProtocol {
    Grpc,
    HttpProtobuf,
}

fn resolve_otlp_protocol() -> OtlpWireProtocol {
    match std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").map(|s| s.trim().to_ascii_lowercase()) {
        Ok(s) if s == "grpc" => OtlpWireProtocol::Grpc,
        Ok(s) if s == "http/protobuf" => OtlpWireProtocol::HttpProtobuf,
        Ok(s) if s == "http/json" => {
            eprintln!(
                "baml-rt-observability: OTEL_EXPORTER_OTLP_PROTOCOL=http/json requires the opentelemetry-otlp `http-json` feature; using http/protobuf"
            );
            OtlpWireProtocol::HttpProtobuf
        }
        Ok(s) if s.is_empty() => OtlpWireProtocol::HttpProtobuf,
        Ok(s) => {
            eprintln!(
                "baml-rt-observability: unknown OTEL_EXPORTER_OTLP_PROTOCOL={s:?}; using http/protobuf"
            );
            OtlpWireProtocol::HttpProtobuf
        }
        Err(_) => {
            // With both `grpc-tonic` and `http-proto` enabled, opentelemetry-otlp 0.26 defaults
            // to HTTP/protobuf (see `OTEL_EXPORTER_OTLP_PROTOCOL_DEFAULT` in that crate).
            OtlpWireProtocol::HttpProtobuf
        }
    }
}

fn span_exporter_builder(protocol: OtlpWireProtocol) -> SpanExporterBuilder {
    match protocol {
        OtlpWireProtocol::Grpc => opentelemetry_otlp::new_exporter().tonic().into(),
        OtlpWireProtocol::HttpProtobuf => opentelemetry_otlp::new_exporter().http().into(),
    }
}

fn metrics_exporter_builder(protocol: OtlpWireProtocol) -> MetricsExporterBuilder {
    match protocol {
        OtlpWireProtocol::Grpc => opentelemetry_otlp::new_exporter().tonic().into(),
        OtlpWireProtocol::HttpProtobuf => opentelemetry_otlp::new_exporter().http().into(),
    }
}

fn otlp_traces_endpoint_configured() -> bool {
    std::env::var(OTEL_EXPORTER_OTLP_TRACES_ENDPOINT)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
        || std::env::var(OTEL_EXPORTER_OTLP_ENDPOINT)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
}

fn otlp_metrics_endpoint_configured() -> bool {
    std::env::var(OTEL_EXPORTER_OTLP_METRICS_ENDPOINT)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
        || std::env::var(OTEL_EXPORTER_OTLP_ENDPOINT)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
}

fn traces_wanted() -> bool {
    match std::env::var("OTEL_TRACES_EXPORTER").map(|s| s.trim().to_ascii_lowercase()) {
        Err(_) => otlp_traces_endpoint_configured(),
        Ok(s) if s.is_empty() => otlp_traces_endpoint_configured(),
        Ok(s) if s == "otlp" => true,
        Ok(s) if s == "none" || s == "noop" => false,
        Ok(s) => {
            eprintln!(
                "baml-rt-observability: unsupported OTEL_TRACES_EXPORTER={s:?}; OTLP tracing disabled"
            );
            false
        }
    }
}

fn metrics_wanted() -> bool {
    match std::env::var("OTEL_METRICS_EXPORTER").map(|s| s.trim().to_ascii_lowercase()) {
        Err(_) => otlp_metrics_endpoint_configured(),
        Ok(s) if s.is_empty() => otlp_metrics_endpoint_configured(),
        Ok(s) if s == "otlp" => true,
        Ok(s) if s == "none" || s == "noop" => false,
        Ok(s) => {
            eprintln!(
                "baml-rt-observability: unsupported OTEL_METRICS_EXPORTER={s:?}; OTLP metrics disabled"
            );
            false
        }
    }
}

fn try_install_traces(
    protocol: OtlpWireProtocol,
    resource: Resource,
) -> Result<Option<opentelemetry_sdk::trace::Tracer>, opentelemetry::trace::TraceError> {
    let exporter = span_exporter_builder(protocol);
    let trace_config = trace::Config::default().with_resource(resource);
    let provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .with_trace_config(trace_config)
        .install_batch(Tokio)?;
    let tracer = provider.tracer("baml_rt");
    global::set_tracer_provider(provider);
    Ok(Some(tracer))
}

fn try_install_metrics(
    protocol: OtlpWireProtocol,
    resource: Resource,
) -> Result<(), opentelemetry::metrics::MetricsError> {
    let exporter = metrics_exporter_builder(protocol);
    let provider = opentelemetry_otlp::new_pipeline()
        .metrics(Tokio)
        .with_exporter(exporter)
        .with_resource(resource)
        .build()?;
    global::set_meter_provider(provider);
    Ok(())
}

/// Installs global OTLP trace and metrics providers when enabled by environment variables,
/// tagging all emitted telemetry with the supplied [`Resource`]. Non-runner binaries
/// (builder CLI, task-daemon, tests) should pass [`Resource::default()`]; the runner
/// passes [`build_runner_resource()`] to adopt the pilot identity contract.
///
/// Returns a [`opentelemetry_sdk::trace::Tracer`] for the tracing-subscriber OpenTelemetry
/// layer when trace export is enabled and installation succeeds.
pub fn install_otel_collectors_from_env(
    resource: Resource,
) -> Option<opentelemetry_sdk::trace::Tracer> {
    if std::env::var("OTEL_SDK_DISABLED").ok().as_deref() == Some("true") {
        return None;
    }

    if tokio::runtime::Handle::try_current().is_err() {
        eprintln!(
            "baml-rt-observability: OTLP skipped (no Tokio runtime; call init_tracing from #[tokio::main] or similar)"
        );
        return None;
    }

    let protocol = resolve_otlp_protocol();
    let mut tracer_out = None;

    if traces_wanted() {
        match try_install_traces(protocol, resource.clone()) {
            Ok(t) => tracer_out = t,
            Err(err) => eprintln!("baml-rt-observability: OTLP trace exporter init failed: {err}"),
        }
    }

    if metrics_wanted()
        && let Err(err) = try_install_metrics(protocol, resource)
    {
        eprintln!("baml-rt-observability: OTLP metrics exporter init failed: {err}");
    }

    tracer_out
}

#[cfg(test)]
mod tests {
    use opentelemetry::Key;

    use super::*;
    use crate::test_env::EnvScope;

    fn resource_value(resource: &Resource, key: &str) -> Option<String> {
        resource
            .get(Key::new(key.to_string()))
            .map(|v| v.to_string())
    }

    #[test]
    fn defaults_when_no_env_overrides() {
        let mut env = EnvScope::new();
        env.set("OTEL_SERVICE_NAME", None);
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAMESPACE", None);
        env.set("OTEL_DEPLOYMENT_ENVIRONMENT", None);

        let r = build_runner_resource_with_instance_id("instance-a");
        assert_eq!(
            resource_value(&r, "service.name").as_deref(),
            Some("agentium-runner"),
        );
        assert_eq!(
            resource_value(&r, "service.namespace").as_deref(),
            Some("agentium"),
        );
        assert_eq!(
            resource_value(&r, "service.instance.id").as_deref(),
            Some("instance-a"),
        );
        assert_eq!(
            resource_value(&r, "deployment.environment").as_deref(),
            Some("pilot"),
        );
        // POD_NAMESPACE absent → k8s.namespace.name is not set.
        assert!(
            resource_value(&r, "k8s.namespace.name").is_none(),
            "k8s.namespace.name should be omitted when POD_NAMESPACE is unset"
        );
    }

    #[test]
    fn otel_service_name_overrides_default() {
        let mut env = EnvScope::new();
        env.set("OTEL_SERVICE_NAME", Some("my-runner"));
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAMESPACE", None);
        env.set("OTEL_DEPLOYMENT_ENVIRONMENT", None);

        let r = build_runner_resource_with_instance_id("instance-b");
        assert_eq!(
            resource_value(&r, "service.name").as_deref(),
            Some("my-runner"),
        );
    }

    #[test]
    fn otel_resource_attributes_can_override_service_namespace() {
        let mut env = EnvScope::new();
        env.set("OTEL_SERVICE_NAME", None);
        env.set(
            "OTEL_RESOURCE_ATTRIBUTES",
            Some("service.namespace=custom-ns"),
        );
        env.set("POD_NAMESPACE", None);
        env.set("OTEL_DEPLOYMENT_ENVIRONMENT", None);

        let r = build_runner_resource_with_instance_id("instance-c");
        assert_eq!(
            resource_value(&r, "service.namespace").as_deref(),
            Some("custom-ns"),
        );
    }

    #[test]
    fn pod_namespace_populates_k8s_namespace_name() {
        let mut env = EnvScope::new();
        env.set("OTEL_SERVICE_NAME", None);
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAMESPACE", Some("agentium-pilot"));
        env.set("OTEL_DEPLOYMENT_ENVIRONMENT", None);

        let r = build_runner_resource_with_instance_id("instance-d");
        assert_eq!(
            resource_value(&r, "k8s.namespace.name").as_deref(),
            Some("agentium-pilot"),
        );
    }

    #[test]
    fn deployment_environment_env_overrides_default() {
        let mut env = EnvScope::new();
        env.set("OTEL_SERVICE_NAME", None);
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAMESPACE", None);
        env.set("OTEL_DEPLOYMENT_ENVIRONMENT", Some("staging"));

        let r = build_runner_resource_with_instance_id("instance-e");
        assert_eq!(
            resource_value(&r, "deployment.environment").as_deref(),
            Some("staging"),
        );
    }

    #[test]
    fn otel_resource_attributes_supplies_unrelated_keys() {
        let mut env = EnvScope::new();
        env.set("OTEL_SERVICE_NAME", None);
        env.set(
            "OTEL_RESOURCE_ATTRIBUTES",
            Some("operator.team=platform,operator.contact=pager@agentium"),
        );
        env.set("POD_NAMESPACE", None);
        env.set("OTEL_DEPLOYMENT_ENVIRONMENT", None);

        let r = build_runner_resource_with_instance_id("instance-f");
        assert_eq!(
            resource_value(&r, "operator.team").as_deref(),
            Some("platform"),
        );
        assert_eq!(
            resource_value(&r, "operator.contact").as_deref(),
            Some("pager@agentium"),
        );
        // Our pilot defaults still apply.
        assert_eq!(
            resource_value(&r, "service.name").as_deref(),
            Some("agentium-runner"),
        );
        assert_eq!(
            resource_value(&r, "service.instance.id").as_deref(),
            Some("instance-f"),
        );
    }
}

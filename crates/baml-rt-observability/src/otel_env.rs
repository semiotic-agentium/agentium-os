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

use opentelemetry::{global, trace::TracerProvider as _};
use opentelemetry_otlp::{
    MetricsExporterBuilder, OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_EXPORTER_OTLP_METRICS_ENDPOINT,
    OTEL_EXPORTER_OTLP_TRACES_ENDPOINT, SpanExporterBuilder,
};
use opentelemetry_sdk::{Resource, runtime::Tokio, trace};

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
) -> Result<Option<opentelemetry_sdk::trace::Tracer>, opentelemetry::trace::TraceError> {
    let exporter = span_exporter_builder(protocol);
    let resource = Resource::default();
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
) -> Result<(), opentelemetry::metrics::MetricsError> {
    let exporter = metrics_exporter_builder(protocol);
    let resource = Resource::default();
    let provider = opentelemetry_otlp::new_pipeline()
        .metrics(Tokio)
        .with_exporter(exporter)
        .with_resource(resource)
        .build()?;
    global::set_meter_provider(provider);
    Ok(())
}

/// Installs global OTLP trace and metrics providers when enabled by environment variables.
///
/// Returns a [`opentelemetry_sdk::trace::Tracer`] for the tracing-subscriber OpenTelemetry
/// layer when trace export is enabled and installation succeeds.
pub fn install_otel_collectors_from_env() -> Option<opentelemetry_sdk::trace::Tracer> {
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
        match try_install_traces(protocol) {
            Ok(t) => tracer_out = t,
            Err(err) => eprintln!("baml-rt-observability: OTLP trace exporter init failed: {err}"),
        }
    }

    if metrics_wanted()
        && let Err(err) = try_install_metrics(protocol)
    {
        eprintln!("baml-rt-observability: OTLP metrics exporter init failed: {err}");
    }

    tracer_out
}

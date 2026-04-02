//! Standard tracing subscriber setup for CLI binaries.

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize tracing/logging and optionally OTLP export (traces + metrics) from env.
///
/// OTLP export is enabled when env settings request it (see `otel_env.rs`), e.g.:
/// - `OTEL_TRACES_EXPORTER=otlp`
/// - `OTEL_METRICS_EXPORTER=otlp`
/// - `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317`
///
/// Default directives:
/// - `baml_rt=info`
/// - `baml_rt_quickjs=info` (explicit; ensures QuickJS bridge logs are visible at info+)
/// - `quickjs_runtime::quickjsrealmadapter=warn`
/// - `quickjs_runtime::typescript=warn`
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("baml_rt=info".parse().unwrap_or_default())
        .add_directive("baml_rt_quickjs=info".parse().unwrap_or_default())
        .add_directive("baml_agent_runner=info".parse().unwrap_or_default())
        .add_directive(
            "quickjs_runtime::quickjsrealmadapter=warn"
                .parse()
                .unwrap_or_default(),
        )
        .add_directive(
            "quickjs_runtime::typescript=warn"
                .parse()
                .unwrap_or_default(),
        );

    let fmt_layer = tracing_subscriber::fmt::layer();
    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

    if let Some(tracer) = crate::otel_env::install_otel_collectors_from_env() {
        registry
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .init();
    } else {
        registry.init();
    }
}

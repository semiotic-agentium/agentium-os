//! Standard tracing subscriber setup for CLI binaries.
//!
//! Per-layer filtering: console (fmt) and OpenTelemetry trace export can use **different**
//! level targets so operators may run verbose local logs (`RUST_LOG` / `RUST_LOG_FMT`) without
//! forwarding every `debug` span to the collector (`RUST_LOG_OTEL`).

use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Env var for **console** (`fmt`) output only. When unset, [`EnvFilter::from_default_env`]
/// applies (i.e. `RUST_LOG`), then the same built-in defaults as before are merged in.
pub const RUST_LOG_FMT_ENV: &str = "RUST_LOG_FMT";

/// Env var for the **OpenTelemetry tracing** layer only (span export). When unset, defaults
/// to `info` plus QuickJS noise suppression so Tempo/Jaeger stay operationally shallow while
/// `RUST_LOG=debug` can still print full console diagnostics.
pub const RUST_LOG_OTEL_ENV: &str = "RUST_LOG_OTEL";

fn console_env_filter() -> EnvFilter {
    if let Ok(spec) = std::env::var(RUST_LOG_FMT_ENV)
        && !spec.trim().is_empty()
    {
        match EnvFilter::try_new(&spec) {
            Ok(f) => return f,
            Err(err) => eprintln!(
                "baml-rt-observability: invalid {RUST_LOG_FMT_ENV}={spec:?} ({err}); using RUST_LOG + defaults"
            ),
        }
    }

    EnvFilter::from_default_env()
        .add_directive("baml_rt=info".parse().expect("static directive"))
        .add_directive("baml_rt_quickjs=debug".parse().expect("static directive"))
        .add_directive(
            "baml_rt_interceptor=debug"
                .parse()
                .expect("static directive"),
        )
        .add_directive("baml_agent_runner=info".parse().expect("static directive"))
        .add_directive(
            "quickjs_runtime::quickjsrealmadapter=warn"
                .parse()
                .expect("static directive"),
        )
        .add_directive(
            "quickjs_runtime::typescript=warn"
                .parse()
                .expect("static directive"),
        )
}

fn otel_trace_env_filter() -> EnvFilter {
    if let Ok(spec) = std::env::var(RUST_LOG_OTEL_ENV)
        && !spec.trim().is_empty()
    {
        match EnvFilter::try_new(&spec) {
            Ok(f) => return f,
            Err(err) => eprintln!(
                "baml-rt-observability: invalid {RUST_LOG_OTEL_ENV}={spec:?} ({err}); using default otel filter"
            ),
        }
    }

    EnvFilter::try_new(concat!(
        "info,",
        "quickjs_runtime::quickjsrealmadapter=warn,",
        "quickjs_runtime::typescript=warn",
    ))
    .expect("static RUST_LOG_OTEL default")
}

/// Initialize tracing/logging and optionally OTLP export (traces + metrics) from env.
///
/// OTLP export is enabled when env settings request it (see `otel_env.rs`), e.g.:
/// - `OTEL_TRACES_EXPORTER=otlp`
/// - `OTEL_METRICS_EXPORTER=otlp`
/// - `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317`
///
/// **Per-layer filters**
///
/// - **Console** (`fmt`): `RUST_LOG_FMT` if set; otherwise `RUST_LOG` via [`EnvFilter::from_default_env`]
///   plus defaults (`baml_rt=info`, `baml_rt_quickjs=debug`, `baml_rt_interceptor=debug`,
///   `baml_agent_runner=info`, QuickJS adapter crates `warn`).
/// - **Trace export** (when OTLP tracing is installed): `RUST_LOG_OTEL` if set; otherwise `info`
///   and the same QuickJS `warn` defaults—so local `RUST_LOG=debug` does not imply exporting every
///   debug span unless `RUST_LOG_OTEL` is widened.
pub fn init_tracing() {
    let console_filter = console_env_filter();
    let fmt_layer = tracing_subscriber::fmt::layer().with_filter(console_filter);

    if let Some(tracer) = crate::otel_env::install_otel_collectors_from_env() {
        let otel_filter = otel_trace_env_filter();
        let otel_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(otel_filter);
        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(otel_layer)
            .init();
    } else {
        tracing_subscriber::registry().with(fmt_layer).init();
    }
}

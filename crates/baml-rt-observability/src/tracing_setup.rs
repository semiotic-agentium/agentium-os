//! Standard tracing subscriber setup for CLI binaries.
//!
//! Per-layer filtering: console (fmt) and OpenTelemetry trace export can use **different**
//! level targets so operators may run verbose local logs (`RUST_LOG` / `RUST_LOG_FMT`) without
//! forwarding every `debug` span to the collector (`RUST_LOG_OTEL`).

use opentelemetry::{global, propagation::TextMapCompositePropagator};
use opentelemetry_sdk::{
    Resource,
    propagation::{BaggagePropagator, TraceContextPropagator},
};
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Env var for **console** (`fmt`) output only. When unset, [`EnvFilter::from_default_env`]
/// applies (i.e. `RUST_LOG`), then the same built-in defaults as before are merged in.
pub const RUST_LOG_FMT_ENV: &str = "RUST_LOG_FMT";

/// Env var for the **OpenTelemetry tracing** layer only (span export). When unset, defaults
/// to `info` plus QuickJS noise suppression so Tempo/Jaeger stay operationally shallow while
/// `RUST_LOG=debug` can still print full console diagnostics.
pub const RUST_LOG_OTEL_ENV: &str = "RUST_LOG_OTEL";

/// Noise-suppression directives shared by both console and OTLP filters.
const QUICKJS_NOISE_DIRECTIVES: &[&str] = &[
    "quickjs_runtime::quickjsrealmadapter=warn",
    "quickjs_runtime::typescript=warn",
];

/// Append the shared QuickJS noise-suppression directives to a filter.
fn with_quickjs_suppression(filter: EnvFilter) -> EnvFilter {
    QUICKJS_NOISE_DIRECTIVES.iter().fold(filter, |f, d| {
        f.add_directive(d.parse().expect("static directive"))
    })
}

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

    with_quickjs_suppression(
        EnvFilter::from_default_env()
            .add_directive("baml_rt=info".parse().expect("static directive"))
            .add_directive("baml_rt_quickjs=debug".parse().expect("static directive"))
            .add_directive(
                "baml_rt_interceptor=debug"
                    .parse()
                    .expect("static directive"),
            )
            .add_directive("baml_agent_runner=info".parse().expect("static directive")),
    )
}

/// Install a composite W3C trace-context + baggage propagator as the process-global
/// text map propagator. This is what makes `opentelemetry_http::{HeaderInjector,
/// HeaderExtractor}` carry trace context and baggage across HTTP hops so a forwarded
/// A2A request appears as a single distributed trace. Safe to call multiple times — the
/// last call wins.
fn install_global_propagator() {
    let propagator = TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ]);
    global::set_text_map_propagator(propagator);
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

    with_quickjs_suppression(EnvFilter::try_new("info").expect("static RUST_LOG_OTEL default"))
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
    init_tracing_with_resource(Resource::default());
}

/// Initialize tracing/logging and optionally OTLP export, tagging all emitted telemetry
/// with the supplied [`Resource`].
///
/// The runner calls this with [`crate::otel_env::build_runner_resource()`] so its spans
/// and metrics adopt the pilot identity contract (`service.name=agentium-runner`,
/// `service.instance.id=$POD_NAME`, etc.). Other binaries (builder CLI, task-daemon,
/// tests) should stay on [`init_tracing`] to inherit `Resource::default()` and avoid
/// being mislabeled as runner telemetry.
pub fn init_tracing_with_resource(resource: Resource) {
    install_global_propagator();

    let console_filter = console_env_filter();
    // Write fmt output to stderr so each event flushes promptly.
    // Rust's std::io::stdout is line-buffered on a TTY but switches to an
    // 8 KB BufWriter when piped (every container kubelet/CRI does this),
    // which can swallow the last log lines before a stall — see issue #343.
    // std::io::stderr is unbuffered in Rust's stdlib, so per-event writes
    // hit the file descriptor immediately. This also cleanly reserves
    // stdout for intentional CLI data output (the runner's --list-agents
    // JSON, etc.) versus stderr for operational logs, per Unix convention.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(console_filter);

    if let Some(tracer) = crate::otel_env::install_otel_collectors_from_env(resource) {
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

//! Standard tracing subscriber setup for CLI binaries.

/// Initialize a tracing subscriber with env-based filtering.
///
/// Default directives:
/// - `baml_rt=info`
/// - `quickjs_runtime::quickjsrealmadapter=warn`
/// - `quickjs_runtime::typescript=warn`
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("baml_rt=info".parse().unwrap_or_default())
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

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

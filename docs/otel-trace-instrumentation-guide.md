# OpenTelemetry Instrumentation Guide

**Patterns for instrumenting Rust crates with production-grade OTel spans.**

Based on `credit-onramp` implementation: orthogonal span module, structured attributes, async propagation, and testability.

---

## Design Goals

1. **Separation of Concerns**: Span instrumentation lives in a separate module, not mixed with business logic
2. **Machine-Parseable**: All attributes are structured fields (no string interpolation in span names or messages)
3. **Async-Safe**: Spans propagate correctly across `.await` boundaries
4. **Testable**: Span structure and attributes are verified in tests
5. **Low Cardinality**: Span names and attribute keys are static; dynamic data goes in attribute _values_

---

## Architecture Pattern

### Module Structure

```
my_crate/
├── src/
│   ├── lib.rs         # Business logic (DB queries, conversions, etc.)
│   ├── spans.rs       # OTel span helpers (orthogonal to business logic)
│   └── tests/
│       └── integration.rs
```

**Why separate?**

- Business logic functions stay clean and focused
- Span naming and attribute schemas are centralized
- Easy to audit and maintain instrumentation
- Can disable/swap tracing without touching core logic

### The `spans.rs` Module

Create a dedicated module with span creation helpers:

```rust
// src/spans.rs
use tracing::Span;

/// Create span for webhook recording operation.
///
/// Parent: HTTP request span (auto-attached by tracing)
/// Children: sqlx.execute (auto from sqlx-tracing)
#[inline]
pub(crate) fn insert_webhook_event(event_id: &str) -> Span {
    tracing::debug_span!(
        "credit_onramp.insert_webhook_event",
        event_id = event_id,
        // All dynamic data as typed fields, never in span name
    )
}

/// Create span for ledger insert.
///
/// Parent: apply transaction span
/// Children: sqlx.execute
#[inline]
pub(crate) fn insert_ledger(
    event_id: &str,
    account_id: &uuid::Uuid,
    resource_id: &str,
) -> Span {
    tracing::debug_span!(
        "credit_onramp.insert_ledger",
        event_id = event_id,
        account_id = %account_id,  // Display formatting for types
        resource_id = resource_id,
    )
}
```

**Key principles:**

- `#[inline]` so they're zero-cost when tracing is disabled
- Static span names (`"credit_onramp.operation_name"`)
- Namespace prefix prevents collisions (`credit_onramp.` vs `sqlx.`)
- All dynamic data goes in **fields**, not the span name
- Document parent/child relationships

---

## Span Naming Convention

**Format**: `{crate_name}.{operation_name}`

### Examples

✅ **Good:**

```rust
tracing::info_span!("credit_onramp.insert_webhook_event", event_id = eid)
tracing::debug_span!("credit_onramp.fetch_effective_rate", currency = cur)
tracing::trace_span!("payment_processor.validate_card", last4 = card.last4)
```

❌ **Bad:**

```rust
tracing::info_span!("insert_webhook_event")  // Missing namespace
tracing::info_span!("credit_onramp")  // Too vague
tracing::info_span!("webhook {}", event_id)  // Dynamic name (high cardinality!)
```

### Span Levels

- `trace_span!`: Handshakes, low-level protocol details
- `debug_span!`: Database queries, API calls, business operations
- `info_span!`: HTTP requests, major workflow steps
- `warn_span!`: Unusual paths (retries, fallbacks)
- `error_span!`: Error handling paths

---

## Structured Attributes

**CRITICAL**: Use typed fields, NEVER string interpolation.

### ✅ Correct Patterns

```rust
// Display formatting for complex types
tracing::info_span!("request", account_id = %uuid, amount = %decimal);

// Direct assignment for simple types
tracing::debug_span!("query", event_id = event_id, count = 42);

// Debug formatting for structs
tracing::trace_span!("parse", payload = ?body);
```

### ❌ Anti-Patterns

```rust
// NEVER: String interpolation in span names
tracing::info_span!("request for account {}", account_id);  // ❌ High cardinality!

// NEVER: Runtime formatting in messages
tracing::info!("Processing {} with amount {}", eid, amt);  // ❌ Not machine-parseable!

// NEVER: Positional arguments
tracing::info!("outcome: {}", outcome);  // ❌ Use fields!
```

### Correct Logging with Spans

```rust
// ✅ Structured fields, static message
let span = tracing::info_span!("apply_event", event_id = eid);
let _guard = span.enter();
tracing::info!(outcome = ?result, "event applied");

// Context comes from span fields, not log message
```

---

## Instrumenting Async Functions

### The Orthogonal Pattern (RECOMMENDED)

**Always use the spans module + guard pattern.** Never use `#[tracing::instrument]` attribute on business logic.

```rust
pub async fn record_webhook(&self, event_id: &str) -> Result<()> {
    let span = spans::insert_webhook_event(event_id);
    let _guard = span.enter();  // Span active for entire function

    sqlx::query("INSERT INTO webhook_events ...")
        .bind(event_id)
        .execute(&mut self.pool)  // sqlx-tracing auto-creates child span
        .await?;

    Ok(())
}
```

**Why this pattern?**

✅ **Separation**: Instrumentation lives in `spans.rs`, not scattered across business logic
✅ **Refactoring-safe**: Change function signatures without touching span schemas
✅ **Centralized**: All span names and attribute schemas in one place
✅ **Conditional**: Can skip instrumentation for hot paths
✅ **Fine control**: Nested spans, early drops, complex scenarios

### ❌ Anti-Pattern: `#[tracing::instrument]`

**Don't do this:**

```rust
// ❌ BAD: Mixes instrumentation into business logic!
#[tracing::instrument(
    name = "credit_onramp.apply_event",
    skip(self),
    fields(event_id = event_id)
)]
pub async fn apply_event(&self, event_id: &str) -> Result<ApplyOutcome> {
    // Business logic here
}
```

**Why avoid it:**

❌ Couples instrumentation to function definition
❌ Span schema changes require touching business logic
❌ Hard to audit all instrumentation (scattered across files)
❌ Attribute names tied to parameter names (refactoring breaks spans)
❌ Violates separation of concerns

**Exception**: `#[instrument]` is acceptable for **test helpers only**, never production code

---

## Database Query Instrumentation with sqlx-tracing

### Why sqlx-tracing?

**Don't use raw `sqlx::Pool`** - use `sqlx_tracing::Pool` instead!

```rust
use sqlx_tracing::Pool;  // ✅ Not sqlx::Pool

pub struct CreditStore {
    pool: Arc<Pool<Postgres>>,  // Traced pool
}

impl CreditStore {
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        // Create traced pool - auto-instruments all queries!
        let pool = Pool::connect_with(
            sqlx::postgres::PgConnectOptions::from_str(url)?
                .application_name("credit-onramp"),
        )
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .await?;

        Ok(Self { pool: Arc::new(pool) })
    }
}
```

### What You Get Automatically

Every database query becomes a **child span** with rich attributes:

```rust
// Your code:
let span = spans::insert_ledger(event_id, account_id, resource_id);
let _guard = span.enter();

sqlx::query("INSERT INTO credit_ledger ...")
    .bind(account_id)
    .bind(event_id)
    .execute(&mut pool)  // sqlx_tracing pool
    .await?;
```

**Resulting span hierarchy:**

```
credit_onramp.insert_ledger {event_id, account_id, resource_id}
└── sqlx.execute {db.name, db.query.text, db.system.name, net.peer.name, elapsed}
```

**Attributes automatically added by sqlx-tracing:**

- `db.name`: Database name (`postgres`)
- `db.system.name`: `"postgresql"`
- `db.query.text`: The SQL query (first 100 chars)
- `net.peer.name`: Database host
- `net.peer.port`: Database port
- `otel.kind`: `"client"`
- `elapsed`: Query duration
- `rows_affected`: Number of rows modified
- `rows_returned`: Number of rows fetched

### Setup Requirements

**Cargo.toml:**

```toml
[dependencies]
sqlx = { version = "0.8", features = ["postgres", "runtime-tokio", "tls-rustls"] }
sqlx-tracing = "0.2"  # The magic sauce!

[dev-dependencies]
otel-test-support = { path = "../otel-test-support" }
```

**Migration note**: If you have existing code using `sqlx::Pool`, search-replace:

```rust
// Old
use sqlx::Pool;
let pool: Pool<Postgres> = ...;

// New
use sqlx_tracing::Pool;
let pool: Pool<Postgres> = ...;  // Same type, just wrapped!
```

### Benefits

✅ **Zero instrumentation code**: No manual span creation for queries
✅ **Automatic parent linking**: Queries are children of your business spans
✅ **Rich context**: Database host, query text, timing, row counts
✅ **Performance tracking**: `elapsed` attribute shows slow queries
✅ **Semantic conventions**: Follows OpenTelemetry database conventions

### Testing Database Spans

```rust
#[tokio::test]
async fn test_ledger_insert_has_db_span() {
    let otel = OtelTestFixture::new();
    let store = CreditStore::connect(&url, 8).await.unwrap();

    // Your operation
    store.insert_ledger_entry(account_id, event_id).await.unwrap();

    // Verify span hierarchy
    otel.assert_spans()
        .assert_exists("credit_onramp.insert_ledger")
        .assert_child_named("credit_onramp.insert_ledger", "sqlx.execute")
        .graph()
        .first_by_name("sqlx.execute")
        .attributes
        .get("db.name")
        .expect("db.name attribute should exist");
}
```

**Pattern**: Your business span → `sqlx.execute` child (automatic!) → attributes (automatic!)

---

## Parent-Child Relationships

### Automatic Propagation (Default)

Tracing automatically makes child spans:

```rust
async fn parent(&self) -> Result<()> {
    let span = tracing::info_span!("parent");
    let _guard = span.enter();

    self.child().await?;  // child's span is automatically parented
    Ok(())
}

async fn child(&self) -> Result<()> {
    let span = tracing::debug_span!("child");
    let _guard = span.enter();
    // This span's parent is "parent" (automatic via tracing context)
    Ok(())
}
```

### Explicit Parenting (Rare, for Spawned Tasks)

```rust
async fn spawn_work(&self) -> Result<()> {
    let parent_span = tracing::Span::current();

    tokio::spawn(async move {
        // Explicitly attach parent context
        let span = tracing::debug_span!(parent: &parent_span, "spawned_work");
        let _guard = span.enter();
        // work...
    });

    Ok(())
}
```

---

## Testing Strategy

### The OtelTestFixture Pattern

Create a reusable test fixture that captures spans:

```rust
// lib/otel-test-support/src/lib.rs

pub struct OtelTestFixture {
    provider: SdkTracerProvider,
    store: InMemorySpanStore,  // Thread-safe span capture
    _guard: tracing::subscriber::DefaultGuard,
}

impl OtelTestFixture {
    pub fn new() -> Self {
        let (exporter, store) = InMemoryExporter::new();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter)
            .build();

        // Set globally for cross-thread/task capture
        global::set_tracer_provider(provider.clone());

        // Also set thread-local with guard
        let tracer = provider.tracer("test");
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("trace"))
            .with(tracing_opentelemetry::layer().with_tracer(tracer));
        let guard = tracing::subscriber::set_default(subscriber);

        Self { provider, store, _guard: guard }
    }

    pub fn assert_spans(&self) -> SpanFlowAsserter {
        let _ = self.provider.force_flush();  // Critical: flush before assertions!
        SpanFlowAsserter::new(&self.store)
    }
}
```

### Fluent Assertion API

**Key types:**

- `OtelTestFixture` - Captures spans via in-memory exporter
- `SpanFlowAsserter` - Fluent API for span structure assertions
- `SpanGraph` - Indexed span storage for fast lookups

**Core assertion methods:**

```rust
// Span existence
.assert_exists("credit_onramp.insert_ledger")
.assert_not_exists("credit_onramp.some_operation")

// Parent-child relationships
.assert_child_of("child_span", "parent_span")
.assert_child_named("parent_span", "child_span")

// Span chains (A → B → C)
.assert_chain(&["root", "child", "grandchild"])

// Attribute assertions
.assert_any_attr_eq("http.route", "/webhooks/hyperswitch")
.assert_any_attr_contains("event_id", "evt_")
.assert_any_attr_key_in(&["http.route", "url.path"])

// Same trace verification
.assert_same_trace("span_a", "span_b")
```

**See**: `lib/otel-test-support/src/lib.rs` for full implementation and additional helpers.

### Example Integration Test

```rust
#[tokio::test]
async fn test_webhook_flow_with_spans() {
    let otel = OtelTestFixture::new();
    let store = CreditStore::connect(&db_url, 8).await.unwrap();

    // Execute operation that emits spans
    let outcome = store.record_and_apply(&headers, &body, &secrets).await.unwrap();
    assert_eq!(outcome, ApplyOutcome::Applied);

    // assert_spans() automatically calls provider.force_flush()!
    // No manual flush or sleep needed
    otel.assert_spans()
        .assert_exists("credit_onramp.insert_webhook_event")
        .assert_child_named("credit_onramp.insert_webhook_event", "sqlx.execute")
        .assert_exists("credit_onramp.insert_ledger")
        .assert_child_named("credit_onramp.insert_ledger", "sqlx.execute")
        .assert_any_attr_eq("event_id", "evt_abc123");
}
```

### Domain-Specific Assertions

Create high-level assertions for common flows:

```rust
impl SpanFlowAsserter<'_> {
    /// Assert complete apply flow (webhook → checks → ledger → commit)
    pub fn assert_apply_flow(&self) -> &Self {
        self
            .assert_exists("credit_onramp.insert_webhook_event")
            .assert_child_named("credit_onramp.insert_webhook_event", "sqlx.execute")
            .assert_exists("credit_onramp.duplicate_exists")
            .assert_child_named("credit_onramp.duplicate_exists", "sqlx.fetch_optional")
            .assert_exists("credit_onramp.insert_ledger")
            .assert_child_named("credit_onramp.insert_ledger", "sqlx.execute")
    }

    /// Assert no ledger writes (for validation failures)
    pub fn assert_no_apply(&self) -> &Self {
        self
            .assert_not_exists("credit_onramp.insert_ledger")
    }
}
```

**Usage in tests:**

```rust
#[tokio::test]
async fn invalid_signature_no_ledger_write() {
    let otel = OtelTestFixture::new();
    let store = CreditStore::connect(&url, 8).await.unwrap();

    let result = store.record_webhook(&bad_headers, &body, &secrets).await;
    assert!(result.is_err());

    // One-line assertion for complex invariant
    otel.assert_spans().assert_no_apply();
}
```

---

## HTTP Middleware Integration

### Layer Order Matters!

For Axum with OTel, layer order determines span parent/child relationships:

```rust
use axum::Router;
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use tower_http::trace::TraceLayer;

let app = Router::new()
    .route("/webhooks/hyperswitch", post(handler))
    // App-level: response header injection
    .layer(OtelInResponseLayer)
    // Route-level: HTTP semantic conventions (AFTER route matching)
    .route_layer(OtelAxumLayer::default())
    .route_layer(TraceLayer::new_for_http()
        .make_span_with(|req: &http::Request<_>| {
            let method = req.method().as_str();
            let route = req.extensions()
                .get::<axum::extract::MatchedPath>()
                .map(|p| p.as_str())
                .unwrap_or_else(|| req.uri().path());
            tracing::info_span!(
                "request",
                http.method = %method,
                http.route = %route,
                otel.name = %format!("{} {}", method, route),
                span.kind = %"server"
            )
        })
    );
```

**Why route_layer for TraceLayer?**

- Route matching happens BEFORE route_layer middleware
- `MatchedPath` extension is available (contains actual route like `/webhooks/hyperswitch`)
- Without route_layer, you only get raw URI (`/webhooks/hyperswitch?foo=bar`)

**Span hierarchy:**

```
request (http.method=POST, http.route=/webhooks/hyperswitch)
├── credit_onramp.insert_webhook_event
│   └── sqlx.execute
├── credit_onramp.fetch_webhook_meta
│   └── sqlx.fetch_optional
├── credit_onramp.insert_ledger
│   └── sqlx.execute
└── credit_onramp.mark_webhook_applied
    └── sqlx.execute
```

---

## Testing HTTP Middleware

Test that HTTP spans have correct attributes:

```rust
#[tokio::test]
async fn http_span_has_semantic_attributes() {
    let otel = OtelTestFixture::new();

    // Build router with same middleware as production
    #[allow(clippy::default_constructed_unit_structs)]
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .layer(OtelInResponseLayer)
        .route_layer(OtelAxumLayer::default())
        .route_layer(TraceLayer::new_for_http().make_span_with(|req| {
            let route = req.extensions()
                .get::<MatchedPath>()
                .map(|p| p.as_str())
                .unwrap_or(req.uri().path());
            tracing::info_span!(
                "request",
                http.method = %req.method(),
                http.route = %route,
            )
        }));

    let req = http::Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // assert_spans() auto-flushes - no sleep needed!
    let spans = otel.assert_spans();
    let graph = spans.graph();
    let http_span = graph.first_by_name("GET");  // OtelAxumLayer creates this

    assert!(http_span.attributes.contains_key("http.route"));
    assert_eq!(http_span.attributes.get("http.request.method"), Some(&"GET".to_string()));
    assert_eq!(http_span.attributes.get("url.path"), Some(&"/health".to_string()));
}
```

---

## Common Pitfalls & Solutions

### 1. Spans Not Captured in Tests

**Problem**: `otel.assert_spans()` shows empty, but spans are emitted in production.

**Solutions**:

- ✅ **Our fixture auto-flushes**: `assert_spans()` calls `provider.force_flush()` internally - no manual flush needed!
- Set both global AND thread-local subscribers in fixture (we do this)
- Check that `sqlx-tracing` pool is used, not raw `sqlx::Pool`

**Note**: The `OtelTestFixture::assert_spans()` method handles all flushing automatically. You never need to call `force_flush()` or add sleep delays in tests.

### 2. String Interpolation in Logs

**Problem**:

```rust
tracing::info!("Processing event {} for account {}", eid, account);  // ❌
```

**Solution**:

```rust
tracing::info!(event_id = eid, account_id = %account, "event processed");  // ✅
```

**Why**: Structured fields allow filtering/aggregation in Tempo/Loki:

```
{span:event_id = "evt_abc123"}  // Works
{span.message =~ ".*evt_abc123.*"}  // Doesn't work, message is unstructured
```

### 3. High-Cardinality Span Names

**Problem**:

```rust
tracing::info_span!("webhook_{}", event_id);  // ❌ Creates millions of unique names!
```

**Solution**:

```rust
tracing::info_span!("credit_onramp.insert_webhook", event_id = event_id);  // ✅
```

**Why**: Span names are indexed; high cardinality breaks Tempo/Jaeger storage.

### 4. Missing Async Propagation

**Problem**: Spans created but not entered, so children aren't linked.

```rust
async fn bad_example(&self) {
    let span = tracing::info_span!("parent");
    // Forgot to enter! Children won't be linked.
    self.child().await;
}
```

**Solution**:

```rust
async fn good_example(&self) {
    let span = tracing::info_span!("parent");
    let _guard = span.enter();  // ✅ Entered, children auto-link
    self.child().await;
}
```

### 5. OtelAxumLayer Creates Duplicate Spans

**Problem**: Both TraceLayer and OtelAxumLayer create root spans.

**Solution**: Let OtelAxumLayer create the span, TraceLayer can enrich it via custom `make_span_with`:

```rust
.route_layer(OtelAxumLayer::default())  // Creates span with HTTP attrs
.route_layer(TraceLayer::new_for_http()
    .make_span_with(|req| {
        // Enrich the existing span, don't create a new one
        tracing::Span::current()
    })
)
```

Or use TraceLayer exclusively if you don't need full HTTP semantic conventions.

---

## Observability Checklist

Before shipping instrumented code, verify:

- [ ] **Span names are static** (no runtime formatting)
- [ ] **All dynamic data in fields**, not span names or log messages
- [ ] **Parent-child relationships tested** (use `assert_child_named`)
- [ ] **Attributes use semantic conventions** (`http.route`, not `url` or `path`)
- [ ] **Spans propagate across async boundaries** (test with nested async calls)
- [ ] **Force flush in test fixtures** before assertions
- [ ] **HTTP middleware layer order correct** (route_layer for MatchedPath)
- [ ] **No string interpolation in tracing macros** (use `event_id = eid`, not `"event {}", eid`)

---

## Example: Complete Flow

### Business Logic (`lib.rs`)

```rust
pub async fn record_and_apply(
    &self,
    headers: &HeaderMap,
    body: &[u8],
    secrets: &[String],
) -> Result<ApplyOutcome> {
    // 1. Record webhook
    let span = spans::insert_webhook_event(&event_id);
    let _guard = span.enter();
    self.q_insert_webhook(&cmd).await?;
    drop(_guard);

    // 2. Apply in transaction
    let span = spans::apply_transaction(&event_id);
    let _guard = span.enter();
    let mut tx = self.pool.begin().await?;

    // Child spans auto-link to apply_transaction
    let outcome = self.apply_impl(&mut tx, &event_id).await?;

    tx.commit().await?;
    Ok(outcome)
}
```

### Span Module (`spans.rs`)

```rust
pub(crate) fn insert_webhook_event(event_id: &str) -> Span {
    tracing::debug_span!(
        "credit_onramp.insert_webhook_event",
        event_id = event_id,
    )
}

pub(crate) fn apply_transaction(event_id: &str) -> Span {
    tracing::debug_span!(
        "credit_onramp.apply_transaction",
        event_id = event_id,
    )
}
```

### Integration Test

```rust
#[tokio::test]
async fn test_full_flow() {
    let otel = OtelTestFixture::new();
    let store = CreditStore::connect(&url, 8).await.unwrap();

    let outcome = store.record_and_apply(&headers, &body, &secrets).await.unwrap();
    assert_eq!(outcome, ApplyOutcome::Applied);

    otel.assert_spans()
        .assert_exists("credit_onramp.insert_webhook_event")
        .assert_child_named("credit_onramp.insert_webhook_event", "sqlx.execute")
        .assert_exists("credit_onramp.apply_transaction")
        .assert_child_named("credit_onramp.apply_transaction", "sqlx.execute");
}
```

---

## Grafana Integration

### TraceQL Query Syntax

**Simple query by trace name** (for Tempo 2.x+):

```traceql
{ resource.service.name = "payments-hub-backend" && name = "POST /webhooks/hyperswitch" }
```

**Alternative - filter by HTTP route attribute:**

```traceql
{ resource:service.name = "payments-hub-backend" && span:http.route = "/webhooks/hyperswitch" }
```

**Note**: Root span name is usually simpler and more reliable than attribute filtering. Use `name` for trace-level queries, `span:attribute` for span-level filtering.

### Dashboard Panel Example

```json
{
  "type": "traces",
  "title": "Webhook Traces",
  "datasource": { "type": "tempo", "uid": "tempo" },
  "targets": [
    {
      "query": "{ resource:service.name = \"payments-hub-backend\" && span:http.route = \"/webhooks/hyperswitch\" }",
      "queryType": "traceql",
      "refId": "A"
    }
  ]
}
```

---

## Per-layer env filters (baml-rt binaries)

[`baml_rt_observability::init_tracing`](crates/baml-rt-observability/src/tracing_setup.rs) wires **separate** [`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/struct.EnvFilter.html) instances for the console (`fmt`) layer and the OpenTelemetry trace layer:

| Variable | Layer | When unset |
|----------|--------|--------------|
| `RUST_LOG_FMT` | Console only | `RUST_LOG` via `from_default_env()`, then defaults: `baml_rt=info`, `baml_rt_quickjs=debug`, `baml_rt_interceptor=debug`, `baml_agent_runner=info`, QuickJS adapter crates `warn`. |
| `RUST_LOG_OTEL` | OTLP span export only | `info` plus QuickJS adapter `warn` — so `RUST_LOG=debug` for local logs does **not** automatically export every `debug` span to the collector. |

**Example:** verbose console, slimmer traces:

```bash
export RUST_LOG_FMT=debug,baml_rt_quickjs=trace
export RUST_LOG_OTEL=info,baml_rt=debug
```

Constants `RUST_LOG_FMT_ENV` and `RUST_LOG_OTEL_ENV` in `tracing_setup.rs` match these names.

### Span and log levels (`baml_rt_observability` + runtime)

- **Ingress / RPC roots** (`a2a_request`, `a2a_stream`, `a2a_cancel`, `a2a_stdio_request`): `info_span!`.
- **Agent execution and inner routing** (`invoke_function`, `a2a_route`, `a2a_js_invoke`, `init_baml_runtime`): `debug_span!` so default `RUST_LOG_OTEL=info` traces stay operational; widen `RUST_LOG_OTEL` for deep span export.
- **Interceptor and QuickJS** payload-style logs: `debug!` (see `baml_rt_interceptor::interceptors::tracing`, `baml_rt_quickjs`); provenance remains the transcript of record.

---

## References

- **Semantic Conventions**: https://opentelemetry.io/docs/specs/semconv/http/
- **TraceQL Syntax**: https://grafana.com/docs/tempo/latest/traceql/
- **tracing crate**: https://docs.rs/tracing/latest/tracing/
- **sqlx-tracing**: Auto-instruments database queries as child spans
- **axum-tracing-opentelemetry**: HTTP semantic conventions for Axum

---

## Summary

**Golden Rules:**

1. **Separate spans module** - keep instrumentation orthogonal
2. **Static span names** - namespace.operation format
3. **Structured fields** - NEVER string interpolation
4. **Test span structure** - verify parent/child relationships
5. **Force flush** - before assertions in tests
6. **Correct layer order** - route_layer for route-aware spans
7. **Semantic conventions** - use `http.route`, `http.method`, etc.

**This pattern gives you:**

- Production-grade observability
- Low-cardinality, queryable traces
- Fast, deterministic tests
- Clean separation of concerns
- Future-proof for Tempo/Jaeger/Zipkin

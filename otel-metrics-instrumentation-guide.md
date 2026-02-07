# OpenTelemetry Metrics Instrumentation Guide

**Patterns for instrumenting Rust crates with production-grade OTel metrics.**

Based on `credit-accounting`, `credit-onramp`, and `database-support` implementations: orthogonal metrics module, OpenTelemetry native API, structured attributes, and testability.

---

## Design Goals

1. **Separation of Concerns**: Metrics instrumentation lives in a separate module, not mixed with business logic
2. **Machine-Parseable**: All attributes are structured fields (no string interpolation in metric names)
3. **OpenTelemetry Native**: Use OTEL's Meter API directly, not third-party bridges
4. **Testable**: Metric structure and attributes are verified in tests
5. **Low Cardinality**: Metric names and attribute keys are static; dynamic data goes in attribute _values_
6. **Production-Ready**: Appropriate but not excessive metrics that surface operational insights

---

## Architecture Pattern

### Module Structure

```
my_crate/
├── src/
│   ├── lib.rs         # Business logic (DB queries, conversions, etc.)
│   ├── metrics.rs     # OTel metrics helpers (orthogonal to business logic)
│   └── tests/
│       └── integration.rs
```

**Why separate?**

- Business logic functions stay clean and focused
- Metric naming and attribute schemas are centralized
- Easy to audit and maintain instrumentation
- Can disable/swap metrics without touching core logic

### The `metrics.rs` Module

Create a dedicated module with metric recording helpers:

```rust
// src/metrics.rs
use opentelemetry::{global, KeyValue};
use std::time::Duration;

/// Record advisory lock acquisition timing
pub fn record_advisory_lock_acquired(operation: &str, wait_duration: Duration) {
    let meter = global::meter("credit_accounting");

    let histogram = meter
        .f64_histogram("accounting.advisory_lock.wait_ms")
        .build();
    let counter = meter
        .u64_counter("accounting.advisory_lock.acquired_total")
        .build();

    histogram.record(
        wait_duration.as_millis() as f64,
        &[KeyValue::new("operation", operation.to_string())],
    );
    counter.add(1, &[KeyValue::new("operation", operation.to_string())]);
}

/// Record operation completion with result
pub fn record_operation_complete(operation: &str, result: &str, duration: Duration) {
    let meter = global::meter("credit_accounting");

    let histogram = meter
        .f64_histogram("accounting.operation.duration_ms")
        .build();
    let counter = meter.u64_counter("accounting.operation.total").build();

    let attributes = &[
        KeyValue::new("operation", operation.to_string()),
        KeyValue::new("result", result.to_string()),
    ];

    histogram.record(duration.as_millis() as f64, attributes);
    counter.add(1, attributes);
}
```

**Key principles:**

- Static metric names (`"accounting.advisory_lock.wait_ms"`)
- Namespace prefix prevents collisions (`accounting.` vs `onramp.`)
- All dynamic data goes in **attributes**, not metric names
- Use appropriate metric types (counter, histogram, gauge)
- Document what each metric measures

---

## Metric Naming Convention

**Format**: `{service_name}.{domain}.{metric_type}`

### Examples

✅ **Good:**

```rust
// Service-specific namespacing
"credit_accounting.operation.duration_ms"
"credit_accounting.advisory_lock.acquired_total"
"credit_onramp.operation.total"
"database_support.pool.connections.idle"
```

❌ **Bad:**

```rust
// Missing namespace
"operation_duration"  // Collision risk!

// Too vague
"accounting"  // What about accounting?

// Dynamic name (high cardinality!)
format!("operation_{}", op_name)  // Creates millions of metrics!
```

### Metric Types

- **Counter**: Always-increasing values (operation counts, errors)
- **Histogram**: Duration, size, latency distributions
- **Gauge**: Current state values (connection pool size, memory usage)

---

## Structured Attributes

**CRITICAL**: Use typed attributes, NEVER string interpolation in metric names.

### ✅ Correct Patterns

```rust
// Operation-specific attributes
let attributes = &[
    KeyValue::new("operation", "process_payment"),
    KeyValue::new("result", "success"),
];

// Resource-specific attributes
let attributes = &[
    KeyValue::new("pool_name", "main"),
    KeyValue::new("state", "idle"),
];

// Error-specific attributes
let attributes = &[
    KeyValue::new("error_type", "insufficient_balance"),
    KeyValue::new("operation", "transfer"),
];
```

### ❌ Anti-Patterns

```rust
// NEVER: String interpolation in metric names
let metric_name = format!("operation_{}", op);  // ❌ High cardinality!

// NEVER: Runtime formatting in metric names
meter.f64_counter(&format!("{}_count", operation));  // ❌ Not machine-parseable!

// NEVER: Positional arguments
counter.add(1, &[KeyValue::new("op", operation)]);  // ❌ Use descriptive names!
```

### Correct Metric Recording

```rust
// ✅ Structured attributes, static metric name
pub fn record_payment_processed(amount: i128, duration: Duration) {
    let meter = global::meter("credit_accounting");
    let histogram = meter
        .f64_histogram("accounting.payment.duration_ms")
        .build();

    histogram.record(
        duration.as_millis() as f64,
        &[KeyValue::new("amount_bucket", bucket_amount(amount))],
    );
}
```

---

## Instrumenting Business Logic

### The Orthogonal Pattern (RECOMMENDED)

**Always use the metrics module + direct calls.** Never embed metric recording in business logic.

```rust
pub async fn process_payment(&self, from: &str, to: &str, amount: i128) -> Result<()> {
    let start = std::time::Instant::now();

    // Business logic here
    let result = self.transfer_impl(from, to, amount).await;

    // Record metrics after operation
    let duration = start.elapsed();
    match &result {
        Ok(_) => metrics::record_operation_complete("process_payment", "success", duration),
        Err(AccountingError::InsufficientBalance) => {
            metrics::record_operation_complete("process_payment", "insufficient_balance", duration)
        }
        Err(_) => metrics::record_operation_complete("process_payment", "error", duration),
    }

    result
}
```

**Why this pattern?**

✅ **Separation**: Instrumentation lives in `metrics.rs`, not scattered across business logic  
✅ **Centralized**: All metric names and attribute schemas in one place  
✅ **Clean**: Business logic stays focused on business concerns

### ❌ Anti-Pattern: Inline Metrics

**Don't do this:**

```rust
// ❌ BAD: Mixes metrics into business logic!
pub async fn process_payment(&self, from: &str, to: &str, amount: i128) -> Result<()> {
    let meter = global::meter("credit_accounting");
    let counter = meter.u64_counter("payment_count").build();

    // Business logic mixed with instrumentation
    counter.add(1, &[KeyValue::new("from", from.to_string())]);

    // ... business logic ...
}
```

**Why avoid it:**

❌ Hard to audit all instrumentation (scattered across files)  
❌ Violates separation of concerns

---

## Performance: Instrument Caching with `OnceLock`

### The Problem: Repeated Instrument Creation

Creating metric instruments on every call is **expensive and wasteful**:

```rust
// ❌ BAD: Creates new instruments every time function is called!
pub fn record_operation(operation: &str, duration: Duration) {
    let meter = global::meter("my_service");
    let histogram = meter.f64_histogram("operation.duration_ms").build(); // ❌ Allocates!
    let counter = meter.u64_counter("operation.total").build();           // ❌ Allocates!

    histogram.record(duration.as_millis() as f64, &[...]);
    counter.add(1, &[...]);
}
```

**Why this is bad:**

- Creates new `Meter` instance every call
- Builds new `Histogram` and `Counter` instances every call
- Allocates memory on hot path
- Unnecessary synchronization overhead

---

### The Solution: Static Instrument Caching

Use `std::sync::OnceLock` to cache instruments once and reuse forever:

```rust
use opentelemetry::{global, KeyValue};
use opentelemetry::metrics::{Counter, Histogram};
use std::sync::OnceLock;
use std::time::Duration;

// Static caches - initialized once, reused forever
static OPERATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static OPERATION_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();

// Getter functions - initialize on first call, return cached reference after
fn operation_histogram() -> &'static Histogram<f64> {
    OPERATION_HISTOGRAM.get_or_init(|| {
        global::meter("my_service")
            .f64_histogram("operation.duration_ms")
            .build()
    })
}

fn operation_counter() -> &'static Counter<u64> {
    OPERATION_COUNTER.get_or_init(|| {
        global::meter("my_service")
            .u64_counter("operation.total")
            .build()
    })
}

// ✅ GOOD: Reuses cached instruments!
pub fn record_operation(operation: &str, duration: Duration) {
    let attrs = &[KeyValue::new("operation", operation.to_string())];
    operation_histogram().record(duration.as_millis() as f64, attrs);
    operation_counter().add(1, attrs);
}
```

---

### Performance Benefits

✅ **Initialized once**: First call creates instrument, all subsequent calls reuse it  
✅ **Thread-safe**: `OnceLock` handles concurrent initialization safely  
✅ **Zero allocation**: No memory allocations on hot path after first call  
✅ **Zero overhead**: After initialization, it's just a pointer dereference  
✅ **Simple**: No manual locking or synchronization needed

---

### Implementation Pattern

**For every metrics module, follow this pattern:**

1. **Declare static `OnceLock` for each instrument:**

   ```rust
   static MY_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
   static MY_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
   static MY_GAUGE: OnceLock<Gauge<f64>> = OnceLock::new();
   ```

2. **Create private getter functions:**

   ```rust
   fn my_histogram() -> &'static Histogram<f64> {
       MY_HISTOGRAM.get_or_init(|| {
           global::meter("service_name")
               .f64_histogram("metric.name")
               .build()
       })
   }
   ```

3. **Use getters in public recording functions:**
   ```rust
   pub fn record_something(value: f64) {
       my_histogram().record(value, &[]);
   }
   ```

---

### Real-World Example: Connection Pool Metrics

```rust
use opentelemetry::metrics::Gauge;
use std::sync::OnceLock;
use sqlx::PgPool;

static POOL_IDLE_GAUGE: OnceLock<Gauge<f64>> = OnceLock::new();
static POOL_ACTIVE_GAUGE: OnceLock<Gauge<f64>> = OnceLock::new();

fn pool_idle_gauge() -> &'static Gauge<f64> {
    POOL_IDLE_GAUGE.get_or_init(|| {
        global::meter("database_support")
            .f64_gauge("db.pool.connections.idle")
            .build()
    })
}

fn pool_active_gauge() -> &'static Gauge<f64> {
    POOL_ACTIVE_GAUGE.get_or_init(|| {
        global::meter("database_support")
            .f64_gauge("db.pool.connections.active")
            .build()
    })
}

// Called every 15 seconds - zero allocation overhead!
pub fn record_pool_state(pool: &PgPool) {
    let num_idle = pool.num_idle() as f64;
    let num_active = (pool.size() - pool.num_idle()) as f64;

    pool_idle_gauge().record(num_idle, &[]);
    pool_active_gauge().record(num_active, &[]);
}
```

**Why this matters for pool metrics:**

- Pool state is recorded every 15-30 seconds
- Without caching: 2 allocations every 15 seconds = memory churn
- With caching: Zero allocations after first call = perfect for background tasks

---

## Setup Requirements

### Cargo.toml Dependencies

```toml
[dependencies]
opentelemetry = { workspace = true }  # Use OTEL native API
# Don't use metrics crate - use OTEL directly!
```

**Why OTEL native over `metrics` crate?**

✅ **No bridge needed**: Direct export via OTLP  
✅ **Semantic conventions**: Built-in support for standard attributes  
✅ **Future-proof**: OTEL is the industry standard  
✅ **Better integration**: Works seamlessly with traces  
✅ **Cacheable instruments**: Can use `OnceLock` for zero-allocation metrics (not possible with `metrics` crate macros)

### Server Telemetry Setup

```rust
// apps/server/src/telemetry.rs
pub fn setup_metrics() -> SdkMeterProvider {
    // ... OTLP setup ...

    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource_from_env_or_default("server"))
        .build();

    global::set_meter_provider(provider.clone());

    // No bridge needed - metrics flow directly via OTLP!
    provider
}
```

---

## Common Metric Patterns

### Infrastructure Metrics Pattern

For infrastructure components (pools, locks, queues), record current state:

```rust
// src/metrics.rs
pub fn record_resource_state(resource_name: &str, current: u32, max: u32) {
    let meter = global::meter("my_service");

    let current_gauge = meter.f64_gauge("resource.current").build();
    let utilization_gauge = meter.f64_gauge("resource.utilization_ratio").build();

    current_gauge.record(current as f64, &[KeyValue::new("resource", resource_name.to_string())]);
    utilization_gauge.record(
        current as f64 / max as f64,
        &[KeyValue::new("resource", resource_name.to_string())],
    );
}
```

### Operation Metrics Pattern

For business operations, record both counts and timing:

```rust
// src/metrics.rs
pub fn record_operation(operation: &str, result: &str, duration: Duration) {
    let meter = global::meter("my_service");

    let histogram = meter.f64_histogram("operation.duration_ms").build();
    let counter = meter.u64_counter("operation.total").build();

    let attributes = &[
        KeyValue::new("operation", operation.to_string()),
        KeyValue::new("result", result.to_string()),
    ];

    histogram.record(duration.as_millis() as f64, attributes);
    counter.add(1, attributes);
}
```

### Periodic Recording Pattern

For infrastructure metrics, record periodically in background:

```rust
// In your server startup
let metrics_task = tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(15));
    loop {
        interval.tick().await;
        metrics::record_resource_state("connection_pool", idle_count, max_connections);
    }
});
```

---

## Common Pitfalls & Solutions

### 1. High Cardinality Attributes

**Problem**: Too many unique attribute values break Prometheus storage.

```rust
// ❌ BAD: High cardinality
KeyValue::new("user_id", user_id.to_string())  // Millions of users!
KeyValue::new("account_id", account_id.to_string())  // Millions of accounts!
```

**Solution**: Use bucketing or filtering:

```rust
// ✅ GOOD: Low cardinality
KeyValue::new("amount_bucket", bucket_amount(amount))  // 0-1k, 1k-10k, 10k+
KeyValue::new("operation", "process_payment")  // Fixed set of operations
```

### 2. Missing Duration Recording

**Problem**: Recording counts but not timing information.

```rust
// ❌ BAD: Only counts
counter.add(1, &[KeyValue::new("operation", "transfer")]);
```

**Solution**: Record both counts and durations:

```rust
// ✅ GOOD: Counts + timing
let start = std::time::Instant::now();
// ... operation ...
let duration = start.elapsed();

counter.add(1, &[KeyValue::new("operation", "transfer")]);
histogram.record(duration.as_millis() as f64, &[KeyValue::new("operation", "transfer")]);
```

### 3. Inconsistent Attribute Names

**Problem**: Same attribute with different names across metrics.

```rust
// ❌ BAD: Inconsistent naming
KeyValue::new("op", "transfer")      // Some metrics
KeyValue::new("operation", "transfer")  // Other metrics
```

**Solution**: Standardize attribute names:

```rust
// ✅ GOOD: Consistent naming
KeyValue::new("operation", "transfer")  // All metrics use same attribute name
```

### 4. Missing Error Metrics

**Problem**: Only recording success cases.

```rust
// ❌ BAD: Only success
metrics::record_operation_complete("transfer", "success", duration);
```

**Solution**: Record all outcomes:

```rust
// ✅ GOOD: All outcomes
match result {
    Ok(_) => metrics::record_operation_complete("transfer", "success", duration),
    Err(AccountingError::InsufficientBalance) => {
        metrics::record_operation_complete("transfer", "insufficient_balance", duration)
    }
    Err(_) => metrics::record_operation_complete("transfer", "error", duration),
}
```

---

## Observability Checklist

Before shipping instrumented code, verify:

- [ ] **Metric names are static** (no runtime formatting like `format!("operation_{}", name)`)
- [ ] **All dynamic data in attributes**, not metric names
- [ ] **Low cardinality attributes** (avoid unique identifiers like user IDs, use bucketing instead)
- [ ] **Both counts and durations recorded** for operations (counters + histograms)
- [ ] **All error cases instrumented** (success, failure, specific error types)
- [ ] **Infrastructure metrics recorded periodically** (connection pools, queues, etc.)
- [ ] **Timing metrics include acquisition time** (locks, resources, etc.)
- [ ] **Metric names follow consistent convention** (e.g., `service.domain.metric_type`)
- [ ] **Attributes use consistent names** across all metrics (e.g., always `operation`, not `op`)
- [ ] **Instruments cached with `OnceLock`** (no repeated creation on hot path)
- [ ] **Metrics flow to observability backend** (verify in dashboards)

---

## Example: Complete Flow

### Business Logic (`lib.rs`)

```rust
pub async fn process_request(&self, input: &Request) -> Result<Response> {
    let start = std::time::Instant::now();

    // Business logic here
    let result = self.process_impl(input).await;

    // Record metrics
    let duration = start.elapsed();
    match &result {
        Ok(_) => metrics::record_operation("process_request", "success", duration),
        Err(Error::ValidationFailed) => {
            metrics::record_operation("process_request", "validation_failed", duration)
        }
        Err(_) => metrics::record_operation("process_request", "error", duration),
    }

    result
}
```

### Metrics Module (`metrics.rs`)

```rust
use opentelemetry::{global, KeyValue};
use std::time::Duration;

pub fn record_operation(operation: &str, result: &str, duration: Duration) {
    let meter = global::meter("my_service");

    let histogram = meter.f64_histogram("operation.duration_ms").build();
    let counter = meter.u64_counter("operation.total").build();

    let attributes = &[
        KeyValue::new("operation", operation.to_string()),
        KeyValue::new("result", result.to_string()),
    ];

    histogram.record(duration.as_millis() as f64, attributes);
    counter.add(1, attributes);
}
```

### Testing Pattern

```bash
# 1. Generate traffic to create metrics
# 2. Check Prometheus endpoint for metric values
# 3. Verify metrics display correctly in dashboards
```

---

## References

- **OpenTelemetry Metrics API**: https://opentelemetry.io/docs/specs/otel/metrics/api/
- **Semantic Conventions**: https://opentelemetry.io/docs/specs/semconv/
- **Prometheus Query Language**: https://prometheus.io/docs/prometheus/latest/querying/basics/
- **Grafana Dashboard JSON**: https://grafana.com/docs/grafana/latest/dashboards/json-model/

---

## Summary

**Golden Rules:**

1. **Separate metrics module** - keep instrumentation orthogonal
2. **Static metric names** - service.domain.metric_type format
3. **Structured attributes** - NEVER string interpolation in names
4. **Low cardinality attributes** - avoid user IDs, use bucketing
5. **Record both counts and durations** for operations
6. **Instrument all outcomes** (success, failure, specific errors)
7. **Use OTEL native API** - no bridges needed

**This pattern gives you:**

- Production-grade operational metrics
- Low-cardinality, queryable metrics
- Clean separation of concerns
- Future-proof for Prometheus/Grafana
- Rich operational insights (pools, locks, business operations)

**Note**: Metrics are simpler than spans - no hierarchy to preserve, just record counts/timings at operation boundaries.

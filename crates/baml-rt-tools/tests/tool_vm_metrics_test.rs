// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! v2 tool VM metrics (`baml_rt_tools` meter): pool lifecycle, in-flight
//! calls, grant denials, and guest-reported usage.
//!
//! One test in its own binary: the OpenTelemetry meter provider is process
//! global, so sharing a process with other tests would mix their VMs into
//! these counts.

mod support;

use std::sync::{Arc, Weak};

use baml_rt_tools::external_tools::host::{PoolPolicy, VmPool};
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::{
    Resource,
    metrics::{
        InstrumentKind, ManualReader, MeterProviderBuilder, Pipeline,
        data::{Histogram, ResourceMetrics, Sum, Temporality},
        reader::{MetricReader, TemporalitySelector},
    },
};
use serde_json::json;
use support::host_v2::*;

/// `ManualReader` is not `Clone`; share one between the provider and the test.
#[derive(Debug, Clone)]
struct SharedReader(Arc<ManualReader>);

impl TemporalitySelector for SharedReader {
    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.0.temporality(kind)
    }
}

impl MetricReader for SharedReader {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.0.register_pipeline(pipeline);
    }
    fn collect(&self, rm: &mut ResourceMetrics) -> opentelemetry::metrics::Result<()> {
        self.0.collect(rm)
    }
    fn force_flush(&self) -> opentelemetry::metrics::Result<()> {
        self.0.force_flush()
    }
    fn shutdown(&self) -> opentelemetry::metrics::Result<()> {
        self.0.shutdown()
    }
}

struct Metrics(ResourceMetrics);

impl Metrics {
    fn collect(reader: &SharedReader) -> Self {
        let mut rm = ResourceMetrics {
            resource: Resource::empty(),
            scope_metrics: Vec::new(),
        };
        reader.collect(&mut rm).expect("collect metrics");
        Self(rm)
    }

    fn data(&self, name: &str) -> Option<&dyn opentelemetry_sdk::metrics::data::Aggregation> {
        self.0
            .scope_metrics
            .iter()
            .flat_map(|scope| &scope.metrics)
            .find(|metric| metric.name == name)
            .map(|metric| metric.data.as_ref())
    }

    /// Value of the data point whose attributes include all of `labels`.
    fn sum_u64(&self, name: &str, labels: &[KeyValue]) -> u64 {
        let Some(sum) = self
            .data(name)
            .and_then(|d| d.as_any().downcast_ref::<Sum<u64>>())
        else {
            return 0;
        };
        sum.data_points
            .iter()
            .filter(|p| labels.iter().all(|l| p.attributes.contains(l)))
            .map(|p| p.value)
            .sum()
    }

    fn sum_i64(&self, name: &str, labels: &[KeyValue]) -> i64 {
        let Some(sum) = self
            .data(name)
            .and_then(|d| d.as_any().downcast_ref::<Sum<i64>>())
        else {
            return 0;
        };
        sum.data_points
            .iter()
            .filter(|p| labels.iter().all(|l| p.attributes.contains(l)))
            .map(|p| p.value)
            .sum()
    }

    /// (count, sum) of a histogram's points matching `labels`.
    fn histogram(&self, name: &str, labels: &[KeyValue]) -> (u64, f64) {
        let Some(h) = self
            .data(name)
            .and_then(|d| d.as_any().downcast_ref::<Histogram<f64>>())
        else {
            return (0, 0.0);
        };
        h.data_points
            .iter()
            .filter(|p| labels.iter().all(|l| p.attributes.contains(l)))
            .fold((0, 0.0), |(count, sum), p| (count + p.count, sum + p.sum))
    }
}

fn kv(key: &'static str, value: &'static str) -> KeyValue {
    KeyValue::new(key, value)
}

#[tokio::test]
async fn tool_vm_metrics_track_lifecycle_calls_denials_and_usage() {
    let reader = SharedReader(Arc::new(ManualReader::builder().build()));
    global::set_meter_provider(
        MeterProviderBuilder::default()
            .with_reader(reader.clone())
            .build(),
    );

    // 1. One invoke: a cold boot joins the pool; the lease is released after.
    let (f, ctx) = fixture(full_request(), policy(full_request()), "s3cret");
    invoke(&f, &ctx, json!({"q": 1})).await;
    let m = Metrics::collect(&reader);
    assert_eq!(
        m.sum_u64(
            "baml_rt_tools.tool_vm.lifecycle.total",
            &[kv("event", "created"), kv("reason", "cold_boot")]
        ),
        1
    );
    assert_eq!(
        m.sum_i64(
            "baml_rt_tools.tool_vm.active",
            &[kv("trust_domain", "third-party")]
        ),
        1
    );
    assert_eq!(m.sum_i64("baml_rt_tools.tool_vm.calls_inflight", &[]), 0);
    assert_eq!(
        m.histogram(
            "baml_rt_tools.tool_vm.boot.duration_ms",
            &[kv("result", "ok")]
        )
        .0,
        1
    );
    // The guest's `usage` event is kept, not dropped.
    assert_eq!(m.histogram("baml_rt_tools.tool_call.cpu_ms", &[]), (1, 7.0));
    assert_eq!(
        m.histogram("baml_rt_tools.tool_call.peak_rss_kib", &[]),
        (1, 2048.0)
    );

    // 2. A warm second call reuses the VM: no new boot.
    invoke(&f, &ctx, json!({"q": 2})).await;
    let m = Metrics::collect(&reader);
    assert_eq!(
        m.sum_u64(
            "baml_rt_tools.tool_vm.lifecycle.total",
            &[kv("event", "created")]
        ),
        1
    );

    // 3. Denials carry a bounded reason.
    let stranger = session_ctx("s3cret");
    assert!(
        f.host
            .invoke(
                &f.snapshot,
                &f.bytes,
                &stranger,
                Default::default(),
                json!({})
            )
            .await
            .is_err()
    );
    let mut narrow = policy(full_request());
    narrow.capabilities.net.clear();
    let (widened, widened_ctx) = fixture(full_request(), narrow, "s3cret");
    assert!(
        widened
            .host
            .invoke(
                &widened.snapshot,
                &widened.bytes,
                &widened_ctx,
                Default::default(),
                json!({})
            )
            .await
            .is_err()
    );
    let m = Metrics::collect(&reader);
    let denied = |reason| {
        m.sum_u64(
            "baml_rt_tools.tool_grant.denied.total",
            &[kv("reason", reason)],
        )
    };
    assert_eq!(denied("unauthorized_agent"), 1);
    assert_eq!(denied("capability_outside_policy"), 1);

    // 4. Shutdown evicts with its reason and the active gauge returns to zero.
    f.host.shutdown().await.expect("shutdown");
    let m = Metrics::collect(&reader);
    assert_eq!(
        m.sum_u64(
            "baml_rt_tools.tool_vm.lifecycle.total",
            &[kv("event", "evicted"), kv("reason", "shutdown")]
        ),
        1
    );
    assert_eq!(m.sum_i64("baml_rt_tools.tool_vm.active", &[]), 0);

    // 5. Start-time sweep counts reclaimed leftovers (the fake reports 2).
    let _pool = VmPool::start(f.vm.clone(), "runner-a", PoolPolicy::default()).await;
    let m = Metrics::collect(&reader);
    assert_eq!(
        m.sum_u64(
            "baml_rt_tools.tool_vm.lifecycle.total",
            &[kv("event", "swept"), kv("reason", "previous_run")]
        ),
        2
    );
}

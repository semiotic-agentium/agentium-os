//! End-to-end webhook → IngressStore → producer tests for `support/grafana-alerts`.

use std::{future::Future, sync::Mutex};

use baml_rt_tools::{EventProducer, ProducerCheckpoint};
use baml_tools_grafana_alerts::{
    DEFAULT_SOURCE_KEY, GRAFANA_ALERT_SCHEMA_VERSION, GRAFANA_ROUTING_KEY, GRAFANA_SOURCE_KIND,
    GrafanaAlert, GrafanaAlertEventProducer, GrafanaWebhookPayload, MappingStore, enqueue_webhook,
    test_support::install_memory_ingress_store,
};

// Producer/ingress globals make these tests inherently sequential.
fn test_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn run_serial_test(test: impl Future<Output = ()>) {
    let _guard = test_lock().lock().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

fn alert(fingerprint: &str, status: &str, starts_at: &str) -> GrafanaAlert {
    GrafanaAlert {
        status: Some(status.to_string()),
        labels: serde_json::json!({"alertname": "HighLatency", "service": "checkout-api"}),
        annotations: serde_json::json!({"summary": "p95 latency high"}),
        starts_at: Some(starts_at.to_string()),
        ends_at: None,
        fingerprint: Some(fingerprint.to_string()),
        dashboard_url: Some("https://grafana.local/d/abc".to_string()),
        panel_url: None,
    }
}

fn payload(status: &str, group_key: &str, alerts: Vec<GrafanaAlert>) -> GrafanaWebhookPayload {
    GrafanaWebhookPayload {
        status: Some(status.to_string()),
        group_key: Some(group_key.to_string()),
        receiver: Some("agentium-webhook".to_string()),
        alerts,
    }
}

#[test]
fn enqueue_then_poll_emits_grafana_alert_event() {
    run_serial_test(async {
        let (_store_guard, store) = install_memory_ingress_store();
        let mapping = MappingStore::open_in_memory().expect("mapping store");

        let p = payload(
            "firing",
            "group-key-1",
            vec![alert("fp1", "firing", "2026-05-25T12:00:00Z")],
        );
        let outcome = enqueue_webhook(
            &p,
            &mapping,
            store.as_ref(),
            DEFAULT_SOURCE_KEY,
            1_700_000_000,
        )
        .await
        .expect("enqueue");
        assert_eq!(outcome.enqueued, 1);
        assert_eq!(outcome.duplicates, 0);

        let producer = GrafanaAlertEventProducer::new().expect("producer");
        let poll = producer
            .poll(&ProducerCheckpoint::none())
            .await
            .expect("poll");
        assert_eq!(poll.events.len(), 1);
        let event = &poll.events[0];
        assert_eq!(event.schema_version.as_str(), GRAFANA_ALERT_SCHEMA_VERSION);
        assert_eq!(event.routing_key.as_str(), GRAFANA_ROUTING_KEY);
        assert_eq!(event.source_kind.as_str(), GRAFANA_SOURCE_KIND);
        assert_eq!(event.source_key.as_str(), DEFAULT_SOURCE_KEY);
        assert!(event.context_id.is_some(), "context_id must be present");
        assert_eq!(
            event.message_id.as_deref(),
            Some("grafana:fp1:firing:2026-05-25T12:00:00Z")
        );
    });
}

#[test]
fn repeated_firing_reuses_context_id_across_polls() {
    run_serial_test(async {
        let (_store_guard, store) = install_memory_ingress_store();
        let mapping = MappingStore::open_in_memory().expect("mapping store");
        let producer = GrafanaAlertEventProducer::new().expect("producer");

        let p1 = payload(
            "firing",
            "g1",
            vec![alert("fp1", "firing", "2026-05-25T12:00:00Z")],
        );
        enqueue_webhook(&p1, &mapping, store.as_ref(), DEFAULT_SOURCE_KEY, 1_000)
            .await
            .unwrap();
        let poll1 = producer.poll(&ProducerCheckpoint::none()).await.unwrap();
        let ctx1 = poll1.events[0].context_id.as_ref().unwrap().clone();
        // Mark delivered by polling once more so checkpoint reconciles.
        let _ = producer.poll(&poll1.checkpoint).await.unwrap();

        let p2 = payload(
            "firing",
            "g1",
            vec![alert("fp1", "firing", "2026-05-25T12:05:00Z")],
        );
        enqueue_webhook(&p2, &mapping, store.as_ref(), DEFAULT_SOURCE_KEY, 2_000)
            .await
            .unwrap();
        let poll2 = producer.poll(&ProducerCheckpoint::none()).await.unwrap();
        assert_eq!(poll2.events.len(), 1);
        let ctx2 = poll2.events[0].context_id.as_ref().unwrap().clone();
        assert_eq!(ctx1, ctx2, "repeated firing must reuse the same context_id");
    });
}

#[test]
fn resolved_then_new_firing_mints_fresh_context() {
    run_serial_test(async {
        let (_store_guard, store) = install_memory_ingress_store();
        let mapping = MappingStore::open_in_memory().expect("mapping store");
        let producer = GrafanaAlertEventProducer::new().expect("producer");

        let p1 = payload(
            "firing",
            "g1",
            vec![alert("fp1", "firing", "2026-05-25T12:00:00Z")],
        );
        enqueue_webhook(&p1, &mapping, store.as_ref(), DEFAULT_SOURCE_KEY, 1_000)
            .await
            .unwrap();
        let poll1 = producer.poll(&ProducerCheckpoint::none()).await.unwrap();
        let firing_ctx = poll1.events[0].context_id.as_ref().unwrap().clone();

        let p_resolved = payload(
            "resolved",
            "g1",
            vec![alert("fp1", "resolved", "2026-05-25T12:00:00Z")],
        );
        enqueue_webhook(
            &p_resolved,
            &mapping,
            store.as_ref(),
            DEFAULT_SOURCE_KEY,
            2_000,
        )
        .await
        .unwrap();
        let poll_resolved = producer.poll(&ProducerCheckpoint::none()).await.unwrap();
        assert_eq!(poll_resolved.events.len(), 1);
        let resolved_ctx = poll_resolved.events[0].context_id.as_ref().unwrap().clone();
        assert_eq!(
            firing_ctx, resolved_ctx,
            "resolved must reuse the active context_id"
        );

        let p_new_firing = payload(
            "firing",
            "g1",
            vec![alert("fp1", "firing", "2026-05-25T13:00:00Z")],
        );
        enqueue_webhook(
            &p_new_firing,
            &mapping,
            store.as_ref(),
            DEFAULT_SOURCE_KEY,
            3_000,
        )
        .await
        .unwrap();
        let poll_new = producer.poll(&ProducerCheckpoint::none()).await.unwrap();
        let new_ctx = poll_new.events[0].context_id.as_ref().unwrap().clone();
        assert_ne!(
            firing_ctx, new_ctx,
            "new firing after resolved must mint a fresh context_id"
        );
    });
}

#[test]
fn ingress_enqueue_is_idempotent_per_message_id() {
    run_serial_test(async {
        let (_store_guard, store) = install_memory_ingress_store();
        let mapping = MappingStore::open_in_memory().expect("mapping store");

        let p = payload(
            "firing",
            "g1",
            vec![alert("fp1", "firing", "2026-05-25T12:00:00Z")],
        );
        let first = enqueue_webhook(&p, &mapping, store.as_ref(), DEFAULT_SOURCE_KEY, 1_000)
            .await
            .unwrap();
        let second = enqueue_webhook(&p, &mapping, store.as_ref(), DEFAULT_SOURCE_KEY, 2_000)
            .await
            .unwrap();
        assert_eq!(first.enqueued, 1);
        assert_eq!(second.enqueued, 0);
        assert_eq!(second.duplicates, 1);
    });
}

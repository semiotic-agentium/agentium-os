//! W3C trace context + baggage plumbing for the HTTP surface.
//!
//! Two pieces live here:
//!
//! 1. [`http_request_span`] — factory the router hands to
//!    `TraceLayer::new_for_http().make_span_with(..)` so every inbound
//!    `baml_rt_api.http.request` span adopts an external `traceparent` when
//!    one is present. This is what turns a forwarded A2A request into a
//!    single distributed trace across ingress + serving runners, and lets
//!    any external caller that ships `traceparent` join its own trace.
//! 2. [`extract_parent_trace_context`] — Axum middleware applied on
//!    `/agents/...` only that lifts the `ingress_service_instance_id`
//!    baggage value into a request extension so the handler can flip
//!    `forwarded=true` on spans/metrics.
//!
//! **Advisory-only classification.** `/agents/...` is a public ingress route
//! (see `crates/baml-rt-api/src/router.rs`). Any caller — including an
//! attacker — can set `traceparent` and `baggage` arbitrarily. `forwarded=true`
//! only implies "a value shaped like an ingress id was present in baggage",
//! not "the request came from a trusted peer runner". It has no routing,
//! authz, or agent-behaviour effect; its sole consumer is telemetry labels.
//! A trusted forwarded-signal (baggage validated against the cluster registry,
//! or peer forwarding moved behind the operator token) is a deliberate
//! follow-up under cluster-security hardening and is not in this PR's scope.

use axum::{
    body::Body,
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use baml_rt_observability::INGRESS_SERVICE_INSTANCE_ID_BAGGAGE_KEY;
use opentelemetry::{
    baggage::BaggageExt,
    global,
    trace::{TraceContextExt, TraceId},
};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Peer runner's `service.instance.id` when the inbound request carried the
/// forwarded-request baggage marker.
#[derive(Clone, Debug)]
pub(crate) struct IngressServiceInstanceId(String);

impl IngressServiceInstanceId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Build the `baml_rt_api.http.request` tracing span for an inbound request,
/// adopting the inbound W3C trace context when one is present.
///
/// Called synchronously from `TraceLayer::new_for_http().make_span_with(..)`
/// — returning the span with `set_parent` already called guarantees that
/// every child span created during the request inherits the inbound trace id
/// via natural tracing-opentelemetry span nesting. No handler-side
/// `set_parent` is needed.
pub(crate) fn http_request_span(req: &Request<Body>) -> Span {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("<unmatched>");
    let span = tracing::info_span!(
        "baml_rt_api.http.request",
        http.request.method = %req.method(),
        http.route = %route,
        url.path = %req.uri().path(),
        span.kind = %"server",
    );
    let ctx = global::get_text_map_propagator(|propagator| {
        propagator.extract(&opentelemetry_http::HeaderExtractor(req.headers()))
    });
    if ctx.span().span_context().trace_id() != TraceId::INVALID {
        span.set_parent(ctx);
    }
    span
}

/// Axum middleware applied on `/agents/...` only. Pulls the
/// `ingress_service_instance_id` baggage entry out of the inbound request
/// and inserts it into request extensions as [`IngressServiceInstanceId`],
/// so the handler can derive the advisory `forwarded` bit without
/// re-parsing headers.
pub(crate) async fn extract_parent_trace_context(mut req: Request<Body>, next: Next) -> Response {
    let ctx = global::get_text_map_propagator(|propagator| {
        propagator.extract(&opentelemetry_http::HeaderExtractor(req.headers()))
    });
    if let Some(id) = ctx
        .baggage()
        .get(INGRESS_SERVICE_INSTANCE_ID_BAGGAGE_KEY)
        .map(|v| v.as_str().to_string())
    {
        req.extensions_mut().insert(IngressServiceInstanceId(id));
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use axum::{
        Extension, Router,
        body::Body,
        http::{Request, StatusCode},
        middleware::from_fn,
        routing::post,
    };
    use opentelemetry::{
        global,
        propagation::TextMapCompositePropagator,
        trace::{TraceContextExt, TracerProvider},
    };
    use opentelemetry_sdk::{
        propagation::{BaggagePropagator, TraceContextPropagator},
        trace::TracerProvider as SdkTracerProvider,
    };
    use tower::ServiceExt;
    use tower_http::trace::TraceLayer;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    use super::*;

    fn install_propagator_once() {
        static GATE: OnceLock<()> = OnceLock::new();
        GATE.get_or_init(|| {
            global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
                Box::new(TraceContextPropagator::new()),
                Box::new(BaggagePropagator::new()),
            ]));
        });
    }

    /// The probe handler reads the TraceId of the currently-active span (which
    /// is the outer `baml_rt_api.http.request` span created by the TraceLayer
    /// the router uses in production) and reports it alongside the advisory
    /// `forwarded` bit. This lets tests assert that an inbound `traceparent`
    /// causes every child span — including this handler's — to share the
    /// inbound trace id.
    async fn probe(ingress: Option<Extension<IngressServiceInstanceId>>) -> (StatusCode, String) {
        let forwarded = ingress.is_some();
        let ingress_val = ingress
            .as_ref()
            .map(|e| e.as_str().to_string())
            .unwrap_or_else(|| "<none>".to_string());
        let trace_id = format!(
            "{:032x}",
            Span::current().context().span().span_context().trace_id()
        );
        (
            StatusCode::OK,
            format!("forwarded={forwarded}|ingress={ingress_val}|trace={trace_id}"),
        )
    }

    fn app() -> Router {
        static SUB_GATE: OnceLock<()> = OnceLock::new();
        SUB_GATE.get_or_init(|| {
            let provider = SdkTracerProvider::builder().build();
            let tracer = provider.tracer("probe_test");
            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
            let subscriber = Registry::default().with(otel_layer);
            tracing::subscriber::set_global_default(subscriber).expect(
                "first set_global_default wins; if this panics another test already set one",
            );
        });

        let http_trace_layer = TraceLayer::new_for_http().make_span_with(http_request_span);

        Router::new()
            .route("/probe", post(probe))
            .route_layer(from_fn(extract_parent_trace_context))
            .route_layer(http_trace_layer)
    }

    async fn send(headers: &[(&str, &str)]) -> String {
        let mut builder = Request::builder().method("POST").uri("/probe");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let req = builder.body(Body::empty()).unwrap();
        let resp = app().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn no_headers_means_not_forwarded() {
        install_propagator_once();
        let body = send(&[]).await;
        assert!(body.contains("forwarded=false"), "body={body}");
        assert!(body.contains("ingress=<none>"), "body={body}");
    }

    #[tokio::test]
    async fn traceparent_only_adopts_trace_but_not_forwarded() {
        install_propagator_once();
        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let body = send(&[("traceparent", tp)]).await;
        assert!(body.contains("forwarded=false"), "body={body}");
        assert!(
            body.contains("trace=4bf92f3577b34da6a3ce929d0e0e4736"),
            "span did not adopt inbound trace; body={body}"
        );
    }

    #[tokio::test]
    async fn traceparent_with_ingress_baggage_flips_forwarded() {
        install_propagator_once();
        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let body = send(&[
            ("traceparent", tp),
            ("baggage", "ingress_service_instance_id=peer-runner"),
        ])
        .await;
        assert!(body.contains("forwarded=true"), "body={body}");
        assert!(body.contains("ingress=peer-runner"), "body={body}");
        assert!(
            body.contains("trace=4bf92f3577b34da6a3ce929d0e0e4736"),
            "body={body}"
        );
    }
}

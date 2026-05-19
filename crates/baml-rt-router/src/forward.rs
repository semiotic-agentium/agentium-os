//! Cross-pod A2A request forwarding with SSRF protection and response size cap.

use std::{net::SocketAddr, time::Instant};

use baml_rt_core::{
    A2aSseDecoder, A2aStreamChunk, AgentInstanceId, AgentPackageName, BamlRtError,
    bus::{BusStream, bus_stream_channel},
    parse_a2a_sse_json_rpc_chunks,
};
use baml_rt_observability::{INGRESS_SERVICE_INSTANCE_ID_BAGGAGE_KEY, metrics, spans};
use opentelemetry::{KeyValue, baggage::BaggageExt};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::ssrf;

/// Maximum response body size (50 MiB) to prevent memory exhaustion from
/// oversized or malicious responses.
const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// Validated and DNS-pinned forwarding target.
#[derive(Debug, Clone)]
pub struct ForwardTarget {
    /// Full URL for the A2A endpoint (e.g. `http://runner-0:18080/agents/pkg/inst/a2a`).
    /// IPv6 literals are correctly bracketed (`http://[fd12::1]:18080/...`)
    /// because this is rendered from [`url::Url`] rather than manual formatting.
    pub url: String,
    /// Unbracketed host portion of the validated URL — suitable for
    /// [`reqwest::ClientBuilder::resolve_to_addrs`]. For IPv6 literals this
    /// is the bare address (`fd12::1`), not the bracketed URL form
    /// (`[fd12::1]`), because hyper's DNS override map is keyed on the bare
    /// form.
    pub host: String,
    /// Resolved socket addresses to pin the HTTP client to, closing the
    /// DNS-rebinding TOCTOU gap.
    pub resolved_addrs: Vec<SocketAddr>,
}

/// Validate and resolve a cluster endpoint, then build the full A2A forward URL.
///
/// Returns a [`ForwardTarget`] containing the pinned addresses from DNS resolution
/// so the HTTP client connects to the validated IP (not a re-resolved one).
pub async fn resolve_forward_target(
    endpoint: &str,
    agent_package: &str,
    agent_instance_id: &str,
) -> Result<ForwardTarget, BamlRtError> {
    let (validated, resolved_addrs) = ssrf::resolve_and_validate_cluster_endpoint(endpoint)
        .await
        .map_err(|e| BamlRtError::InvalidArgument(format!("cluster endpoint rejected: {e}")))?;

    // Build the forward URL from the validated `url::Url` rather than via
    // string formatting. This strips attacker-controlled path/query/fragment
    // and automatically brackets IPv6 literals on render.
    let forward_url = build_a2a_forward_url(&validated, agent_package, agent_instance_id)?;
    // `url::Url::host_str` includes the square brackets for IPv6 literals;
    // hyper's DNS override map is keyed on the bare address, so match on
    // `Host` to get the unbracketed form.
    let host = match validated.host() {
        Some(url::Host::Domain(d)) => d.to_string(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => String::new(),
    };

    Ok(ForwardTarget {
        url: forward_url.to_string(),
        host,
        resolved_addrs,
    })
}

/// Construct the forwarded A2A URL from a validated cluster endpoint URL.
///
/// - Strips userinfo, path, query, and fragment from the origin.
/// - Appends `/agents/{agent_package}/{agent_instance_id}/a2a` via
///   [`url::Url::path_segments_mut`] so segments are percent-encoded.
/// - IPv6 literals are bracketed automatically by the [`url::Url`] renderer.
fn build_a2a_forward_url(
    validated: &url::Url,
    agent_package: &str,
    agent_instance_id: &str,
) -> Result<url::Url, BamlRtError> {
    let mut url = validated.clone();
    url.set_username("").map_err(|()| {
        BamlRtError::InvalidArgument("cannot clear username on endpoint URL".to_string())
    })?;
    url.set_password(None).map_err(|()| {
        BamlRtError::InvalidArgument("cannot clear password on endpoint URL".to_string())
    })?;
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .map_err(|()| {
            BamlRtError::InvalidArgument("endpoint URL cannot have path segments".to_string())
        })?
        .clear()
        .push("agents")
        .push(agent_package)
        .push(agent_instance_id)
        .push("a2a");
    Ok(url)
}

/// Forward a JSON body to a remote runner via HTTP POST, reading the response
/// with a byte-count cap to prevent memory exhaustion.
///
/// The caller is responsible for building the `ForwardTarget` via
/// [`resolve_forward_target`] so the DNS-pinned addresses are used.
///
/// Typed `agent_package` / `agent_instance_id` refs keep raw public path input
/// out of the identity-bearing cluster-forward span and metric; the caller
/// must parse before invoking this function.
///
/// `ingress_service_instance_id` is the local runner's `service.instance.id`.
/// It is injected into outbound `baggage` so the serving runner can surface it
/// on forwarded telemetry. The resulting classification is advisory because
/// `/agents/...` is a public route — any caller can spoof the marker.
pub async fn forward_request(
    target: &ForwardTarget,
    body: &serde_json::Value,
    agent_package: &AgentPackageName,
    agent_instance_id: &AgentInstanceId,
    ingress_service_instance_id: &str,
    target_service_instance_id: Option<&str>,
) -> Result<Vec<serde_json::Value>, BamlRtError> {
    let span = spans::cluster_a2a_forward(
        agent_package,
        agent_instance_id,
        &target.url,
        ingress_service_instance_id,
        target_service_instance_id,
    );

    async move {
        let start = Instant::now();
        let out = forward_request_inner(target, body, ingress_service_instance_id).await;
        let label = match &out {
            Ok(_) => "success",
            Err(e) => cluster_forward_error_label(e),
        };
        metrics::record_cluster_a2a_forward(
            agent_package,
            agent_instance_id,
            label,
            ingress_service_instance_id,
            target_service_instance_id,
            start.elapsed(),
        );
        out
    }
    .instrument(span)
    .await
}

/// Forward a JSON body to a remote runner and surface each SSE `data:` JSON-RPC
/// object as it arrives (no full-body buffer before the first chunk).
///
/// Uses the same DNS pinning, trace/baggage injection, and byte cap as
/// [`forward_request`]. Metrics are recorded when the HTTP body read finishes
/// (success or transport/parse/cap failure).
pub async fn forward_stream_request(
    target: &ForwardTarget,
    body: &serde_json::Value,
    agent_package: &AgentPackageName,
    agent_instance_id: &AgentInstanceId,
    ingress_service_instance_id: &str,
    target_service_instance_id: Option<&str>,
) -> Result<BusStream<A2aStreamChunk>, BamlRtError> {
    let span = spans::cluster_a2a_forward(
        agent_package,
        agent_instance_id,
        &target.url,
        ingress_service_instance_id,
        target_service_instance_id,
    );
    let span_reader = span.clone();

    let target = target.clone();
    let body = body.clone();
    let agent_package = agent_package.clone();
    let agent_instance_id = agent_instance_id.clone();
    let ingress_service_instance_id = ingress_service_instance_id.to_string();
    let target_service_instance_id = target_service_instance_id.map(str::to_string);

    async move {
        let start = Instant::now();
        let record_early = |label: &'static str| {
            metrics::record_cluster_a2a_forward(
                &agent_package,
                &agent_instance_id,
                label,
                ingress_service_instance_id.as_str(),
                target_service_instance_id.as_deref(),
                start.elapsed(),
            );
        };

        let mut builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none());

        builder = builder.resolve_to_addrs(&target.host, &target.resolved_addrs);
        tracing::debug!(
            url = %target.url,
            host = %target.host,
            resolved_addrs = ?target.resolved_addrs,
            "forwarding cluster A2A request (streaming) with DNS-pinned addresses"
        );

        let client = builder.build().map_err(|e| {
            let err = BamlRtError::Io(std::io::Error::other(format!(
                "HTTP client build failed: {e}"
            )));
            record_early(cluster_forward_error_label(&err));
            err
        })?;

        let mut request = client.post(&target.url).json(&body).build().map_err(|e| {
            let err = BamlRtError::Io(std::io::Error::other(format!(
                "cluster A2A request build: {e}"
            )));
            record_early(cluster_forward_error_label(&err));
            err
        })?;

        let ctx = tracing::Span::current()
            .context()
            .with_baggage(vec![KeyValue::new(
                INGRESS_SERVICE_INSTANCE_ID_BAGGAGE_KEY,
                ingress_service_instance_id.clone(),
            )]);
        opentelemetry::global::get_text_map_propagator(|propagator| {
            let mut injector = opentelemetry_http::HeaderInjector(request.headers_mut());
            propagator.inject_context(&ctx, &mut injector);
        });

        let resp = client.execute(request).await.map_err(|e| {
            let err = BamlRtError::Io(std::io::Error::other(format!(
                "cluster A2A forward failed: {e}"
            )));
            record_early(cluster_forward_error_label(&err));
            err
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = read_body_lossy(resp, 512).await;
            let text = ssrf::truncate_body(&text, 512);
            let err = BamlRtError::Io(std::io::Error::other(format!(
                "cluster A2A forward returned {status}: {text}"
            )));
            record_early("http_error");
            return Err(err);
        }

        let (tx, stream) = bus_stream_channel::<A2aStreamChunk>(32);
        let ingress_for_task = ingress_service_instance_id.clone();
        let target_si_for_task = target_service_instance_id.clone();

        tokio::spawn(
            {
                let agent_package = agent_package.clone();
                let agent_instance_id = agent_instance_id.clone();
                async move {
                    let task_start = Instant::now();
                    let mut decoder = A2aSseDecoder::new();
                    let mut total: usize = 0;
                    let mut resp = resp;
                    let label = 'read: loop {
                        match resp.chunk().await {
                            Ok(Some(chunk)) => {
                                total = total.saturating_add(chunk.len());
                                if total > MAX_RESPONSE_BYTES {
                                    tracing::warn!(
                                        cap = MAX_RESPONSE_BYTES,
                                        "cluster A2A streaming response exceeded byte cap"
                                    );
                                    break 'read "invalid_argument";
                                }
                                match decoder.feed(chunk.as_ref()) {
                                    Ok(events) => {
                                        for v in events {
                                            if tx.send(A2aStreamChunk(v)).await.is_err() {
                                                break 'read "success";
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            "cluster A2A SSE incremental parse failed"
                                        );
                                        break 'read "parse_error";
                                    }
                                }
                            }
                            Ok(None) => {
                                match decoder.finish() {
                                    Ok(events) => {
                                        for v in events {
                                            if tx.send(A2aStreamChunk(v)).await.is_err() {
                                                break 'read "success";
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            "cluster A2A SSE finish parse failed"
                                        );
                                        break 'read "parse_error";
                                    }
                                }
                                break 'read "success";
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "cluster A2A streaming body read failed"
                                );
                                break 'read "transport_error";
                            }
                        }
                    };
                    metrics::record_cluster_a2a_forward(
                        &agent_package,
                        &agent_instance_id,
                        label,
                        ingress_for_task.as_str(),
                        target_si_for_task.as_deref(),
                        task_start.elapsed(),
                    );
                }
            }
            .instrument(span_reader),
        );

        Ok(stream)
    }
    .instrument(span)
    .await
}

fn cluster_forward_error_label(e: &BamlRtError) -> &'static str {
    match e {
        BamlRtError::InvalidArgument(_) => "invalid_argument",
        BamlRtError::Io(io) => {
            let m = io.to_string();
            if m.contains("forward returned") {
                "http_error"
            } else if m.contains("parse") || m.contains("JSON") {
                "parse_error"
            } else {
                "transport_error"
            }
        }
        _ => "error",
    }
}

async fn forward_request_inner(
    target: &ForwardTarget,
    body: &serde_json::Value,
    ingress_service_instance_id: &str,
) -> Result<Vec<serde_json::Value>, BamlRtError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none());

    // Pin all resolved addresses so the HTTP client connects to the validated IPs.
    builder = builder.resolve_to_addrs(&target.host, &target.resolved_addrs);
    tracing::debug!(
        url = %target.url,
        host = %target.host,
        resolved_addrs = ?target.resolved_addrs,
        "forwarding cluster A2A request with DNS-pinned addresses"
    );

    let client = builder.build().map_err(|e| {
        BamlRtError::Io(std::io::Error::other(format!(
            "HTTP client build failed: {e}"
        )))
    })?;

    // Build the request ahead of `.send()` so we can mutate headers to inject
    // W3C trace context + the `ingress_service_instance_id` baggage marker.
    let mut request = client.post(&target.url).json(body).build().map_err(|e| {
        BamlRtError::Io(std::io::Error::other(format!(
            "cluster A2A request build: {e}"
        )))
    })?;

    let ctx = tracing::Span::current()
        .context()
        .with_baggage(vec![KeyValue::new(
            INGRESS_SERVICE_INSTANCE_ID_BAGGAGE_KEY,
            ingress_service_instance_id.to_string(),
        )]);
    opentelemetry::global::get_text_map_propagator(|propagator| {
        let mut injector = opentelemetry_http::HeaderInjector(request.headers_mut());
        propagator.inject_context(&ctx, &mut injector);
    });

    let resp = client.execute(request).await.map_err(|e| {
        BamlRtError::Io(std::io::Error::other(format!(
            "cluster A2A forward failed: {e}"
        )))
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = read_body_lossy(resp, 512).await;
        let text = ssrf::truncate_body(&text, 512);
        return Err(BamlRtError::Io(std::io::Error::other(format!(
            "cluster A2A forward returned {status}: {text}"
        ))));
    }

    // On the success path a truncated transport MUST surface as an error — the
    // previous `while let Ok(Some(_))` loop silently dropped read errors and
    // handed back a partial buffer, which could parse as syntactically valid
    // JSON even though the upstream dropped mid-stream.
    let body_bytes = read_body_capped(resp, MAX_RESPONSE_BYTES).await?;
    let text = String::from_utf8(body_bytes).map_err(|e| {
        BamlRtError::Io(std::io::Error::other(format!(
            "cluster A2A response body is not UTF-8: {e}"
        )))
    })?;
    parse_a2a_sse_json_rpc_chunks(&text)
        .map_err(|e| BamlRtError::Io(std::io::Error::other(format!("cluster A2A SSE parse: {e}"))))
}

/// Read a response body into bytes with a byte-count cap, propagating read
/// failures and cap breaches as errors.
///
/// This is used on the success path. Read failures must not be confused with
/// EOF: a connection drop after partial bytes can leave behind a syntactically
/// complete but transport-truncated buffer, which we must never accept.
async fn read_body_capped(
    mut resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, BamlRtError> {
    let mut total: usize = 0;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                total = total.saturating_add(chunk.len());
                if total > max_bytes {
                    return Err(BamlRtError::Io(std::io::Error::other(format!(
                        "cluster A2A response body exceeded {max_bytes}-byte cap"
                    ))));
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(buf),
            Err(e) => {
                return Err(BamlRtError::Io(std::io::Error::other(format!(
                    "cluster A2A response read failed: {e}"
                ))));
            }
        }
    }
}

/// Best-effort body read for diagnostic use: silently stops on cap breach,
/// EOF, or transport failure so a partial error body can still surface in the
/// final error message. The success path must NOT use this helper — use
/// [`read_body_capped`] instead, which distinguishes EOF from read errors.
async fn read_body_lossy(mut resp: reqwest::Response, max_bytes: usize) -> String {
    let mut total: usize = 0;
    let mut buf: Vec<u8> = Vec::new();
    // Treating `Err` and `Ok(None)` the same is deliberate here — this reader
    // is for diagnostic bodies where a truncated error body is still better
    // than no error body at all.
    while let Ok(Some(chunk)) = resp.chunk().await {
        total = total.saturating_add(chunk.len());
        if total > max_bytes {
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, sync::OnceLock};

    use opentelemetry::{global, propagation::TextMapCompositePropagator, trace::TracerProvider};
    use opentelemetry_sdk::{
        propagation::{BaggagePropagator, TraceContextPropagator},
        trace::TracerProvider as SdkTracerProvider,
    };
    use test_support::common::bind_ephemeral_tokio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    use super::*;

    fn test_pkg() -> AgentPackageName {
        AgentPackageName::parse("pkg").expect("valid package name")
    }

    fn test_inst() -> AgentInstanceId {
        AgentInstanceId::parse("inst").expect("valid instance id")
    }

    fn install_propagator_once() {
        static GATE: OnceLock<()> = OnceLock::new();
        GATE.get_or_init(|| {
            global::set_text_map_propagator(TextMapCompositePropagator::new(vec![
                Box::new(TraceContextPropagator::new()),
                Box::new(BaggagePropagator::new()),
            ]));
        });
    }

    #[tokio::test]
    async fn resolve_forward_target_rejects_ssrf() {
        let result = resolve_forward_target("http://169.254.169.254", "pkg", "default").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("link-local") || err.contains("metadata"),
            "should mention link-local or metadata: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_forward_target_builds_url() {
        let result = resolve_forward_target("http://10.0.0.1:18080", "my-agent", "default").await;
        assert!(result.is_ok());
        let target = result.unwrap();
        assert_eq!(
            target.url,
            "http://10.0.0.1:18080/agents/my-agent/default/a2a"
        );
        assert!(!target.resolved_addrs.is_empty());
    }

    /// Regression test for the IPv6 forwarding regression: `origin_url` stripped
    /// the square brackets from IPv6 literals, producing URLs like
    /// `http://fd12::1:18080/...` that are not parseable.
    #[tokio::test]
    async fn resolve_forward_target_brackets_ipv6_literal() {
        let result = resolve_forward_target("http://[fd12::1]:18080", "my-agent", "default").await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let target = result.unwrap();

        assert_eq!(
            target.url, "http://[fd12::1]:18080/agents/my-agent/default/a2a",
            "IPv6 host must stay bracketed in the forwarded URL"
        );

        // The serialized URL must round-trip through `url::Url::parse` so
        // reqwest can build a request from it. `url::Url::host_str` keeps the
        // bracketed form for IPv6 literals — that's the URL-level
        // representation, which is what we want in the forward URL string.
        let reparsed = url::Url::parse(&target.url).expect("forward URL must be a valid URL");
        assert_eq!(reparsed.host_str(), Some("[fd12::1]"));
        assert_eq!(reparsed.port(), Some(18080));
        assert_eq!(
            reparsed.path(),
            "/agents/my-agent/default/a2a",
            "forwarded path must be preserved"
        );

        // `target.host` is the bare address — this is what
        // `reqwest::ClientBuilder::resolve_to_addrs` matches against, since
        // hyper strips the brackets before looking up overrides.
        assert_eq!(target.host, "fd12::1");
        assert_eq!(
            target.resolved_addrs[0].ip(),
            "fd12::1".parse::<IpAddr>().unwrap()
        );
    }

    /// Attacker-controlled path/query/fragment on the inbound endpoint must be
    /// dropped — only the canonical `/agents/.../a2a` path must remain.
    #[tokio::test]
    async fn resolve_forward_target_strips_attacker_controlled_components() {
        let target =
            resolve_forward_target("http://10.0.0.1:18080/../../admin?x=1#frag", "pkg", "inst")
                .await
                .expect("endpoint should validate");

        assert_eq!(target.url, "http://10.0.0.1:18080/agents/pkg/inst/a2a");
    }

    /// Regression: `build_a2a_forward_url` cloned the validated `url::Url`
    /// without stripping userinfo, so `http://user:pass@host` leaked
    /// credentials into the forwarded URL (and into debug logs).
    #[tokio::test]
    async fn resolve_forward_target_strips_ipv4_userinfo() {
        let target =
            resolve_forward_target("http://user:pass@10.0.0.1:18080", "my-agent", "default")
                .await
                .expect("endpoint with userinfo should validate after stripping");

        assert_eq!(
            target.url, "http://10.0.0.1:18080/agents/my-agent/default/a2a",
            "forwarded URL must not contain userinfo"
        );
        assert!(
            !target.url.contains('@'),
            "forwarded URL must not contain '@'"
        );
        assert!(
            !target.url.contains("user"),
            "forwarded URL must not contain username"
        );
        assert!(
            !target.url.contains("pass"),
            "forwarded URL must not contain password"
        );
        assert_eq!(target.host, "10.0.0.1");
    }

    /// Same regression as above, but with an IPv6 literal — ensures bracket
    /// handling and userinfo stripping compose correctly.
    #[tokio::test]
    async fn resolve_forward_target_strips_ipv6_userinfo() {
        let target =
            resolve_forward_target("http://user:pass@[fd12::1]:18080", "my-agent", "default")
                .await
                .expect("IPv6 endpoint with userinfo should validate after stripping");

        assert_eq!(
            target.url, "http://[fd12::1]:18080/agents/my-agent/default/a2a",
            "forwarded URL must strip userinfo and preserve IPv6 brackets"
        );
        assert!(
            !target.url.contains('@'),
            "forwarded URL must not contain '@'"
        );
        assert_eq!(target.host, "fd12::1");
    }

    /// Regression test for the body-read swallowing bug: when the upstream
    /// advertises a `Content-Length` larger than what it actually sends and
    /// then closes the socket, the old `while let Ok(Some(_))` loop dropped
    /// the read error and returned a partial body — so a syntactically valid
    /// `[]` prefix was parsed as success.
    ///
    /// A raw TCP server is used because axum/hyper will honour the declared
    /// `Content-Length` and won't produce this malformed shape.
    #[tokio::test]
    async fn forward_request_surfaces_truncated_response_as_error() {
        let (listener, addr) = bind_ephemeral_tokio("127.0.0.1").await.unwrap();
        let port = addr.port();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            // Drain the HTTP request headers so the client's write-side
            // completes and we can respond.
            let mut buf = [0u8; 4096];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Announce a 100-byte body, then send only `[]` (a syntactically
            // complete JSON array of zero items) and close. hyper should see
            // the short close and emit an error on the next chunk read.
            let response = b"HTTP/1.1 200 OK\r\n\
                             Content-Type: application/json\r\n\
                             Content-Length: 100\r\n\
                             Connection: close\r\n\
                             \r\n\
                             []";
            let _ = socket.write_all(response).await;
            let _ = socket.shutdown().await;
        });

        let target = ForwardTarget {
            url: format!("http://127.0.0.1:{port}/agents/pkg/inst/a2a"),
            host: "127.0.0.1".to_string(),
            resolved_addrs: vec![SocketAddr::from(([127, 0, 0, 1], port))],
        };

        let result = forward_request(
            &target,
            &serde_json::json!({}),
            &test_pkg(),
            &test_inst(),
            "runner-0",
            None,
        )
        .await;
        let _ = server.await;

        assert!(
            result.is_err(),
            "expected Err on truncated body, got Ok({result:?}) — the old read_body_capped would accept `[]` as success"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("read failed") || err.contains("forward failed"),
            "expected read-failure surface, got: {err}"
        );
    }

    /// A well-formed body whose size exactly matches `Content-Length` must
    /// parse successfully — the new error handling must not regress the
    /// happy path.
    #[tokio::test]
    async fn forward_request_parses_complete_response() {
        let (listener, addr) = bind_ephemeral_tokio("127.0.0.1").await.unwrap();
        let port = addr.port();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let body = b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
                len = body.len()
            );
            let _ = socket.write_all(headers.as_bytes()).await;
            let _ = socket.write_all(body).await;
            let _ = socket.shutdown().await;
        });

        let target = ForwardTarget {
            url: format!("http://127.0.0.1:{port}/agents/pkg/inst/a2a"),
            host: "127.0.0.1".to_string(),
            resolved_addrs: vec![SocketAddr::from(([127, 0, 0, 1], port))],
        };

        let items = forward_request(
            &target,
            &serde_json::json!({}),
            &test_pkg(),
            &test_inst(),
            "runner-0",
            Some("runner-1"),
        )
        .await
        .expect("complete SSE body should parse");
        let _ = server.await;

        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], 1);
    }

    /// Forwarded requests must carry W3C trace context and the
    /// `ingress_service_instance_id` baggage marker so the serving runner can
    /// (a) join the trace via `OpenTelemetrySpanExt::set_parent` and
    /// (b) flip `forwarded=true` on its `post_a2a` span.
    #[tokio::test]
    async fn forward_request_injects_traceparent_and_ingress_baggage() {
        install_propagator_once();

        let (listener, addr) = bind_ephemeral_tokio("127.0.0.1").await.unwrap();
        let port = addr.port();

        let (header_tx, header_rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = header_tx.send(acc);

            let body = b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
                len = body.len()
            );
            let _ = socket.write_all(headers.as_bytes()).await;
            let _ = socket.write_all(body).await;
            let _ = socket.shutdown().await;
        });

        let target = ForwardTarget {
            url: format!("http://127.0.0.1:{port}/agents/pkg/inst/a2a"),
            host: "127.0.0.1".to_string(),
            resolved_addrs: vec![SocketAddr::from(([127, 0, 0, 1], port))],
        };

        // Install a per-test tracing-opentelemetry subscriber so
        // `Span::current().context()` returns a non-empty OTEL context with a
        // real TraceId. Global subscriber state is untouched — `set_default`
        // only affects this thread's dispatch.
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("forward_test");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        let subscriber = Registry::default().with(otel_layer);
        let _guard = tracing::subscriber::set_default(subscriber);

        let outer = tracing::info_span!("test_outer");
        let result = async {
            forward_request(
                &target,
                &serde_json::json!({}),
                &test_pkg(),
                &test_inst(),
                "runner-ingress",
                Some("runner-serving"),
            )
            .await
        }
        .instrument(outer)
        .await;
        result.expect("happy path should still succeed");

        let raw = header_rx.await.expect("server captured headers");
        let _ = server.await;
        let hdrs = String::from_utf8_lossy(&raw).to_string();

        assert!(
            hdrs.to_lowercase().contains("\ntraceparent:")
                || hdrs.to_lowercase().starts_with("traceparent:"),
            "expected traceparent injected; saw:\n{hdrs}"
        );
        assert!(
            hdrs.to_lowercase().contains("\nbaggage:")
                || hdrs.to_lowercase().starts_with("baggage:"),
            "expected baggage injected; saw:\n{hdrs}"
        );
        assert!(
            hdrs.contains("ingress_service_instance_id=runner-ingress"),
            "baggage must carry ingress pod name; saw:\n{hdrs}"
        );
    }
}

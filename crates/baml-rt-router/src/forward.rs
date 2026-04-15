//! Cross-pod A2A request forwarding with SSRF protection and response size cap.

use std::net::SocketAddr;

use baml_rt_core::BamlRtError;

use crate::ssrf;

/// Maximum response body size (50 MiB) to prevent memory exhaustion from
/// oversized or malicious responses.
const MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// Validated and DNS-pinned forwarding target.
#[derive(Debug)]
pub struct ForwardTarget {
    /// Full URL for the A2A endpoint (e.g. `http://runner-0:18080/agents/pkg/inst/a2a`).
    pub url: String,
    /// The host portion of the validated URL (for reqwest DNS pinning).
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

    let base = ssrf::origin_url(&validated);
    let host = validated.host_str().unwrap_or("").to_string();
    let url = format!("{base}/agents/{agent_package}/{agent_instance_id}/a2a");

    Ok(ForwardTarget {
        url,
        host,
        resolved_addrs,
    })
}

/// Forward a JSON body to a remote runner via HTTP POST, reading the response
/// with a byte-count cap to prevent memory exhaustion.
///
/// The caller is responsible for building the `ForwardTarget` via
/// [`resolve_forward_target`] so the DNS-pinned addresses are used.
pub async fn forward_request(
    target: &ForwardTarget,
    body: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, BamlRtError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none());

    // Pin all resolved addresses so the HTTP client connects to the validated IPs.
    builder = builder.resolve_to_addrs(&target.host, &target.resolved_addrs);

    let client = builder.build().map_err(|e| {
        BamlRtError::Io(std::io::Error::other(format!(
            "HTTP client build failed: {e}"
        )))
    })?;

    let resp = client
        .post(&target.url)
        .json(body)
        .send()
        .await
        .map_err(|e| {
            BamlRtError::Io(std::io::Error::other(format!(
                "cluster A2A forward failed: {e}"
            )))
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = read_body_capped(resp, 512).await;
        let text = ssrf::truncate_body(&text, 512);
        return Err(BamlRtError::Io(std::io::Error::other(format!(
            "cluster A2A forward returned {status}: {text}"
        ))));
    }

    let body_bytes = read_body_capped(resp, MAX_RESPONSE_BYTES).await;
    let items: Vec<serde_json::Value> = serde_json::from_str(&body_bytes).map_err(|e| {
        BamlRtError::Io(std::io::Error::other(format!(
            "cluster A2A response parse: {e}"
        )))
    })?;

    Ok(items)
}

/// Read response body by streaming chunks with a byte counter, bailing if the
/// total exceeds `max_bytes`.
async fn read_body_capped(resp: reqwest::Response, max_bytes: usize) -> String {
    let mut total = 0usize;
    let mut parts = Vec::new();

    // Use chunk() to stream the response incrementally.
    let mut resp = resp;
    while let Ok(Some(chunk)) = resp.chunk().await {
        total = total.saturating_add(chunk.len());
        if total > max_bytes {
            tracing::warn!(
                total_bytes = total,
                max_bytes = max_bytes,
                "response body exceeded size cap, truncating"
            );
            break;
        }
        parts.push(chunk);
    }

    let combined: Vec<u8> = parts.into_iter().flat_map(|c| c.to_vec()).collect();
    String::from_utf8_lossy(&combined).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

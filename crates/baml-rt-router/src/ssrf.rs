//! Cluster endpoint validation: blocks SSRF-dangerous targets.

use std::net::{IpAddr, SocketAddr};

/// Validate that a raw URL string is safe for cluster-internal forwarding.
///
/// Rejects:
/// - Non-HTTP(S) schemes
/// - Loopback addresses (127.0.0.0/8, ::1)
/// - Unspecified addresses (0.0.0.0, ::)
/// - Link-local / metadata addresses (169.254.0.0/16, fe80::/10)
/// - Known cloud metadata hostnames
/// - Cloud metadata IPs (169.254.169.254, 100.100.100.200, fd00:ec2::/32)
pub fn validate_cluster_endpoint(raw: &str) -> Result<url::Url, String> {
    // Reject non-ASCII input before URL parsing to prevent IDNA normalization
    // bypasses (e.g. Unicode look-alikes that normalize to blocked hostnames).
    if !raw.is_ascii() {
        return Err("endpoint URL contains non-ASCII characters".to_string());
    }

    let parsed = url::Url::parse(raw).map_err(|e| format!("invalid endpoint URL '{raw}': {e}"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "endpoint scheme must be http or https, got '{other}'"
            ));
        }
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| format!("endpoint '{raw}' has no host"))?;

    // Block known cloud metadata hostnames (exact match and subdomain suffix).
    let blocked_hostnames = [
        "localhost",
        "metadata.google.internal",
        "metadata.aws.internal",
        "metadata.azure.internal",
    ];
    let lower = host.to_ascii_lowercase();
    for blocked in &blocked_hostnames {
        if lower == *blocked || lower.ends_with(&format!(".{blocked}")) {
            return Err(format!("endpoint host '{host}' is blocked"));
        }
    }

    // Check IP address classes (url::Url gives us the parsed Host directly).
    if let Some(url::Host::Ipv4(v4)) = parsed.host() {
        let ip = IpAddr::V4(v4);
        reject_dangerous_ip(ip, host)?;
    }
    if let Some(url::Host::Ipv6(v6)) = parsed.host() {
        let ip = IpAddr::V6(v6);
        reject_dangerous_ip(ip, host)?;
    }

    Ok(parsed)
}

/// Async DNS-resolving variant: validates the URL (scheme, blocked hostnames,
/// literal-IP ranges) and then resolves DNS names to check every resolved IP
/// against the blocklist.
///
/// Returns the validated URL and the resolved socket addresses so callers can
/// pin the HTTP client to the validated IPs (closing the DNS-rebinding TOCTOU gap).
pub async fn resolve_and_validate_cluster_endpoint(
    raw: &str,
) -> Result<(url::Url, Vec<SocketAddr>), String> {
    let parsed = validate_cluster_endpoint(raw)?;

    let host = parsed.host_str().unwrap_or("");
    let port = parsed.port_or_known_default().unwrap_or(80);

    // Literal IPs are already checked by validate_cluster_endpoint.
    // DNS names need resolution + IP check.
    match parsed.host() {
        Some(url::Host::Ipv4(v4)) => {
            let addr = SocketAddr::new(IpAddr::V4(v4), port);
            Ok((parsed, vec![addr]))
        }
        Some(url::Host::Ipv6(v6)) => {
            let addr = SocketAddr::new(IpAddr::V6(v6), port);
            Ok((parsed, vec![addr]))
        }
        _ => {
            let lookup = format!("{host}:{port}");
            let resolved = tokio::net::lookup_host(&lookup)
                .await
                .map_err(|e| format!("DNS resolution failed for '{host}': {e}"))?;
            let ips: Vec<SocketAddr> = resolved.collect();
            if ips.is_empty() {
                return Err(format!("DNS resolution returned no addresses for '{host}'"));
            }
            for sock_addr in &ips {
                reject_dangerous_ip(sock_addr.ip(), host)?;
            }
            Ok((parsed, ips))
        }
    }
}

fn reject_dangerous_ip(ip: IpAddr, host: &str) -> Result<(), String> {
    if ip.is_loopback() {
        return Err(format!("endpoint host '{host}' is a loopback address"));
    }
    if ip.is_unspecified() {
        return Err(format!("endpoint host '{host}' is an unspecified address"));
    }
    if is_link_local_or_metadata(ip) {
        return Err(format!(
            "endpoint host '{host}' is a link-local or metadata address"
        ));
    }
    if is_cloud_metadata_ip(ip) {
        return Err(format!(
            "endpoint host '{host}' is a cloud metadata address"
        ));
    }
    Ok(())
}

fn is_link_local_or_metadata(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 169.254.0.0/16 — link-local / cloud metadata
            octets[0] == 169 && octets[1] == 254
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            // fe80::/10 — IPv6 link-local
            segments[0] & 0xffc0 == 0xfe80
        }
    }
}

fn is_cloud_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // Alibaba Cloud metadata: 100.100.100.200
            octets == [100, 100, 100, 200]
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            // AWS IPv6 IMDS: fd00:ec2::/32 prefix (covers fd00:ec2::254 and the full range)
            segments[0] == 0xfd00 && segments[1] == 0x0ec2
        }
    }
}

/// Extract the origin (scheme + host + port) from a validated URL, discarding
/// any attacker-controlled path components.
pub fn origin_url(url: &url::Url) -> String {
    let port = url.port_or_known_default().unwrap_or(80);
    format!(
        "{scheme}://{host}:{port}",
        scheme = url.scheme(),
        host = url.host_str().unwrap_or(""),
    )
}

/// Truncate a string to at most `max_bytes` bytes (on a char boundary).
pub fn truncate_body(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        text.to_string()
    } else {
        let truncated = &text[..text.floor_char_boundary(max_bytes)];
        format!("{truncated}...[truncated]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_cluster_endpoint() {
        let url = validate_cluster_endpoint("http://runner-0.runner.agentium.svc:18080");
        assert!(url.is_ok(), "expected Ok, got {url:?}");
    }

    #[test]
    fn valid_https_endpoint() {
        let url = validate_cluster_endpoint("https://runner-1.example.com:443");
        assert!(url.is_ok());
    }

    #[test]
    fn rejects_non_http_scheme() {
        let err = validate_cluster_endpoint("ftp://runner-0:18080").unwrap_err();
        assert!(err.contains("scheme"), "error should mention scheme: {err}");
    }

    #[test]
    fn rejects_loopback_ipv4() {
        let err = validate_cluster_endpoint("http://127.0.0.1:18080").unwrap_err();
        assert!(
            err.contains("loopback"),
            "error should mention loopback: {err}"
        );
    }

    #[test]
    fn rejects_loopback_ipv6() {
        let err = validate_cluster_endpoint("http://[::1]:18080").unwrap_err();
        assert!(
            err.contains("loopback"),
            "error should mention loopback: {err}"
        );
    }

    #[test]
    fn rejects_unspecified() {
        let err = validate_cluster_endpoint("http://0.0.0.0:18080").unwrap_err();
        assert!(
            err.contains("unspecified"),
            "error should mention unspecified: {err}"
        );
    }

    #[test]
    fn rejects_link_local_metadata() {
        let err = validate_cluster_endpoint("http://169.254.169.254/latest/meta-data").unwrap_err();
        assert!(
            err.contains("link-local") || err.contains("metadata"),
            "error should mention link-local or metadata: {err}"
        );
    }

    #[test]
    fn rejects_localhost() {
        let err = validate_cluster_endpoint("http://localhost:18080").unwrap_err();
        assert!(
            err.contains("blocked"),
            "error should mention blocked: {err}"
        );
    }

    #[test]
    fn rejects_cloud_metadata_hostname() {
        let err =
            validate_cluster_endpoint("http://metadata.google.internal/v1/instance").unwrap_err();
        assert!(
            err.contains("blocked"),
            "error should mention blocked: {err}"
        );
    }

    #[test]
    fn rejects_azure_metadata_hostname() {
        let err = validate_cluster_endpoint("http://metadata.azure.internal/metadata").unwrap_err();
        assert!(
            err.contains("blocked"),
            "error should mention blocked: {err}"
        );
    }

    #[test]
    fn rejects_metadata_subdomain() {
        let err = validate_cluster_endpoint("http://evil.metadata.google.internal/v1/instance")
            .unwrap_err();
        assert!(
            err.contains("blocked"),
            "subdomain of blocked host should be blocked: {err}"
        );
    }

    #[test]
    fn rejects_aws_ipv6_imds_exact() {
        let err = validate_cluster_endpoint("http://[fd00:ec2::254]/latest/meta-data").unwrap_err();
        assert!(
            err.contains("cloud metadata"),
            "error should mention cloud metadata: {err}"
        );
    }

    #[test]
    fn rejects_aws_ipv6_imds_prefix() {
        let err = validate_cluster_endpoint("http://[fd00:ec2::1]:18080").unwrap_err();
        assert!(
            err.contains("cloud metadata"),
            "fd00:ec2::/32 prefix should be blocked: {err}"
        );
    }

    #[test]
    fn allows_different_ipv6_ula_prefix() {
        assert!(
            validate_cluster_endpoint("http://[fd00:ec3::1]:18080").is_ok(),
            "fd00:ec3:: is not in the fd00:ec2::/32 range and must be allowed"
        );
    }

    #[test]
    fn rejects_alibaba_metadata_ip() {
        let err = validate_cluster_endpoint("http://100.100.100.200:18080").unwrap_err();
        assert!(
            err.contains("cloud metadata"),
            "Alibaba Cloud metadata IP should be blocked: {err}"
        );
    }

    #[test]
    fn accepts_rfc1918_10() {
        assert!(
            validate_cluster_endpoint("http://10.0.0.1:18080").is_ok(),
            "RFC1918 10.x addresses must be allowed for K8s cluster communication"
        );
    }

    #[test]
    fn accepts_rfc1918_172() {
        assert!(
            validate_cluster_endpoint("http://172.16.0.1:18080").is_ok(),
            "RFC1918 172.16+ addresses must be allowed for K8s cluster communication"
        );
        assert!(validate_cluster_endpoint("http://172.15.0.1:18080").is_ok());
    }

    #[test]
    fn accepts_rfc1918_192() {
        assert!(
            validate_cluster_endpoint("http://192.168.1.1:18080").is_ok(),
            "RFC1918 192.168 addresses must be allowed for K8s cluster communication"
        );
    }

    #[test]
    fn accepts_ipv6_ula() {
        assert!(
            validate_cluster_endpoint("http://[fd12::1]:18080").is_ok(),
            "non-IMDS ULA addresses must be allowed for K8s cluster communication"
        );
    }

    #[test]
    fn accepts_k8s_cluster_ip() {
        assert!(
            validate_cluster_endpoint("http://10.96.0.1:18080").is_ok(),
            "typical K8s ClusterIP range must be allowed"
        );
    }

    #[test]
    fn origin_url_strips_path() {
        let url = url::Url::parse("http://runner-0:18080/internal/redirect").unwrap();
        assert_eq!(origin_url(&url), "http://runner-0:18080");
    }

    #[test]
    fn rejects_non_ascii_url() {
        let err =
            validate_cluster_endpoint("http://metadata\u{2161}.google.internal:18080").unwrap_err();
        assert!(
            err.contains("non-ASCII"),
            "error should mention non-ASCII: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_rejects_localhost_dns() {
        let err = resolve_and_validate_cluster_endpoint("http://localhost:18080")
            .await
            .unwrap_err();
        assert!(
            err.contains("blocked"),
            "expected blocked error, got: {err}"
        );
    }

    #[tokio::test]
    async fn resolve_accepts_literal_ip() {
        let result = resolve_and_validate_cluster_endpoint("http://8.8.8.8:18080").await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let (_url, addrs) = result.unwrap();
        assert!(!addrs.is_empty(), "resolved addresses should be non-empty");
        assert_eq!(addrs[0].ip(), "8.8.8.8".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_body("hello", 512), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let long = "a".repeat(1000);
        let result = truncate_body(&long, 512);
        assert!(result.len() < 600, "truncated result should be bounded");
        assert!(result.ends_with("...[truncated]"));
    }
}

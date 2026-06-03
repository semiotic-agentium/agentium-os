// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
    // bypasses (e.g. Unicode homoglyphs that normalize to blocked hostnames).
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

/// Build a reqwest client and the resolved URL for an outbound cluster-peer
/// call. Centralizes SSRF validation, IPv6/IPv4/domain host extraction, URL
/// path rewriting (any attacker-controlled path/query/fragment is stripped),
/// and DNS-pinning (`resolve_to_addrs`) so security-sensitive plumbing lives
/// in one place rather than three near-identical copies across handlers,
/// agent fan-out, and deploy fan-out.
///
/// `path_segment` is appended as a single segment (percent-encoded) under
/// the validated origin, e.g. `path_segment = "agents"` yields
/// `https://host:port/agents`.
///
/// Returned `(client, url)` is ready for `.get(url)` / `.post(url).json(...)`;
/// callers add any per-route headers (e.g. `X-Runner-Token`) and bodies.
pub async fn build_validated_peer_client(
    endpoint: &str,
    path_segment: &str,
    timeout: std::time::Duration,
) -> Result<(reqwest::Client, url::Url), String> {
    let (validated, resolved_addrs) = resolve_and_validate_cluster_endpoint(endpoint).await?;
    let host = match validated.host() {
        Some(url::Host::Domain(d)) => d.to_string(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => return Err("endpoint has no host".to_string()),
    };
    let mut target_url = validated;
    target_url.set_query(None);
    target_url.set_fragment(None);
    target_url
        .path_segments_mut()
        .map_err(|()| "endpoint URL is not a base".to_string())?
        .clear()
        .push(path_segment);
    let client = reqwest::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &resolved_addrs)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    Ok((client, target_url))
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

    struct RejectCase {
        label: &'static str,
        url: &'static str,
        check: fn(&str) -> bool,
    }

    #[test]
    fn cluster_endpoint_rejection_matrix() {
        let cases = [
            RejectCase {
                label: "non_http_scheme",
                url: "ftp://runner-0:18080",
                check: |e| e.contains("scheme"),
            },
            RejectCase {
                label: "loopback_ipv4",
                url: "http://127.0.0.1:18080",
                check: |e| e.contains("loopback"),
            },
            RejectCase {
                label: "loopback_ipv6",
                url: "http://[::1]:18080",
                check: |e| e.contains("loopback"),
            },
            RejectCase {
                label: "unspecified",
                url: "http://0.0.0.0:18080",
                check: |e| e.contains("unspecified"),
            },
            RejectCase {
                label: "link_local_metadata",
                url: "http://169.254.169.254/latest/meta-data",
                check: |e| e.contains("link-local") || e.contains("metadata"),
            },
            RejectCase {
                label: "localhost",
                url: "http://localhost:18080",
                check: |e| e.contains("blocked"),
            },
            RejectCase {
                label: "cloud_metadata_hostname",
                url: "http://metadata.google.internal/v1/instance",
                check: |e| e.contains("blocked"),
            },
            RejectCase {
                label: "azure_metadata_hostname",
                url: "http://metadata.azure.internal/metadata",
                check: |e| e.contains("blocked"),
            },
            RejectCase {
                label: "metadata_subdomain",
                url: "http://evil.metadata.google.internal/v1/instance",
                check: |e| e.contains("blocked"),
            },
            RejectCase {
                label: "aws_ipv6_imds_exact",
                url: "http://[fd00:ec2::254]/latest/meta-data",
                check: |e| e.contains("cloud metadata"),
            },
            RejectCase {
                label: "aws_ipv6_imds_prefix",
                url: "http://[fd00:ec2::1]:18080",
                check: |e| e.contains("cloud metadata"),
            },
            RejectCase {
                label: "alibaba_metadata_ip",
                url: "http://100.100.100.200:18080",
                check: |e| e.contains("cloud metadata"),
            },
            RejectCase {
                label: "non_ascii_url",
                url: "http://metadata\u{2161}.google.internal:18080",
                check: |e| e.contains("non-ASCII"),
            },
        ];
        for case in cases {
            let err = validate_cluster_endpoint(case.url).expect_err(case.label);
            assert!((case.check)(&err), "{}: {err}", case.label);
        }
    }

    struct AllowCase {
        label: &'static str,
        url: &'static str,
        hint: &'static str,
    }

    #[test]
    fn cluster_endpoint_allowance_matrix() {
        let cases = [
            AllowCase {
                label: "valid_cluster",
                url: "http://runner-0.runner.agentium.svc:18080",
                hint: "expected Ok",
            },
            AllowCase {
                label: "valid_https",
                url: "https://runner-1.example.com:443",
                hint: "expected Ok",
            },
            AllowCase {
                label: "different_ipv6_ula_prefix",
                url: "http://[fd00:ec3::1]:18080",
                hint: "fd00:ec3:: is not in the fd00:ec2::/32 range and must be allowed",
            },
            AllowCase {
                label: "rfc1918_10",
                url: "http://10.0.0.1:18080",
                hint: "RFC1918 10.x addresses must be allowed for K8s cluster communication",
            },
            AllowCase {
                label: "rfc1918_172_16",
                url: "http://172.16.0.1:18080",
                hint: "RFC1918 172.16+ addresses must be allowed for K8s cluster communication",
            },
            AllowCase {
                label: "rfc1918_172_15",
                url: "http://172.15.0.1:18080",
                hint: "RFC1918 172.15.x must be allowed",
            },
            AllowCase {
                label: "rfc1918_192",
                url: "http://192.168.1.1:18080",
                hint: "RFC1918 192.168 addresses must be allowed for K8s cluster communication",
            },
            AllowCase {
                label: "ipv6_ula",
                url: "http://[fd12::1]:18080",
                hint: "non-IMDS ULA addresses must be allowed for K8s cluster communication",
            },
            AllowCase {
                label: "k8s_cluster_ip",
                url: "http://10.96.0.1:18080",
                hint: "typical K8s ClusterIP range must be allowed",
            },
        ];
        for case in cases {
            let result = validate_cluster_endpoint(case.url);
            assert!(
                result.is_ok(),
                "{}: {} — got {result:?}",
                case.label,
                case.hint
            );
        }
    }

    #[test]
    fn origin_url_strips_path() {
        let url = url::Url::parse("http://runner-0:18080/internal/redirect").unwrap();
        assert_eq!(origin_url(&url), "http://runner-0:18080");
    }

    struct ResolveCase {
        label: &'static str,
        url: &'static str,
        expect_ok: bool,
        want_ip: Option<&'static str>,
    }

    #[tokio::test]
    async fn resolve_and_validate_cluster_endpoint_matrix() {
        let cases = [
            ResolveCase {
                label: "rejects_localhost_dns",
                url: "http://localhost:18080",
                expect_ok: false,
                want_ip: None,
            },
            ResolveCase {
                label: "accepts_literal_ip",
                url: "http://8.8.8.8:18080",
                expect_ok: true,
                want_ip: Some("8.8.8.8"),
            },
        ];
        for case in cases {
            let result = resolve_and_validate_cluster_endpoint(case.url).await;
            if case.expect_ok {
                assert!(
                    result.is_ok(),
                    "{}: expected Ok, got {result:?}",
                    case.label
                );
                let (_url, addrs) = result.unwrap();
                assert!(
                    !addrs.is_empty(),
                    "{}: resolved addresses should be non-empty",
                    case.label
                );
                if let Some(ip) = case.want_ip {
                    assert_eq!(
                        addrs[0].ip(),
                        ip.parse::<IpAddr>().unwrap(),
                        "{}",
                        case.label
                    );
                }
            } else {
                let err = result.expect_err(case.label);
                assert!(
                    err.contains("blocked"),
                    "{}: expected blocked error, got: {err}",
                    case.label
                );
            }
        }
    }

    #[test]
    fn truncate_body_matrix() {
        assert_eq!(truncate_body("hello", 512), "hello");
        let long = "a".repeat(1000);
        let result = truncate_body(&long, 512);
        assert!(result.len() < 600, "truncated result should be bounded");
        assert!(result.ends_with("...[truncated]"));
    }
}

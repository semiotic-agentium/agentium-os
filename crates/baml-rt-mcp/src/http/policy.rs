// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Network policy + URL guardrails for Streamable HTTP MCP transport.
//!
//! Validated **before** any TCP connection is opened. The transport refuses to
//! `start()` unless this module returns `Ok`. This module does not implement
//! a custom DNS resolver — DNS-rebinding / private-network defense is the
//! egress layer's job (proxy / firewall / service mesh). It only catches what
//! the *app* layer can catch deterministically: scheme, URL shape, host
//! allowlist, literal-IP rejection.
//!
//! Plaintext policy is **intrinsic to operator config**, not an env flag:
//! `http://` is rejected if and only if the resolved transport carries auth
//! secrets (bearer / header / basic) — sending a credential cleartext is
//! always wrong; sending an unauth public request cleartext is the operator's
//! call. Loopback gets no special exception: the rule self-aligns.

use std::net::IpAddr;

use baml_rt_tools::mcp_config::HttpNetworkPolicyConfig;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("MCP server `{server}` url could not be parsed: {reason}")]
    InvalidUrl { server: String, reason: String },
    #[error(
        "MCP server `{server}` configures auth secrets but uses plaintext http:// scheme; switch to https:// or remove auth"
    )]
    PlaintextWithAuth { server: String },
    #[error("MCP server `{server}` url contains embedded credentials; remove userinfo")]
    UserinfoRejected { server: String },
    #[error(
        "MCP server `{server}` url query parameter `{name}` looks secret; declare it under `auth` / `secrets` instead"
    )]
    SecretQueryRejected { server: String, name: String },
    #[error("MCP server `{server}` host `{host}` is not on the configured allowlist")]
    HostNotAllowed { server: String, host: String },
    #[error(
        "MCP server `{server}` host resolves to literal private/loopback IP `{ip}`; set `allow_private_ips: true` to permit"
    )]
    PrivateIpRejected { server: String, ip: String },
    #[error("MCP server `{server}` url policy violation: {reason}")]
    Other { server: String, reason: String },
}

/// Effective policy resolved from operator config.
#[derive(Debug, Clone)]
pub struct EffectivePolicy {
    pub allow_hosts: Vec<String>,
    pub allow_private_ips: bool,
    pub follow_redirects: bool,
}

impl EffectivePolicy {
    pub fn from_config(cfg: &HttpNetworkPolicyConfig, url: &Url) -> Self {
        // Derive allowlist from URL host if operator did not specify one.
        let allow_hosts = if cfg.allow_hosts.is_empty() {
            url.host_str()
                .map(|h| vec![h.to_string()])
                .unwrap_or_default()
        } else {
            cfg.allow_hosts.clone()
        };
        Self {
            allow_hosts,
            allow_private_ips: cfg.allow_private_ips,
            follow_redirects: cfg.follow_redirects,
        }
    }
}

/// Validate the URL + effective policy. `has_auth_secrets` indicates whether
/// the resolver produced any HTTP-injection secret for this server — when
/// true, `http://` is refused unconditionally so a credential can never go
/// over a cleartext wire.
pub fn validate_http_target(
    server: &str,
    raw_url: &str,
    cfg: &HttpNetworkPolicyConfig,
    has_auth_secrets: bool,
) -> Result<(Url, EffectivePolicy), PolicyError> {
    let url = Url::parse(raw_url).map_err(|err| PolicyError::InvalidUrl {
        server: server.into(),
        reason: err.to_string(),
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(PolicyError::InvalidUrl {
            server: server.into(),
            reason: "url must be absolute http(s) URL with host".into(),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PolicyError::UserinfoRejected {
            server: server.into(),
        });
    }
    if let Some(q) = url.query()
        && !q.is_empty()
    {
        // Defense in depth: schema layer already rejects query, but the
        // transport must fail closed if a future config path bypasses it.
        for (k, _) in url.query_pairs() {
            if SECRET_QUERY_KEYS
                .iter()
                .any(|needle| k.to_ascii_lowercase().contains(needle))
            {
                return Err(PolicyError::SecretQueryRejected {
                    server: server.into(),
                    name: k.to_string(),
                });
            }
        }
        return Err(PolicyError::Other {
            server: server.into(),
            reason: "url must not contain query parameters".into(),
        });
    }

    // Plaintext-with-auth rule. The credential would otherwise be visible to
    // anything between this process and the server: pcap, transparent proxy,
    // mistaken on-path logging. Loopback gets no exception — a dev fixture
    // that needs auth can use https with a self-signed cert or drop the auth.
    if url.scheme() == "http" && has_auth_secrets {
        return Err(PolicyError::PlaintextWithAuth {
            server: server.into(),
        });
    }

    let policy = EffectivePolicy::from_config(cfg, &url);
    let host = url.host_str().expect("host present, checked above");

    // Host allowlist. Empty operator list = derived from URL host, so this is
    // a membership check against {url.host} unless explicit hosts were set.
    if !policy
        .allow_hosts
        .iter()
        .any(|allowed| host_matches(host, allowed))
    {
        return Err(PolicyError::HostNotAllowed {
            server: server.into(),
            host: host.to_string(),
        });
    }

    // Literal-IP private/loopback check. DNS-resolved private IPs are NOT
    // checked here — that's an egress-layer responsibility per the plan.
    // Loopback is gated by the same `allow_private_ips` flag as RFC1918; no
    // implicit exception, the operator opts in explicitly.
    if let Some(ip) = url.host().and_then(url_host_ip)
        && is_private_or_loopback(&ip)
        && !policy.allow_private_ips
    {
        return Err(PolicyError::PrivateIpRejected {
            server: server.into(),
            ip: ip.to_string(),
        });
    }

    Ok((url, policy))
}

const SECRET_QUERY_KEYS: &[&str] = &[
    "token", "key", "secret", "password", "auth", "bearer", "apikey",
];

fn host_matches(observed: &str, allowed: &str) -> bool {
    // Case-insensitive exact match. Wildcards/CIDRs are intentionally NOT
    // supported — operator must list literal hosts.
    observed.eq_ignore_ascii_case(allowed)
}

fn url_host_ip(host: url::Host<&str>) -> Option<IpAddr> {
    match host {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(_) => None,
    }
}

fn is_private_or_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            v6.to_ipv4_mapped()
                .as_ref()
                .is_some_and(|v4| is_private_or_loopback(&IpAddr::V4(*v4)))
                || v6.is_loopback()
                || v6.is_multicast()
                // RFC 4193 ULA fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(allow_hosts: &[&str], allow_private: bool) -> HttpNetworkPolicyConfig {
        HttpNetworkPolicyConfig {
            allow_hosts: allow_hosts.iter().map(|s| s.to_string()).collect(),
            allow_private_ips: allow_private,
            follow_redirects: false,
        }
    }

    struct PolicyCase {
        label: &'static str,
        url: &'static str,
        allow_hosts: &'static [&'static str],
        allow_private_ips: bool,
        has_auth_secrets: bool,
        expect_ok: bool,
        check_err: Option<fn(&PolicyError) -> bool>,
    }

    #[test]
    fn http_mcp_target_policy_matrix() {
        let cases = [
            PolicyCase {
                label: "rejects_plain_http_when_auth_present",
                url: "http://example.com/mcp",
                allow_hosts: &["example.com"],
                allow_private_ips: false,
                has_auth_secrets: true,
                expect_ok: false,
                check_err: Some(|e| matches!(e, PolicyError::PlaintextWithAuth { .. })),
            },
            PolicyCase {
                label: "allows_plain_http_when_no_auth",
                url: "http://example.com/mcp",
                allow_hosts: &["example.com"],
                allow_private_ips: false,
                has_auth_secrets: false,
                expect_ok: true,
                check_err: None,
            },
            PolicyCase {
                label: "allows_https_with_auth_and_allowlist",
                url: "https://example.com/mcp",
                allow_hosts: &["example.com"],
                allow_private_ips: false,
                has_auth_secrets: true,
                expect_ok: true,
                check_err: None,
            },
            PolicyCase {
                label: "allows_loopback_when_allow_private_ips_set",
                url: "http://127.0.0.1:8080/mcp",
                allow_hosts: &["127.0.0.1"],
                allow_private_ips: true,
                has_auth_secrets: false,
                expect_ok: true,
                check_err: None,
            },
            PolicyCase {
                label: "rejects_loopback_without_allow_private_ips",
                url: "http://127.0.0.1/mcp",
                allow_hosts: &["127.0.0.1"],
                allow_private_ips: false,
                has_auth_secrets: false,
                expect_ok: false,
                check_err: Some(|e| matches!(e, PolicyError::PrivateIpRejected { .. })),
            },
            PolicyCase {
                label: "rejects_ipv4_mapped_ipv6_private_without_allow_private_ips",
                url: "http://[::ffff:10.0.0.1]/mcp",
                allow_hosts: &[],
                allow_private_ips: false,
                has_auth_secrets: false,
                expect_ok: false,
                check_err: Some(|e| matches!(e, PolicyError::PrivateIpRejected { .. })),
            },
            PolicyCase {
                label: "rejects_userinfo",
                url: "http://u:p@127.0.0.1/mcp",
                allow_hosts: &["127.0.0.1"],
                allow_private_ips: true,
                has_auth_secrets: false,
                expect_ok: false,
                check_err: Some(|e| matches!(e, PolicyError::UserinfoRejected { .. })),
            },
            PolicyCase {
                label: "rejects_secret_query_param",
                url: "http://127.0.0.1/mcp?token=abc",
                allow_hosts: &["127.0.0.1"],
                allow_private_ips: true,
                has_auth_secrets: false,
                expect_ok: false,
                check_err: Some(|e| matches!(e, PolicyError::SecretQueryRejected { .. })),
            },
            PolicyCase {
                label: "rejects_non_allowlisted_host",
                url: "https://evil.example/mcp",
                allow_hosts: &["good.example"],
                allow_private_ips: false,
                has_auth_secrets: true,
                expect_ok: false,
                check_err: Some(|e| matches!(e, PolicyError::HostNotAllowed { .. })),
            },
            PolicyCase {
                label: "rejects_literal_private_ip",
                url: "https://10.0.0.1/mcp",
                allow_hosts: &["10.0.0.1"],
                allow_private_ips: false,
                has_auth_secrets: true,
                expect_ok: false,
                check_err: Some(|e| matches!(e, PolicyError::PrivateIpRejected { .. })),
            },
        ];
        for case in cases {
            let result = validate_http_target(
                "s",
                case.url,
                &cfg(case.allow_hosts, case.allow_private_ips),
                case.has_auth_secrets,
            );
            if case.expect_ok {
                assert!(result.is_ok(), "{}: {result:?}", case.label);
            } else {
                let err = result.expect_err(case.label);
                let check = case.check_err.expect("reject row must supply check_err");
                assert!(check(&err), "{}: {err:?}", case.label);
            }
        }
    }
}

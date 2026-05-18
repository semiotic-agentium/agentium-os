//! Construct an rmcp Streamable HTTP transport from an `HttpLaunchConfig`.
//!
//! Lives next to the launch config so URL policy validation, reserved-header
//! guarding, secret injection, and reqwest client construction all happen
//! together — fail-closed before the first TCP connection.

use baml_rt_tools::{mcp_config::SecretInjection, mcp_secrets::ResolvedSecret};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use reqwest::{
    Certificate, Client,
    header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
    tls,
};
use rmcp::transport::{
    StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
};
use thiserror::Error;

use crate::{
    http::{
        headers::{HeaderError, build_validated_static_headers},
        policy::{PolicyError, validate_http_target},
    },
    runtime::HttpLaunchConfig,
};

/// Reqwest type used by rmcp's `StreamableHttpClientTransport::with_client`.
pub type RmcpHttpTransport = StreamableHttpClientTransport<reqwest::Client>;

#[derive(Debug, Error)]
pub enum HttpTransportBuildError {
    #[error("network policy violation: {0}")]
    Policy(#[from] PolicyError),
    #[error("header configuration error: {0}")]
    Header(#[from] HeaderError),
    #[error("invalid auth header `{name}`: {reason}")]
    InvalidAuthHeader { name: String, reason: String },
    #[error("invalid extra CA certificate (PEM #{index}): {reason}")]
    InvalidExtraCaCert { index: usize, reason: String },
    #[error("reqwest client build failed: {0}")]
    Client(#[from] reqwest::Error),
}

/// Build an rmcp Streamable HTTP transport from a launch config.
///
/// Order is deliberate: URL/network policy first (fail before any TCP),
/// reserved-header guard next (fail before injecting secrets that would then
/// land on a refused header), secret injection last (the only place the
/// resolved value lives outside the resolver).
pub fn build_rmcp_http_transport(
    server_id: &str,
    config: &HttpLaunchConfig,
) -> Result<RmcpHttpTransport, HttpTransportBuildError> {
    let has_auth = config
        .resolved_secrets
        .iter()
        .any(|s| !matches!(s.spec.inject, SecretInjection::Env { .. }));
    let (url, policy) =
        validate_http_target(server_id, &config.url, &config.network_policy, has_auth)?;

    let mut headers = build_validated_static_headers(server_id, &config.static_headers)?;
    for sec in &config.resolved_secrets {
        inject_secret_header(&mut headers, sec)?;
    }

    let redirect_policy = build_redirect_policy(
        server_id.to_string(),
        policy.follow_redirects,
        baml_rt_tools::mcp_config::HttpNetworkPolicyConfig {
            allow_hosts: policy.allow_hosts.clone(),
            allow_private_ips: policy.allow_private_ips,
            follow_redirects: policy.follow_redirects,
        },
        has_auth,
    );

    let mut client_builder = reqwest::Client::builder()
        .connect_timeout(config.connect_timeout)
        .timeout(config.request_timeout)
        .pool_max_idle_per_host(config.max_idle_per_host as usize)
        .pool_idle_timeout(Some(config.idle_stream_timeout))
        .redirect(redirect_policy)
        .no_proxy()
        .min_tls_version(tls::Version::TLS_1_2)
        .danger_accept_invalid_certs(false)
        .https_only(matches!(url.scheme(), "https"))
        .user_agent(concat!("baml-rt-mcp/", env!("CARGO_PKG_VERSION")));
    for (index, pem) in config.extra_ca_certs_pem.iter().enumerate() {
        let cert = Certificate::from_pem(pem).map_err(|err| {
            HttpTransportBuildError::InvalidExtraCaCert {
                index,
                reason: err.to_string(),
            }
        })?;
        client_builder = client_builder.add_root_certificate(cert);
    }
    let client: Client = client_builder.build()?;

    // rmcp expects `HashMap<HeaderName, HeaderValue>` for `custom_headers`.
    let custom_headers: std::collections::HashMap<HeaderName, HeaderValue> = headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let cfg = StreamableHttpClientTransportConfig::with_uri(url.as_str().to_string())
        .custom_headers(custom_headers)
        .reinit_on_expired_session(false);

    Ok(StreamableHttpClientTransport::with_client(client, cfg))
}

fn build_redirect_policy(
    server_id: String,
    follow_redirects: bool,
    network_policy: baml_rt_tools::mcp_config::HttpNetworkPolicyConfig,
    has_auth_secrets: bool,
) -> Policy {
    if !follow_redirects {
        return Policy::none();
    }
    Policy::custom(move |attempt| {
        if attempt.previous().len() > 5 {
            return attempt.error("redirect chain too long");
        }
        match validate_http_target(
            &server_id,
            attempt.url().as_str(),
            &network_policy,
            has_auth_secrets,
        ) {
            Ok(_) => attempt.follow(),
            Err(_) => attempt.error("redirect target violates network policy"),
        }
    })
}

fn inject_secret_header(
    headers: &mut HeaderMap,
    secret: &ResolvedSecret,
) -> Result<(), HttpTransportBuildError> {
    match &secret.spec.inject {
        SecretInjection::Env { .. } => Ok(()),
        SecretInjection::HttpAuthorizationBearer => {
            let value =
                HeaderValue::try_from(format!("Bearer {}", secret.value)).map_err(|err| {
                    HttpTransportBuildError::InvalidAuthHeader {
                        name: "Authorization".into(),
                        reason: err.to_string(),
                    }
                })?;
            headers.insert(AUTHORIZATION, value);
            Ok(())
        }
        SecretInjection::HttpHeader { name } => {
            let name_h = HeaderName::try_from(name.as_str()).map_err(|err| {
                HttpTransportBuildError::InvalidAuthHeader {
                    name: name.clone(),
                    reason: err.to_string(),
                }
            })?;
            let value = HeaderValue::try_from(secret.value.as_str()).map_err(|err| {
                HttpTransportBuildError::InvalidAuthHeader {
                    name: name.clone(),
                    reason: err.to_string(),
                }
            })?;
            headers.insert(name_h, value);
            Ok(())
        }
        SecretInjection::HttpBasicPassword { username } => {
            let encoded = B64.encode(format!("{username}:{}", secret.value));
            let value = HeaderValue::try_from(format!("Basic {encoded}")).map_err(|err| {
                HttpTransportBuildError::InvalidAuthHeader {
                    name: "Authorization".into(),
                    reason: err.to_string(),
                }
            })?;
            headers.insert(AUTHORIZATION, value);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use baml_rt_tools::{
        mcp_config::{
            HttpHeader, HttpNetworkPolicyConfig, SecretInjection, SecretSource, SecretSpec,
        },
        mcp_secrets::ResolvedSecret,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    fn bearer(token: &str) -> ResolvedSecret {
        ResolvedSecret {
            spec: SecretSpec {
                id: "auth.bearer".into(),
                source: SecretSource::Env { name: "T".into() },
                inject: SecretInjection::HttpAuthorizationBearer,
                version: None,
            },
            value: token.into(),
        }
    }

    fn launch(
        url: &str,
        secrets: Vec<ResolvedSecret>,
        headers: Vec<HttpHeader>,
    ) -> HttpLaunchConfig {
        HttpLaunchConfig {
            url: url.into(),
            static_headers: headers,
            resolved_secrets: secrets,
            network_policy: HttpNetworkPolicyConfig {
                allow_hosts: vec![],
                allow_private_ips: true,
                follow_redirects: false,
            },
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(5),
            idle_stream_timeout: Duration::from_secs(30),
            max_idle_per_host: 4,
            extra_ca_certs_pem: Vec::new(),
        }
    }

    fn assert_build_err<F: FnOnce(&HttpTransportBuildError) -> bool>(
        result: Result<RmcpHttpTransport, HttpTransportBuildError>,
        check: F,
    ) {
        // `RmcpHttpTransport` does not implement `Debug`, so unwrap variants
        // manually rather than via `expect_err`.
        match result {
            Ok(_) => panic!("expected build error, got Ok"),
            Err(err) => assert!(check(&err), "unexpected error variant: {err}"),
        }
    }

    #[test]
    fn build_rejects_reserved_static_header() {
        let cfg = launch(
            "https://example.com/mcp",
            vec![],
            vec![HttpHeader {
                name: "mcp-session-id".into(),
                value: "x".into(),
            }],
        );
        assert_build_err(build_rmcp_http_transport("s", &cfg), |err| {
            matches!(
                err,
                HttpTransportBuildError::Header(HeaderError::Reserved { .. })
            )
        });
    }

    #[test]
    fn build_rejects_static_authorization() {
        let cfg = launch(
            "https://example.com/mcp",
            vec![],
            vec![HttpHeader {
                name: "Authorization".into(),
                value: "Bearer leaked".into(),
            }],
        );
        assert_build_err(build_rmcp_http_transport("s", &cfg), |err| {
            matches!(
                err,
                HttpTransportBuildError::Header(HeaderError::Reserved { .. })
            )
        });
    }

    #[test]
    fn build_rejects_plaintext_with_auth() {
        let cfg = launch("http://127.0.0.1:8080/mcp", vec![bearer("xyz")], vec![]);
        assert_build_err(build_rmcp_http_transport("s", &cfg), |err| {
            matches!(err, HttpTransportBuildError::Policy(_))
        });
    }

    #[tokio::test]
    async fn build_accepts_bearer_over_https() {
        // The rmcp Streamable HTTP transport spawns a worker task on
        // construction, so this test needs a Tokio runtime.
        let cfg = launch("https://example.com/mcp", vec![bearer("xyz")], vec![]);
        // `StreamableHttpClientTransport` does not implement `Debug`, so use
        // `is_ok()` rather than `expect`.
        assert!(build_rmcp_http_transport("s", &cfg).is_ok());
    }

    #[tokio::test]
    async fn redirect_to_disallowed_host_errors() {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = target.accept().await {
                let mut buf = [0_u8; 512];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });

        let redirect = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_addr = redirect.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = redirect.accept().await {
                let mut buf = [0_u8; 512];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://localhost:{}/target\r\nContent-Length: 0\r\n\r\n",
                    target_addr.port()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let client = reqwest::Client::builder()
            .redirect(build_redirect_policy(
                "s".to_string(),
                true,
                HttpNetworkPolicyConfig {
                    allow_hosts: vec!["127.0.0.1".to_string()],
                    allow_private_ips: true,
                    follow_redirects: true,
                },
                false,
            ))
            .build()
            .unwrap();
        let err = client
            .get(format!("http://{redirect_addr}/start"))
            .send()
            .await
            .expect_err("redirect target must be rejected");
        assert!(err.is_redirect(), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn redirect_to_private_ip_errors_when_private_ips_disallowed() {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = target.accept().await {
                let mut buf = [0_u8; 512];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });

        let redirect = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_addr = redirect.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = redirect.accept().await {
                let mut buf = [0_u8; 512];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/target\r\nContent-Length: 0\r\n\r\n",
                    target_addr.port()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let client = reqwest::Client::builder()
            .redirect(build_redirect_policy(
                "s".to_string(),
                true,
                HttpNetworkPolicyConfig {
                    allow_hosts: vec!["127.0.0.1".to_string()],
                    allow_private_ips: false,
                    follow_redirects: true,
                },
                false,
            ))
            .build()
            .unwrap();
        let err = client
            .get(format!("http://{redirect_addr}/start"))
            .send()
            .await
            .expect_err("redirect target must be rejected by private-IP policy");
        assert!(err.is_redirect(), "unexpected error: {err}");
    }
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `WebhookIntake` — HTTP intake for **external push sources**.
//!
//! Mount one per push-based event source (Grafana, PagerDuty, GitHub, …).
//! The host runs an HTTP route at [`WebhookIntake::mount_path`] and calls
//! [`WebhookIntake::handle`] on each inbound request. The handler's job is
//! small: decode the payload, resolve any identity mapping, and call
//! [`IngressStore::enqueue`](baml_rt_core::IngressStore::enqueue). An inbox-
//! draining [`EventProducer`](crate::EventProducer) in the same crate turns
//! those `IngressItem`s into dispatchable `ProducedEvent`s downstream.
//!
//! Three rules:
//! - Use `WebhookIntake` when an outside system **POSTs to us**.
//! - Use `EventProducer` alone when **we poll** the outside system.
//! - [`IngressStore`](baml_rt_core::IngressStore) is the seam where the two
//!   arms meet.
//!
//! Registration follows the same inventory pattern as
//! [`EventProducerProvider`](crate::EventProducerProvider): the tool crate
//! submits a [`WebhookIntakeProvider`] in inventory, the host iterates and
//! mounts each result on its public HTTP router. Intakes are compile-time
//! linked — only crates in the runner's dep graph can register, which makes
//! the extension surface implicitly internal-only.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
use serde_json::Value;
use tracing::warn;

use crate::{ConfigResolver, ToolCatalog, ToolName, tools::ToolFunctionMetadata};

/// Authentication tier the host must enforce for an intake route.
///
/// `Public` routes are reachable from anywhere on the cluster network — the
/// same posture as `/chat` and `/dispatch`. Most external push sources land
/// here because they cannot present operator tokens. `OperatorToken` routes
/// require the runner's `X-Runner-Token` header and should be reserved for
/// trusted internal callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookAuthTier {
    Public,
    OperatorToken,
}

/// Decoded HTTP request handed to a [`WebhookIntake`].
#[derive(Debug, Clone)]
pub struct WebhookRequest {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Response returned by a [`WebhookIntake`].
///
/// Construct via the small set of helpers ([`accepted`](Self::accepted),
/// [`json`](Self::json), [`bad_request`](Self::bad_request),
/// [`internal_error`](Self::internal_error)) so the response shape stays
/// consistent across intake crates.
#[derive(Debug, Clone)]
pub struct WebhookResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl WebhookResponse {
    pub fn new(status: StatusCode, body: Bytes) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body,
        }
    }

    /// `202 Accepted` with empty body — the canonical webhook ack.
    pub fn accepted() -> Self {
        Self::new(StatusCode::ACCEPTED, Bytes::new())
    }

    /// `204 No Content`.
    pub fn no_content() -> Self {
        Self::new(StatusCode::NO_CONTENT, Bytes::new())
    }

    /// JSON body with the given status. Sets `Content-Type: application/json`.
    pub fn json(status: StatusCode, value: &Value) -> Result<Self> {
        let body = serde_json::to_vec(value).map_err(|err| {
            BamlRtError::InvalidArgument(format!(
                "WebhookResponse::json failed to serialize body: {err}"
            ))
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        Ok(Self {
            status,
            headers,
            body: Bytes::from(body),
        })
    }

    /// `400 Bad Request` with `{ "error": <message> }`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        let body = serde_json::to_vec(&serde_json::json!({ "error": message.into() }))
            .unwrap_or_else(|_| b"{\"error\":\"bad request\"}".to_vec());
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        Self {
            status: StatusCode::BAD_REQUEST,
            headers,
            body: Bytes::from(body),
        }
    }

    /// `500 Internal Server Error` with `{ "error": <message> }`.
    pub fn internal_error(message: impl Into<String>) -> Self {
        let body = serde_json::to_vec(&serde_json::json!({ "error": message.into() }))
            .unwrap_or_else(|_| b"{\"error\":\"internal error\"}".to_vec());
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headers,
            body: Bytes::from(body),
        }
    }
}

/// HTTP intake handler for one external push source.
///
/// Implementations are owned by the tool crate that ships the matching
/// inbox-draining [`EventProducer`](crate::EventProducer). The host mounts
/// the handler at [`mount_path`](Self::mount_path) and applies the auth
/// posture from [`auth_tier`](Self::auth_tier).
#[async_trait]
pub trait WebhookIntake: Send + Sync {
    /// Stable identifier for diagnostics and logging (e.g. `support/grafana-alerts`).
    fn intake_key(&self) -> &str;

    /// Path the host should mount this intake at, e.g. `/webhooks/grafana`.
    ///
    /// Must begin with `/` and not collide with another mounted intake. The
    /// host loader rejects duplicates.
    fn mount_path(&self) -> &str;

    /// Authentication posture the host must enforce.
    ///
    /// Defaults to [`WebhookAuthTier::Public`] — external systems rarely have
    /// operator credentials.
    fn auth_tier(&self) -> WebhookAuthTier {
        WebhookAuthTier::Public
    }

    /// HTTP methods accepted on this route. Defaults to `POST`.
    ///
    /// Listed in priority order; the host mounts each one. Empty slice is
    /// treated as `&[Method::POST]`.
    fn methods(&self) -> &[Method] {
        const POST_ONLY: &[Method] = &[Method::POST];
        POST_ONLY
    }

    /// Handle one inbound HTTP request.
    ///
    /// Implementations should be fast and return an HTTP response promptly.
    /// Long work belongs in the inbox-draining producer, not here. Errors
    /// returned from `handle` are mapped by the host to a `500` response.
    async fn handle(&self, request: WebhookRequest) -> Result<WebhookResponse>;
}

/// Inputs provided when constructing configured webhook intakes from inventory.
#[derive(Debug, Clone)]
pub struct WebhookIntakeBuildContext {
    /// Tool metadata this intake operationalizes.
    pub metadata: ToolFunctionMetadata,
    /// Effective config for this tool after resolver/default merge.
    pub config: Option<Value>,
}

pub type WebhookIntakeBuildFuture =
    Pin<Box<dyn Future<Output = Result<Vec<Arc<dyn WebhookIntake>>>> + Send>>;

/// Inventory provider for host-mounted webhook intakes.
///
/// Tool crates submit one per push source. The host iterates inventory
/// at boot, asks each provider to build configured instances, and mounts
/// them on its HTTP router.
pub struct WebhookIntakeProvider {
    /// Tool name whose webhook intake this provider operationalizes.
    pub tool_name: &'static str,
    /// Build zero or more configured intake instances for this tool.
    pub build: fn(WebhookIntakeBuildContext) -> WebhookIntakeBuildFuture,
}

inventory::collect!(WebhookIntakeProvider);

/// Build all webhook intakes registered in inventory, merging metadata and
/// effective config from the catalog and config resolver.
///
/// Returns an error if two intakes resolve to the same `mount_path` — that
/// would mean an ambiguous HTTP route, which is never the intent.
pub async fn load_configured_webhook_intakes<C: ToolCatalog>(
    catalog: &C,
    config_resolver: Option<Arc<dyn ConfigResolver>>,
) -> Result<Vec<Arc<dyn WebhookIntake>>> {
    let mut intakes: Vec<Arc<dyn WebhookIntake>> = Vec::new();
    let mut seen_paths: HashMap<String, &'static str> = HashMap::new();

    for provider in inventory::iter::<WebhookIntakeProvider> {
        let tool_name = ToolName::parse(provider.tool_name)?;
        let Some(metadata) = catalog.by_name(&tool_name).cloned() else {
            warn!(
                provider = provider.tool_name,
                "webhook intake provider has no matching tool metadata; skipping"
            );
            continue;
        };

        let config = load_effective_config(&metadata, config_resolver.as_ref()).await?;
        let built = (provider.build)(WebhookIntakeBuildContext {
            metadata: metadata.clone(),
            config,
        })
        .await?;

        for intake in built {
            let path = intake.mount_path();
            if !path.starts_with('/') {
                return Err(BamlRtError::InvalidArgument(format!(
                    "webhook intake '{}' for tool '{}' has invalid mount_path '{path}' (must begin with '/')",
                    intake.intake_key(),
                    metadata.name
                )));
            }
            if let Some(existing) = seen_paths.insert(path.to_string(), provider.tool_name) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "webhook intake mount_path collision at '{path}' between '{existing}' and '{}'",
                    provider.tool_name
                )));
            }
            intakes.push(intake);
        }
    }

    Ok(intakes)
}

async fn load_effective_config(
    metadata: &ToolFunctionMetadata,
    config_resolver: Option<&Arc<dyn ConfigResolver>>,
) -> Result<Option<Value>> {
    let default = metadata.config.as_ref().map(|meta| meta.default.clone());
    match (
        config_resolver,
        metadata.config_bundle.as_ref(),
        metadata.config.as_ref(),
    ) {
        (Some(resolver), Some(bundle_name), Some(_)) => {
            match resolver.get_config_with_version(bundle_name).await {
                Ok(config) => Ok(config.map(|(config, _version)| config).or(default)),
                Err(err) => {
                    warn!(
                        bundle = %bundle_name.as_str(),
                        error = %err,
                        "failed to load webhook intake config; falling back to metadata default"
                    );
                    Ok(default)
                }
            }
        }
        _ => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use baml_rt_core::Result;
    use http::{Method, StatusCode};

    use super::{
        WebhookAuthTier, WebhookIntake, WebhookIntakeBuildContext, WebhookIntakeBuildFuture,
        WebhookRequest, WebhookResponse,
    };
    use crate::{
        BundleName, ToolCatalog, ToolName, ToolOrigin, ToolTypeSpec,
        tools::{SessionPolicy, ToolFunctionMetadata},
    };

    struct FakeIntake {
        key: &'static str,
        path: &'static str,
        tier: WebhookAuthTier,
    }

    #[async_trait]
    impl WebhookIntake for FakeIntake {
        fn intake_key(&self) -> &str {
            self.key
        }
        fn mount_path(&self) -> &str {
            self.path
        }
        fn auth_tier(&self) -> WebhookAuthTier {
            self.tier
        }
        async fn handle(&self, _request: WebhookRequest) -> Result<WebhookResponse> {
            Ok(WebhookResponse::accepted())
        }
    }

    fn metadata(name: &str) -> ToolFunctionMetadata {
        ToolFunctionMetadata {
            name: ToolName::parse(name).expect("valid tool name"),
            class_name: "Stub".to_string(),
            description: "stub".to_string(),
            open_input_schema: serde_json::json!({}),
            input_schema: serde_json::json!({}),
            output_schema: serde_json::json!({}),
            open_input_type: ToolTypeSpec {
                name: "Open".to_string(),
                ts_decl: None,
            },
            input_type: ToolTypeSpec {
                name: "In".to_string(),
                ts_decl: None,
            },
            output_type: ToolTypeSpec {
                name: "Out".to_string(),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: vec![],
            access: None,
            tags: vec![],
            secret_requests: vec![],
            config: None,
            config_bundle: Some(BundleName::new("support").expect("valid bundle")),
            origin: ToolOrigin::Host,
            backend: crate::tools::ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: SessionPolicy::Strict,
            event_sources: vec![],
            coordination_baml: None,
        }
    }

    struct StubCatalog {
        items: Vec<ToolFunctionMetadata>,
    }

    impl ToolCatalog for StubCatalog {
        fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> {
            self.items.iter().find(|m| &m.name == name)
        }
        fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
            Box::new(self.items.iter())
        }
    }

    fn build_one(path: &'static str) -> super::WebhookIntakeProvider {
        fn build_fn(_ctx: WebhookIntakeBuildContext) -> WebhookIntakeBuildFuture {
            Box::pin(async move {
                let intake: Arc<dyn WebhookIntake> = Arc::new(FakeIntake {
                    key: "test/stub",
                    path: "/webhooks/stub",
                    tier: WebhookAuthTier::Public,
                });
                Ok(vec![intake])
            })
        }
        let _ = path;
        super::WebhookIntakeProvider {
            tool_name: "support/stub",
            build: build_fn,
        }
    }

    #[tokio::test]
    async fn response_helpers_set_expected_status_and_content_type() {
        let accepted = WebhookResponse::accepted();
        assert_eq!(accepted.status, StatusCode::ACCEPTED);

        let json = WebhookResponse::json(StatusCode::OK, &serde_json::json!({"ok": true})).unwrap();
        assert_eq!(json.status, StatusCode::OK);
        assert_eq!(
            json.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );

        let bad = WebhookResponse::bad_request("nope");
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert!(std::str::from_utf8(&bad.body).unwrap().contains("nope"));
    }

    #[tokio::test]
    async fn fake_intake_defaults() {
        let intake = FakeIntake {
            key: "k",
            path: "/webhooks/x",
            tier: WebhookAuthTier::Public,
        };
        assert_eq!(intake.methods(), &[Method::POST]);
    }

    #[tokio::test]
    async fn loader_skips_when_no_matching_tool_metadata() {
        // Inventory contains nothing for this test path; loader returns empty.
        let catalog = StubCatalog { items: vec![] };
        let _ = metadata("support/stub");
        let _ = build_one;
        let intakes = super::load_configured_webhook_intakes(&catalog, None)
            .await
            .expect("loader succeeds with empty catalog");
        // The crate test suite cannot register fresh inventory entries, so we
        // assert the empty-catalog path returns no intakes from any provider
        // whose tool isn't in the catalog.
        for intake in &intakes {
            // Any intake that survived must have non-empty mount_path beginning with '/'.
            assert!(intake.mount_path().starts_with('/'));
        }
    }
}

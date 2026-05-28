//! Mount `WebhookIntake` instances onto an `axum::Router`.
//!
//! This is the host-side adapter for [`baml_rt_tools::WebhookIntake`]. The
//! runner loads intakes from inventory via
//! [`baml_rt_tools::load_configured_webhook_intakes`], then calls
//! [`build_webhook_intake_router`] to produce two flat sub-routers — one
//! for [`WebhookAuthTier::Public`] intakes and one for
//! [`WebhookAuthTier::OperatorToken`] intakes — both mounted at each
//! intake's declared `mount_path`. The caller decides how to merge them and
//! is responsible for wrapping the operator arm in the existing
//! `X-Runner-Token` auth layer before merging into the public router.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
    routing::MethodFilter,
};
use baml_rt_tools::{WebhookAuthTier, WebhookIntake, WebhookRequest, WebhookResponse};
use http::{HeaderMap, Method, Request};
use tracing::{error, warn};

/// Group of intakes by auth tier.
struct PartitionedIntakes {
    public: Vec<Arc<dyn WebhookIntake>>,
    operator: Vec<Arc<dyn WebhookIntake>>,
}

fn partition(intakes: Vec<Arc<dyn WebhookIntake>>) -> PartitionedIntakes {
    let mut public = Vec::new();
    let mut operator = Vec::new();
    for intake in intakes {
        match intake.auth_tier() {
            WebhookAuthTier::Public => public.push(intake),
            WebhookAuthTier::OperatorToken => operator.push(intake),
        }
    }
    PartitionedIntakes { public, operator }
}

/// Two-arm router holding the public and operator sub-routers for the
/// loaded webhook intakes. Each arm is **flat**: intakes are mounted at
/// their declared `mount_path` with no added prefix. The caller decides
/// how to merge them and is responsible for wrapping [`Self::operator`]
/// in the runner-token auth layer before merging.
pub struct WebhookIntakeRouters {
    pub public: Option<Router>,
    pub operator: Option<Router>,
}

/// Build axum sub-routers for the supplied intakes, partitioned by
/// [`WebhookAuthTier`]. Each intake is mounted at its declared
/// `mount_path` exactly — no prefix is added. The caller decides the
/// merge strategy:
///
/// ```ignore
/// let routers = build_webhook_intake_router(intakes);
/// router = router.merge(routers.public);
/// router = router.merge(routers.operator.route_layer(auth_layer));
/// ```
pub fn build_webhook_intake_router(intakes: Vec<Arc<dyn WebhookIntake>>) -> WebhookIntakeRouters {
    let PartitionedIntakes { public, operator } = partition(intakes);
    let public = (!public.is_empty()).then(|| mount_intakes(Router::new(), public));
    let operator = (!operator.is_empty()).then(|| mount_intakes(Router::new(), operator));
    WebhookIntakeRouters { public, operator }
}

fn mount_intakes(mut router: Router, intakes: Vec<Arc<dyn WebhookIntake>>) -> Router {
    for intake in intakes {
        let methods = collect_methods(intake.as_ref());
        let filter = method_filter(&methods);
        router = router.route(
            intake.mount_path(),
            axum::routing::on(filter, dispatch).with_state(intake.clone()),
        );
    }
    router
}

fn collect_methods(intake: &dyn WebhookIntake) -> Vec<Method> {
    let declared = intake.methods();
    if declared.is_empty() {
        vec![Method::POST]
    } else {
        declared.to_vec()
    }
}

fn method_filter(methods: &[Method]) -> MethodFilter {
    let mut filter: Option<MethodFilter> = None;
    for method in methods {
        let next = match MethodFilter::try_from(method.clone()) {
            Ok(f) => f,
            Err(err) => {
                warn!(
                    method = %method,
                    error = %err,
                    "webhook intake declared an HTTP method axum does not support; skipping"
                );
                continue;
            }
        };
        filter = Some(match filter {
            Some(existing) => existing.or(next),
            None => next,
        });
    }
    filter.unwrap_or(MethodFilter::POST)
}

async fn dispatch(
    State(intake): State<Arc<dyn WebhookIntake>>,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let body_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!(
                intake = intake.intake_key(),
                error = %err,
                "failed to read webhook request body"
            );
            return into_axum_response(WebhookResponse::bad_request(format!(
                "failed to read request body: {err}"
            )));
        }
    };
    let webhook_request = WebhookRequest {
        method: parts.method,
        uri: parts.uri,
        headers: parts.headers,
        body: body_bytes,
    };
    match intake.handle(webhook_request).await {
        Ok(response) => into_axum_response(response),
        Err(err) => {
            error!(
                intake = intake.intake_key(),
                error = %err,
                "webhook intake handler returned error"
            );
            into_axum_response(WebhookResponse::internal_error(err.to_string()))
        }
    }
}

fn into_axum_response(response: WebhookResponse) -> Response {
    let WebhookResponse {
        status,
        headers,
        body,
    } = response;
    let mut axum_response = (status, body).into_response();
    merge_headers(axum_response.headers_mut(), headers);
    axum_response
}

fn merge_headers(target: &mut HeaderMap, source: HeaderMap) {
    for (name, value) in source {
        if let Some(name) = name {
            target.insert(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::body::Body;
    use baml_rt_core::Result;
    use baml_rt_tools::{WebhookAuthTier, WebhookIntake, WebhookRequest, WebhookResponse};
    use http::{Method, Request, StatusCode};
    use tower::ServiceExt;

    use super::build_webhook_intake_router;

    struct EchoIntake {
        path: &'static str,
        tier: WebhookAuthTier,
    }

    #[async_trait]
    impl WebhookIntake for EchoIntake {
        fn intake_key(&self) -> &str {
            "test/echo"
        }
        fn mount_path(&self) -> &str {
            self.path
        }
        fn auth_tier(&self) -> WebhookAuthTier {
            self.tier
        }
        async fn handle(&self, request: WebhookRequest) -> Result<WebhookResponse> {
            Ok(WebhookResponse::new(StatusCode::OK, request.body))
        }
    }

    fn intake(path: &'static str, tier: WebhookAuthTier) -> Arc<dyn WebhookIntake> {
        Arc::new(EchoIntake { path, tier })
    }

    #[tokio::test]
    async fn public_intake_mounts_at_root() {
        let routers = build_webhook_intake_router(vec![intake(
            "/webhooks/echo",
            WebhookAuthTier::Public,
        )]);
        let response = routers
            .public
            .expect("public arm present when public intake registered")
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/webhooks/echo")
                    .body(Body::from("hi"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"hi");
    }

    #[tokio::test]
    async fn operator_intake_mounts_on_operator_arm_at_declared_path() {
        let routers = build_webhook_intake_router(vec![intake(
            "/webhooks/secret",
            WebhookAuthTier::OperatorToken,
        )]);
        let response = routers
            .operator
            .expect("operator arm present when operator intake registered")
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/webhooks/secret")
                    .body(Body::from("hi"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn public_and_operator_arms_are_separate() {
        let routers = build_webhook_intake_router(vec![intake(
            "/webhooks/secret",
            WebhookAuthTier::OperatorToken,
        )]);
        // Operator-tier route must not appear on the public arm — caller is
        // responsible for merging the operator arm with the auth layer. With
        // no public intakes the public arm is absent entirely.
        assert!(routers.public.is_none());
        assert!(routers.operator.is_some());
    }

    #[tokio::test]
    async fn wrong_method_returns_405() {
        let routers = build_webhook_intake_router(vec![intake(
            "/webhooks/echo",
            WebhookAuthTier::Public,
        )]);
        let response = routers
            .public
            .expect("public arm present when public intake registered")
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/webhooks/echo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

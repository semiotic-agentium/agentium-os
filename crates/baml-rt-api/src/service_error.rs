//! Unified service error type for provenance-backed API services.

use std::{error::Error, fmt};

/// Common error type for provenance-backed service traits.
///
/// All provenance services (mermaid, metrics, planning, provenance ops, episode)
/// share the same error shape: `NotFound | Unavailable | Other`.
#[derive(Debug)]
pub enum ServiceError {
    /// No data found for the given scope.
    NotFound,
    /// Service or store unavailable (e.g. provenance not configured).
    Unavailable,
    /// Other error (e.g. storage/query failure).
    Other(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::NotFound => write!(f, "not found"),
            ServiceError::Unavailable => write!(f, "service unavailable"),
            ServiceError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl Error for ServiceError {}

/// Map a `ServiceError` into an RFC 7807 HTTP response with metrics recording.
///
/// Consolidates the `NotFound → 404, Unavailable → 501, Other → 500` pattern
/// used across all provenance service handlers.
#[expect(
    clippy::result_large_err,
    reason = "HttpApiProblem is the service error type shared across all provenance handlers"
)]
pub fn service_result_to_http<T: serde::Serialize>(
    route: &str,
    start: std::time::Instant,
    result: Result<T, ServiceError>,
) -> Result<axum::Json<T>, http_api_problem::HttpApiProblem> {
    match result {
        Ok(val) => {
            crate::metrics::record_request(route, "success", start.elapsed());
            Ok(axum::Json(val))
        }
        Err(ServiceError::NotFound) => {
            crate::metrics::record_request(route, "not_found", start.elapsed());
            Err(http_api_problem::HttpApiProblem::try_new(404)
                .expect("404 is valid")
                .title("Not Found")
                .detail("resource not found"))
        }
        Err(ServiceError::Unavailable) => {
            crate::metrics::record_request(route, "unavailable", start.elapsed());
            Err(http_api_problem::HttpApiProblem::try_new(501)
                .expect("501 is valid")
                .title("Not Implemented")
                .detail("service unavailable"))
        }
        Err(ServiceError::Other(e)) => {
            crate::metrics::record_request(route, "internal", start.elapsed());
            Err(http_api_problem::HttpApiProblem::try_new(500)
                .expect("500 is valid")
                .title("Internal Server Error")
                .detail(e.to_string()))
        }
    }
}

//! Per-route request deadline middleware.
//!
//! Replaces a single router-wide `tower::timeout` layer so that operations can
//! declare their own deadline (`.request_timeout(..)` on `OperationBuilder`) or
//! opt out entirely (`.no_request_timeout()`, for long-lived streaming routes).
//! Routes that declare nothing inherit `defaults.request_timeout`. An optional
//! `defaults.max_request_timeout` caps longer deadlines and opt-outs.
//!
//! Requests that match no gateway-owned route (the reverse-proxy fallback)
//! consult the [`toolkit_gateway::ProxyRegistry`], which carries the deadline
//! each upstream gear published on its `x-toolkit-request-timeout` `OpenAPI`
//! extension — subject to the edge's configured maximum deadline.

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use http::Method;
use std::sync::Arc;
use std::time::Duration;
use toolkit::api::{OperationSpec, RouteTimeout};
use toolkit_gateway::ProxyRegistry;

use crate::middleware::common;
use crate::middleware::errors::ApiGatewayGatewayError;

/// The deadline a route resolved to: `Some` bounds the pipeline, `None`
/// means the route opted out of any server-side timeout.
type RouteDeadline = Option<Duration>;

/// Shared state for [`request_timeout_middleware`]: the per-route overrides
/// built from operation specs, the gateway-wide default, and (when the
/// reverse-proxy fallback is enabled) the proxied route table.
pub struct RequestTimeoutState {
    overrides: DashMap<(Method, String), RouteDeadline>,
    default: Duration,
    max: Option<Duration>,
    proxy_registry: Option<Arc<ProxyRegistry>>,
}

/// Build the middleware state from the registered operation specs.
///
/// `proxy_registry` is shared with the proxy fallback: proxied routes declare
/// their deadline through the `x-toolkit-request-timeout` extension on the
/// upstream gear's `OpenAPI` document.
#[must_use]
pub fn build_request_timeout_state(
    specs: &[OperationSpec],
    default: Duration,
    max: Option<Duration>,
    proxy_registry: Option<Arc<ProxyRegistry>>,
) -> Arc<RequestTimeoutState> {
    let overrides = DashMap::new();
    for spec in specs {
        let declared = match spec.request_timeout {
            RouteTimeout::Inherit => continue,
            RouteTimeout::Disabled => match max {
                Some(max) => {
                    tracing::warn!(
                        method = %spec.method,
                        path = %spec.path,
                        max_timeout = %humantime_serde::re::humantime::format_duration(max),
                        "route timeout opt-out limited by defaults.max_request_timeout"
                    );
                    Some(max)
                }
                None => None,
            },
            RouteTimeout::Custom(timeout) => match max {
                Some(max) if timeout > max => {
                    tracing::warn!(
                        method = %spec.method,
                        path = %spec.path,
                        declared_timeout = %humantime_serde::re::humantime::format_duration(timeout),
                        max_timeout = %humantime_serde::re::humantime::format_duration(max),
                        "route timeout capped by defaults.max_request_timeout"
                    );
                    Some(max)
                }
                _ => Some(timeout),
            },
        };
        overrides.insert((spec.method.clone(), spec.path.clone()), declared);
    }
    Arc::new(RequestTimeoutState {
        overrides,
        default,
        max,
        proxy_registry,
    })
}

impl RequestTimeoutState {
    fn resolve_proxy_timeout(&self, declared: toolkit_gateway::RouteTimeout) -> RouteDeadline {
        match (declared, self.max) {
            (toolkit_gateway::RouteTimeout::Inherit, _) => Some(self.default),
            (toolkit_gateway::RouteTimeout::Custom(timeout), Some(max)) if timeout > max => {
                Some(max)
            }
            (toolkit_gateway::RouteTimeout::Disabled, Some(max)) => Some(max),
            (toolkit_gateway::RouteTimeout::Disabled, None) => None,
            (toolkit_gateway::RouteTimeout::Custom(timeout), _) => Some(timeout),
        }
    }

    /// The deadline for this request, or `None` when the route opted out.
    /// Resolution order: the route's own declaration, the upstream's
    /// declaration for a proxied route, else the gateway-wide default.
    fn resolve(&self, req: &Request) -> RouteDeadline {
        let method = req.method();
        // Strip `prefix_path`: spec paths and proxy templates are unprefixed.
        let path = common::resolve_path(
            req,
            req.extensions()
                .get::<MatchedPath>()
                .map_or_else(|| req.uri().path(), MatchedPath::as_str),
        );

        if let Some(declared) = self.overrides.get(&(method.clone(), path.clone())) {
            return *declared;
        }
        let proxied_timeout = self
            .proxy_registry
            .as_ref()
            .and_then(|registry| registry.request_timeout(method, &path));
        match proxied_timeout {
            Some(declared) => self.resolve_proxy_timeout(declared),
            None => Some(self.default),
        }
    }
}

/// Canonical `deadline_exceeded` Problem response, matching the body the
/// previous `tower::timeout` layer produced.
fn deadline_exceeded_response(timeout: Duration) -> Response {
    ApiGatewayGatewayError::deadline_exceeded(format!(
        "Request exceeded {} timeout",
        humantime_serde::re::humantime::format_duration(timeout)
    ))
    .create()
    .into_response()
}

/// Apply the resolved deadline around the rest of the pipeline.
///
/// The timer covers everything inside it (auth, throttling, the handler
/// itself), exactly as the replaced `tower::timeout` layer did.
pub async fn request_timeout_middleware(
    axum::extract::State(state): axum::extract::State<Arc<RequestTimeoutState>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(timeout) = state.resolve(&req) else {
        return next.run(req).await;
    };
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    if let Ok(response) = tokio::time::timeout(timeout, next.run(req)).await {
        response
    } else {
        tracing::warn!(
            method = %method,
            path = %path,
            "request exceeded its deadline"
        );
        deadline_exceeded_response(timeout)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_gateway::{Endpoint, RouteTemplate};

    fn spec(method: Method, path: &str, timeout: RouteTimeout) -> OperationSpec {
        OperationSpec {
            method,
            path: path.to_owned(),
            operation_id: None,
            summary: None,
            description: None,
            tags: Vec::new(),
            params: Vec::new(),
            request_body: None,
            responses: Vec::new(),
            handler_id: "test".to_owned(),
            auth: toolkit::api::RouteAuth::Anonymous,
            exposed: true,
            throttling: None,
            allowed_request_content_types: None,
            vendor_extensions: toolkit::api::operation_builder::VendorExtensions::default(),
            license_requirement: None,
            request_timeout: timeout,
        }
    }

    // Test paths are literals, so the URI equals the route template and no
    // `MatchedPath` extension is needed (resolve() falls back to the URI).
    fn request(method: Method, uri: &str) -> Request {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[test]
    fn route_with_custom_timeout_uses_its_own_deadline() {
        let state = build_request_timeout_state(
            &[spec(
                Method::POST,
                "/eb/v1/events",
                RouteTimeout::Custom(Duration::from_secs(60)),
            )],
            Duration::from_secs(30),
            None,
            None,
        );
        let req = request(Method::POST, "/eb/v1/events");
        assert_eq!(state.resolve(&req), Some(Duration::from_secs(60)));
    }

    #[test]
    fn streaming_route_disables_the_deadline() {
        let state = build_request_timeout_state(
            &[spec(
                Method::GET,
                "/eb/v1/events:sse",
                RouteTimeout::Disabled,
            )],
            Duration::from_secs(30),
            None,
            None,
        );
        let req = request(Method::GET, "/eb/v1/events:sse");
        assert_eq!(state.resolve(&req), None);
    }

    #[test]
    fn undeclared_route_inherits_the_default() {
        let state = build_request_timeout_state(
            &[spec(Method::GET, "/eb/v1/topics", RouteTimeout::Inherit)],
            Duration::from_secs(30),
            None,
            None,
        );
        let req = request(Method::GET, "/eb/v1/topics");
        assert_eq!(state.resolve(&req), Some(Duration::from_secs(30)));
    }

    #[test]
    fn unmatched_request_falls_back_to_the_default() {
        let state = build_request_timeout_state(&[], Duration::from_secs(30), None, None);
        let req = request(Method::GET, "/nowhere");
        assert_eq!(state.resolve(&req), Some(Duration::from_secs(30)));
    }

    #[test]
    fn proxied_route_uses_the_upstream_declared_deadline() {
        let registry = Arc::new(ProxyRegistry::new());
        registry.register(
            "event-broker".into(),
            "i1",
            Endpoint::parse("http://10.0.0.1:8080").unwrap(),
            vec![
                RouteTemplate::new(Method::POST, "/eb/v1/events", true).with_request_timeout(
                    toolkit_gateway::RouteTimeout::Custom(Duration::from_secs(60)),
                ),
                RouteTemplate::new(Method::GET, "/eb/v1/events:sse", true)
                    .with_request_timeout(toolkit_gateway::RouteTimeout::Disabled),
            ],
        );
        let state = build_request_timeout_state(
            &[],
            Duration::from_secs(30),
            None,
            Some(Arc::clone(&registry)),
        );

        // Fallback requests carry no MatchedPath.
        let publish = request(Method::POST, "/eb/v1/events");
        assert_eq!(state.resolve(&publish), Some(Duration::from_secs(60)));
        let sse = request(Method::GET, "/eb/v1/events:sse");
        assert_eq!(state.resolve(&sse), None);

        let capped_state = build_request_timeout_state(
            &[],
            Duration::from_secs(30),
            Some(Duration::from_secs(20)),
            Some(registry),
        );
        assert_eq!(capped_state.resolve(&sse), Some(Duration::from_secs(20)));
    }

    #[test]
    fn configured_cap_limits_custom_and_disabled_routes() {
        let state = build_request_timeout_state(
            &[
                spec(
                    Method::GET,
                    "/slow",
                    RouteTimeout::Custom(Duration::from_secs(60)),
                ),
                spec(Method::GET, "/stream", RouteTimeout::Disabled),
            ],
            Duration::from_secs(30),
            Some(Duration::from_secs(45)),
            None,
        );

        assert_eq!(
            state.resolve(&request(Method::GET, "/slow")),
            Some(Duration::from_secs(45))
        );
        assert_eq!(
            state.resolve(&request(Method::GET, "/stream")),
            Some(Duration::from_secs(45))
        );
    }

    #[tokio::test]
    async fn nested_in_process_route_uses_its_declared_timeout() {
        let state = build_request_timeout_state(
            &[spec(
                Method::GET,
                "/v1/events",
                RouteTimeout::Custom(Duration::from_millis(20)),
            )],
            Duration::from_millis(200),
            None,
            None,
        );
        let inner = axum::Router::new()
            .route(
                "/v1/events",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    "done"
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state,
                request_timeout_middleware,
            ));
        let router = axum::Router::new().nest("/api", inner);

        let response = tower::ServiceExt::oneshot(router, request(Method::GET, "/api/v1/events"))
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn nested_proxy_fallback_uses_capped_upstream_timeout() {
        let registry = Arc::new(ProxyRegistry::new());
        registry.register(
            "event-broker".into(),
            "i1",
            Endpoint::parse("http://10.0.0.1:8080").unwrap(),
            vec![
                RouteTemplate::new(Method::GET, "/v1/{event}", true).with_request_timeout(
                    toolkit_gateway::RouteTimeout::Custom(Duration::from_secs(60)),
                ),
            ],
        );
        let state = build_request_timeout_state(
            &[],
            Duration::from_millis(200),
            Some(Duration::from_millis(20)),
            Some(registry),
        );
        let inner = axum::Router::new()
            .fallback(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "proxied"
            })
            .layer(axum::middleware::from_fn_with_state(
                state,
                request_timeout_middleware,
            ));
        let router = axum::Router::new().nest("/api", inner);

        let response = tower::ServiceExt::oneshot(router, request(Method::GET, "/api/v1/events"))
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::GATEWAY_TIMEOUT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("20ms"),
            "deadline response should report the applied cap"
        );
    }

    #[tokio::test]
    async fn a_slow_handler_on_a_default_route_gets_deadline_exceeded() {
        use axum::response::IntoResponse;

        let state = build_request_timeout_state(&[], Duration::from_millis(20), None, None);
        let router = axum::Router::new()
            .route(
                "/slow",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "done".into_response()
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state,
                request_timeout_middleware,
            ));

        let response = tower::ServiceExt::oneshot(router, request(Method::GET, "/slow"))
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn a_slow_handler_on_a_disabled_route_completes() {
        use axum::response::IntoResponse;

        let state = build_request_timeout_state(
            &[spec(Method::GET, "/stream", RouteTimeout::Disabled)],
            Duration::from_millis(20),
            None,
            None,
        );
        let router = axum::Router::new()
            .route(
                "/stream",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    "done".into_response()
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state,
                request_timeout_middleware,
            ));

        let response = tower::ServiceExt::oneshot(router, request(Method::GET, "/stream"))
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
    }
}

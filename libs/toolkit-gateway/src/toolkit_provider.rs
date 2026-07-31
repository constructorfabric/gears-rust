//! In-process [`GatewayProvider`] backed by a shared [`ProxyRegistry`].
//!
//! [`ToolKitGatewayProvider`] parses the public routes out of a gear's
//! `OpenAPI` document (those carrying the
//! [`API_VISIBILITY_EXTENSION`](crate::API_VISIBILITY_EXTENSION) vendor
//! extension) and writes them into the registry the [`Forwarder`](crate::Forwarder)
//! reads. Pair it with a `Forwarder` sharing the same `Arc<ProxyRegistry>`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::GatewayError;
use crate::provider::GatewayProvider;
use crate::registry::{ProxyRegistry, RouteTemplate};
use crate::types::{Endpoint, GearName, OpenApiSpec};
use crate::{API_VISIBILITY_EXTENSION, API_VISIBILITY_PUBLIC};

/// HTTP method keys recognized when scanning an `OpenAPI` path item.
const HTTP_METHOD_KEYS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

/// A [`GatewayProvider`] that reverse-proxies through the built-in `api-gateway`
/// by updating a shared in-process [`ProxyRegistry`].
pub struct ToolKitGatewayProvider {
    registry: Arc<ProxyRegistry>,
}

impl ToolKitGatewayProvider {
    /// Builds a provider that writes into `registry`.
    #[must_use]
    pub fn new(registry: Arc<ProxyRegistry>) -> Self {
        Self { registry }
    }

    /// Returns the shared registry, e.g. to build a [`Forwarder`](crate::Forwarder)
    /// over the same route table.
    #[must_use]
    pub fn registry(&self) -> &Arc<ProxyRegistry> {
        &self.registry
    }
}

#[async_trait]
impl GatewayProvider for ToolKitGatewayProvider {
    async fn register_routes(
        &self,
        gear: &GearName,
        spec: OpenApiSpec<'_>,
        endpoint: &Endpoint,
    ) -> Result<(), GatewayError> {
        let templates = extract_public_routes(&spec)?;
        tracing::info!(
            gear = %gear,
            endpoint = %endpoint.authority(),
            routes = templates.len(),
            "registering gear proxy routes",
        );
        self.registry
            .register(gear.clone(), endpoint.clone(), templates);
        Ok(())
    }

    async fn deregister_routes(&self, gear: &GearName) -> Result<(), GatewayError> {
        let removed = self.registry.deregister(gear);
        tracing::info!(gear = %gear, removed, "deregistering gear proxy routes");
        Ok(())
    }
}

/// Extract the public route templates from an `OpenAPI` document.
///
/// Walks the document's JSON form (uniformly for all [`OpenApiSpec`] variants)
/// and returns every `(method, path)` whose operation carries
/// `x-cf-api-visibility: public`. A document with no public operations yields an
/// empty list (not an error).
///
/// # Errors
/// Returns [`GatewayError::InvalidSpec`] if the document cannot be serialized or
/// parsed to JSON.
fn extract_public_routes(spec: &OpenApiSpec<'_>) -> Result<Vec<RouteTemplate>, GatewayError> {
    let doc: serde_json::Value = match spec {
        OpenApiSpec::Owned(api) => to_value(api.as_ref())?,
        OpenApiSpec::Borrowed(api) => to_value(api)?,
        OpenApiSpec::SerializedJson(bytes) => {
            serde_json::from_slice(bytes.as_ref()).map_err(|err| GatewayError::InvalidSpec {
                reason: err.to_string(),
            })?
        }
    };

    let mut routes = Vec::new();
    let Some(paths) = doc.get("paths").and_then(serde_json::Value::as_object) else {
        return Ok(routes);
    };

    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for key in HTTP_METHOD_KEYS {
            let Some(operation) = item.get(key).and_then(serde_json::Value::as_object) else {
                continue;
            };
            let is_public = operation
                .get(API_VISIBILITY_EXTENSION)
                .and_then(serde_json::Value::as_str)
                == Some(API_VISIBILITY_PUBLIC);
            if !is_public {
                continue;
            }
            let method =
                http::Method::from_bytes(key.to_uppercase().as_bytes()).map_err(|err| {
                    GatewayError::InvalidSpec {
                        reason: format!("invalid HTTP method '{key}': {err}"),
                    }
                })?;
            // Authenticated iff the operation carries a non-empty `security`
            // requirement (emitted for `OperationBuilder::authenticated()`).
            // An exposed-but-anonymous route has no `security` entry.
            let authenticated = operation
                .get("security")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|reqs| !reqs.is_empty());
            routes.push(RouteTemplate::new(method, path.clone(), authenticated));
        }
    }

    Ok(routes)
}

/// Serialize an `OpenAPI` document to a JSON value, mapping errors to
/// [`GatewayError::InvalidSpec`].
fn to_value(api: &utoipa::openapi::OpenApi) -> Result<serde_json::Value, GatewayError> {
    serde_json::to_value(api).map_err(|err| GatewayError::InvalidSpec {
        reason: err.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::{ToolKitGatewayProvider, extract_public_routes};
    use crate::provider::GatewayProvider;
    use crate::registry::ProxyRegistry;
    use crate::types::{Endpoint, GearName, OpenApiSpec};
    use std::sync::Arc;

    /// Minimal `OpenAPI` doc: one public+authenticated GET, one public+anonymous
    /// POST (no `security`), and one internal GET (not exposed).
    fn sample_spec_json() -> Bytes {
        let doc = serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "calc", "version": "1.0.0" },
            "paths": {
                "/calc/v1/items/{id}": {
                    "get": {
                        "x-cf-api-visibility": "public",
                        "security": [{ "bearerAuth": [] }],
                        "responses": {}
                    },
                    "post": { "x-cf-api-visibility": "public", "responses": {} }
                },
                "/calc/v1/internal": {
                    "get": { "security": [{ "bearerAuth": [] }], "responses": {} }
                }
            }
        });
        Bytes::from(serde_json::to_vec(&doc).expect("serialize sample"))
    }

    #[test]
    fn extract_only_public_routes() {
        let spec = OpenApiSpec::SerializedJson(sample_spec_json());
        let mut routes = extract_public_routes(&spec).expect("parse spec");
        routes.sort_by(|a, b| (a.method.as_str(), &a.path).cmp(&(b.method.as_str(), &b.path)));

        assert_eq!(routes.len(), 2);
        assert!(routes.iter().all(|r| r.path == "/calc/v1/items/{id}"));
        // GET has a `security` requirement -> authenticated; POST has none -> anonymous.
        let get = routes
            .iter()
            .find(|r| r.method == http::Method::GET)
            .expect("public GET");
        assert!(get.authenticated, "GET with security must be authenticated");
        let post = routes
            .iter()
            .find(|r| r.method == http::Method::POST)
            .expect("public POST");
        assert!(
            !post.authenticated,
            "POST without security must be anonymous"
        );
    }

    #[test]
    fn spec_without_public_routes_is_empty_not_error() {
        let doc = serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "x", "version": "1.0.0" },
            "paths": { "/x/v1/y": { "get": { "responses": {} } } }
        });
        let spec =
            OpenApiSpec::SerializedJson(Bytes::from(serde_json::to_vec(&doc).expect("serialize")));
        assert!(extract_public_routes(&spec).expect("parse").is_empty());
    }

    #[tokio::test]
    async fn provider_registers_and_deregisters() {
        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        let gear = GearName::from("calculator");
        let endpoint = Endpoint::parse("http://calculator:8080").expect("endpoint");

        provider
            .register_routes(
                &gear,
                OpenApiSpec::SerializedJson(sample_spec_json()),
                &endpoint,
            )
            .await
            .expect("register");

        assert!(registry.match_path("/calc/v1/items/7").is_some());
        assert!(registry.match_path("/calc/v1/internal").is_none());

        provider.deregister_routes(&gear).await.expect("deregister");
        assert!(registry.match_path("/calc/v1/items/7").is_none());
    }
}

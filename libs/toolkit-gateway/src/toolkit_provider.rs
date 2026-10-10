//! In-process [`GatewayProvider`] backed by a shared [`ProxyRegistry`].
//!
//! [`ToolKitGatewayProvider`] parses the public routes out of a gear's
//! `OpenAPI` document (those carrying the
//! [`API_VISIBILITY_EXTENSION`](crate::API_VISIBILITY_EXTENSION) vendor
//! extension) and writes them into the registry the [`Forwarder`](crate::Forwarder)
//! reads. Pair it with a `Forwarder` sharing the same `Arc<ProxyRegistry>`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::error::GatewayError;
use crate::provider::{GatewayProvider, InstanceSpec};
use crate::registry::{InstanceRoutes, ProxyRegistry, RouteTemplate};
use crate::types::{Endpoint, GearInstance, GearName, OpenApiSpec};
use crate::{API_VISIBILITY_EXPOSED, API_VISIBILITY_EXTENSION};
use toolkit_security::constants::INTERNAL_TOKEN_SECURITY_SCHEME;

/// HTTP method keys recognized when scanning an `OpenAPI` path item.
const HTTP_METHOD_KEYS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

/// Platform-authenticated `(method, path)` routes of one gear.
type PlatformRoutes = HashSet<(http::Method, String)>;

/// A [`GatewayProvider`] that reverse-proxies through the built-in `api-gateway`
/// by updating a shared in-process [`ProxyRegistry`].
pub struct ToolKitGatewayProvider {
    registry: Arc<ProxyRegistry>,
    /// Platform routes already warned about, per gear: the gear's latest reported set, so
    /// repeated polls stay quiet, retired paths are forgotten, and a gear with no
    /// registered instance left is dropped.
    reported_platform: Mutex<HashMap<GearName, PlatformRoutes>>,
}

impl ToolKitGatewayProvider {
    /// Builds a provider that writes into `registry`.
    #[must_use]
    pub fn new(registry: Arc<ProxyRegistry>) -> Self {
        Self {
            registry,
            reported_platform: Mutex::new(HashMap::new()),
        }
    }

    /// Warn about exposed platform operations that cannot be published, once per route while
    /// the gear keeps declaring it. The gear's set is replaced by `platform`, so a retired
    /// route is forgotten and warns again if it comes back.
    fn report_platform(
        &self,
        gear: &GearName,
        instance_id: &str,
        platform: &[(http::Method, String)],
    ) {
        let current: PlatformRoutes = platform.iter().cloned().collect();
        let mut reported = self.reported_platform.lock();
        let previous = reported.remove(gear).unwrap_or_default();
        for (method, path) in current.difference(&previous) {
            tracing::warn!(
                gear = %gear,
                instance_id,
                method = %method,
                path = %path,
                "exposed operation is platform-authenticated; not published as a proxied route",
            );
        }
        if !current.is_empty() {
            reported.insert(gear.clone(), current);
        }
    }

    /// Forget the platform warnings of gears that no longer have a registered instance.
    fn prune_reported(&self) {
        self.reported_platform
            .lock()
            .retain(|gear, _| self.registry.contains_gear(gear));
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
        instance_id: &str,
        spec: OpenApiSpec<'_>,
        endpoint: &Endpoint,
    ) -> Result<(), GatewayError> {
        let PublicRoutes {
            routes: templates,
            platform,
        } = extract_public_routes(&spec)?;
        self.report_platform(gear, instance_id, &platform);
        tracing::info!(
            gear = %gear,
            instance_id,
            endpoint = %endpoint.authority(),
            routes = templates.len(),
            "registering gear proxy routes",
        );
        self.registry
            .register(gear.clone(), instance_id, endpoint.clone(), templates);
        Ok(())
    }

    async fn deregister_routes(
        &self,
        gear: &GearName,
        instance_id: &str,
    ) -> Result<(), GatewayError> {
        let removed = self.registry.deregister(gear, instance_id);
        self.prune_reported();
        tracing::info!(gear = %gear, instance_id, removed, "deregistering gear proxy routes");
        Ok(())
    }

    fn registered_instances(&self) -> Vec<GearInstance> {
        self.registry.registered_instances()
    }

    /// Reconciles a full directory snapshot into the registry with a **single**
    /// router rebuild: `instances` are registered (public routes extracted from
    /// each spec) and `removals` are dropped.
    ///
    /// This overrides the trait default (which would apply each instance one at
    /// a time, rebuilding the router per change) to batch the whole snapshot
    /// into one [`ProxyRegistry::apply`] call.
    ///
    /// An instance whose spec cannot be parsed is logged and skipped so one bad
    /// gear cannot stall the rest; its previous routes (if any) are retained
    /// because it is neither re-registered nor removed.
    ///
    /// Public routes are extracted **once per gear**: co-located instances of
    /// the same gear (Profile 2, Host + Workers) advertise an identical
    /// `OpenAPI` document, so the first instance's parse is cached (keyed by
    /// [`GearName`]) and reused for the rest rather than re-parsing the same
    /// document per instance. The cache is local to this call, so a spec change
    /// observed on a later poll is re-parsed fresh.
    ///
    /// Because the first instance's routes are applied to *every* instance of a
    /// gear, replicas that transiently run different versions during a rolling
    /// upgrade share one route set until the rollout converges (see the
    /// aggregation note in [`ProxyRegistry`]). This is acceptable — the upstream
    /// gear re-validates and owns its own routing — and converging mixed
    /// versions exactly would require a spec per instance, which the discovery
    /// snapshot does not carry.
    async fn apply_snapshot<'a>(
        &self,
        instances: Vec<InstanceSpec<'a>>,
        removals: &[GearInstance],
    ) {
        let mut additions = Vec::with_capacity(instances.len());
        // Per-gear extraction cache. `None` records a gear whose spec failed to
        // parse (already logged) so the failure is not re-reported for every
        // co-located instance.
        let mut route_cache: HashMap<GearName, Option<Vec<RouteTemplate>>> = HashMap::new();
        for inst in instances {
            let templates = route_cache
                .entry(inst.gear.clone())
                .or_insert_with(|| match extract_public_routes(&inst.spec) {
                    Ok(PublicRoutes { routes, platform }) => {
                        self.report_platform(&inst.gear, &inst.instance_id, &platform);
                        Some(routes)
                    }
                    Err(err) => {
                        tracing::warn!(
                            gear = %inst.gear,
                            error = %err,
                            "skipping gear: failed to extract public routes",
                        );
                        None
                    }
                })
                .clone();
            let Some(templates) = templates else {
                continue;
            };
            tracing::info!(
                gear = %inst.gear,
                instance_id = %inst.instance_id,
                endpoint = %inst.endpoint.authority(),
                routes = templates.len(),
                "registering gear proxy routes",
            );
            additions.push(InstanceRoutes {
                gear: inst.gear,
                instance_id: inst.instance_id,
                endpoint: inst.endpoint,
                templates,
            });
        }
        for instance in removals {
            tracing::info!(
                gear = %instance.gear(),
                instance_id = %instance.instance_id(),
                "deregistering stale gear instance",
            );
        }
        self.registry.apply(additions, removals);
        self.prune_reported();
    }
}

/// What [`extract_public_routes`] found in one document.
#[derive(Debug, Default)]
struct PublicRoutes {
    /// The routes to publish.
    routes: Vec<RouteTemplate>,
    /// Exposed operations that need the platform plane, never published.
    platform: Vec<(http::Method, String)>,
}

/// Extract the public route templates from an `OpenAPI` document.
///
/// Walks the document's JSON form (uniformly for all [`OpenApiSpec`] variants)
/// and returns every `(method, path)` whose operation carries
/// `x-toolkit-visibility: exposed`. A document with no public operations yields an
/// empty list (not an error).
///
/// # Errors
/// Returns [`GatewayError::InvalidSpec`] if the document cannot be serialized or
/// parsed to JSON.
fn extract_public_routes(spec: &OpenApiSpec<'_>) -> Result<PublicRoutes, GatewayError> {
    let doc: serde_json::Value = match spec {
        OpenApiSpec::Owned(api) => to_value(api.as_ref())?,
        OpenApiSpec::Borrowed(api) => to_value(api)?,
        OpenApiSpec::SerializedJson(bytes) => {
            serde_json::from_slice(bytes.as_ref()).map_err(|err| GatewayError::InvalidSpec {
                reason: err.to_string(),
            })?
        }
    };

    let mut found = PublicRoutes::default();
    // Document-level `security` is the default applied to any operation that
    // does not declare its own (OpenAPI 3.x semantics). `None` means the
    // document declared no default; `Some(bool)` carries whether that default
    // requires authentication (a non-empty requirement).
    let doc_authenticated = security_authenticated(doc.get("security"));
    let Some(paths) = doc.get("paths").and_then(serde_json::Value::as_object) else {
        return Ok(found);
    };

    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for key in HTTP_METHOD_KEYS {
            let Some(operation) = item.get(key).and_then(serde_json::Value::as_object) else {
                continue;
            };
            let exposed = operation
                .get(API_VISIBILITY_EXTENSION)
                .and_then(serde_json::Value::as_str)
                == Some(API_VISIBILITY_EXPOSED);
            if !exposed {
                continue;
            }
            let method =
                http::Method::from_bytes(key.to_uppercase().as_bytes()).map_err(|err| {
                    GatewayError::InvalidSpec {
                        reason: format!("invalid HTTP method '{key}': {err}"),
                    }
                })?;
            // Authenticated iff the effective `security` requirement is
            // non-empty. The operation's own `security` (when present) wins,
            // including an explicit empty array `[]` that overrides the
            // document default to anonymous; otherwise the document-level
            // default applies, and a route with neither is anonymous.
            let security = operation.get("security").or(doc.get("security"));
            // Never proxy platform operations: the edge strips their required token.
            // The caller reports each rejected route once per gear.
            if requires_internal_token(security) {
                found.platform.push((method, path.clone()));
                continue;
            }
            let authenticated = security_authenticated(operation.get("security"))
                .or(doc_authenticated)
                .unwrap_or(false);
            found
                .routes
                .push(RouteTemplate::new(method, path.clone(), authenticated));
        }
    }

    Ok(found)
}

/// Interpret a `security` field (from a document root or an operation object).
///
/// Returns `None` when no `security` array is present, and `Some(authenticated)`
/// otherwise — `true` for a non-empty requirement, `false` for an explicit
/// empty array (anonymous). Callers combine the operation-level result with the
/// document-level default via `Option::or`.
fn security_authenticated(security: Option<&serde_json::Value>) -> Option<bool> {
    security
        .and_then(serde_json::Value::as_array)
        .map(|reqs| !reqs.is_empty())
}

/// True when every security alternative (OR) requires the internal-token scheme (AND).
fn requires_internal_token(security: Option<&serde_json::Value>) -> bool {
    security
        .and_then(serde_json::Value::as_array)
        .is_some_and(|alternatives| {
            !alternatives.is_empty()
                && alternatives.iter().all(|requirement| {
                    requirement
                        .as_object()
                        .is_some_and(|schemes| schemes.contains_key(INTERNAL_TOKEN_SECURITY_SCHEME))
                })
        })
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

    use super::{InstanceSpec, ToolKitGatewayProvider, extract_public_routes};
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
                        "x-toolkit-visibility": "exposed",
                        "security": [{ "bearerAuth": [] }],
                        "responses": {}
                    },
                    "post": { "x-toolkit-visibility": "exposed", "responses": {} }
                },
                "/calc/v1/internal": {
                    "get": { "security": [{ "bearerAuth": [] }], "responses": {} }
                }
            }
        });
        Bytes::from(serde_json::to_vec(&doc).expect("serialize sample"))
    }

    /// One exposed GET per path; absent operation security inherits the document default.
    fn security_cases_spec(
        doc_security: &serde_json::Value,
        cases: &[(&str, Option<serde_json::Value>)],
    ) -> Bytes {
        let mut paths = serde_json::Map::new();
        for (path, security) in cases {
            let mut op = serde_json::json!({ "x-toolkit-visibility": "exposed", "responses": {} });
            if let Some(security) = security {
                op["security"] = security.clone();
            }
            paths.insert((*path).to_owned(), serde_json::json!({ "get": op }));
        }
        let doc = serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "t", "version": "1.0.0" },
            "security": doc_security.clone(),
            "paths": paths,
        });
        Bytes::from(serde_json::to_vec(&doc).expect("serialize"))
    }

    #[test]
    fn platform_operations_are_never_published() {
        use serde_json::json;
        let spec = OpenApiSpec::SerializedJson(security_cases_spec(
            &json!([{ "internalToken": [] }]),
            &[
                ("/token", Some(json!([{ "internalToken": [] }]))),
                (
                    "/token-and-bearer",
                    Some(json!([{ "internalToken": [], "bearerAuth": [] }])),
                ),
                (
                    "/token-either-way",
                    Some(
                        json!([{ "internalToken": [] }, { "internalToken": [], "bearerAuth": [] }]),
                    ),
                ),
                ("/inherited", None),
                (
                    "/token-or-bearer",
                    Some(json!([{ "internalToken": [] }, { "bearerAuth": [] }])),
                ),
                ("/bearer", Some(json!([{ "bearerAuth": [] }]))),
                ("/anonymous", Some(json!([]))),
            ],
        ));
        let found = extract_public_routes(&spec).expect("parse spec");
        let mut skipped: Vec<&str> = found.platform.iter().map(|(_, p)| p.as_str()).collect();
        skipped.sort_unstable();
        assert_eq!(
            skipped,
            [
                "/inherited",
                "/token",
                "/token-and-bearer",
                "/token-either-way"
            ],
            "every unpublished platform operation is handed back to be reported"
        );
        let routes = found.routes;
        let published = |path: &str| {
            routes
                .iter()
                .find(|r| r.path == path)
                .map(|r| r.authenticated)
        };

        for path in [
            "/token",
            "/token-and-bearer",
            "/token-either-way",
            "/inherited",
        ] {
            assert_eq!(
                published(path),
                None,
                "{path} needs the internal token: not published"
            );
        }
        assert_eq!(
            published("/token-or-bearer"),
            Some(true),
            "a bearer-only alternative exists"
        );
        assert_eq!(published("/bearer"), Some(true));
        assert_eq!(published("/anonymous"), Some(false));
    }

    #[test]
    fn extract_only_public_routes() {
        let spec = OpenApiSpec::SerializedJson(sample_spec_json());
        let mut routes = extract_public_routes(&spec).expect("parse spec").routes;
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
    fn operation_inherits_document_level_security() {
        // Document-level `security` is the default. The GET declares none, so it
        // inherits (authenticated). The POST overrides with an explicit empty
        // array -> anonymous. The DELETE declares its own non-empty requirement.
        let doc = serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "calc", "version": "1.0.0" },
            "security": [{ "bearerAuth": [] }],
            "paths": {
                "/calc/v1/a": {
                    "get": { "x-toolkit-visibility": "exposed", "responses": {} },
                    "post": {
                        "x-toolkit-visibility": "exposed",
                        "security": [],
                        "responses": {}
                    },
                    "delete": {
                        "x-toolkit-visibility": "exposed",
                        "security": [{ "bearerAuth": [] }],
                        "responses": {}
                    }
                }
            }
        });
        let spec =
            OpenApiSpec::SerializedJson(Bytes::from(serde_json::to_vec(&doc).expect("serialize")));
        let routes = extract_public_routes(&spec).expect("parse").routes;

        let auth = |m: http::Method| {
            routes
                .iter()
                .find(|r| r.method == m)
                .expect("route present")
                .authenticated
        };
        assert!(
            auth(http::Method::GET),
            "GET inherits document-level security -> authenticated"
        );
        assert!(
            !auth(http::Method::POST),
            "POST with explicit empty security overrides to anonymous"
        );
        assert!(
            auth(http::Method::DELETE),
            "DELETE with own non-empty security -> authenticated"
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
        assert!(
            extract_public_routes(&spec)
                .expect("parse")
                .routes
                .is_empty()
        );
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
                "calc-1",
                OpenApiSpec::SerializedJson(sample_spec_json()),
                &endpoint,
            )
            .await
            .expect("register");

        assert!(registry.match_path("/calc/v1/items/7").is_some());
        assert!(registry.match_path("/calc/v1/internal").is_none());

        provider
            .deregister_routes(&gear, "calc-1")
            .await
            .expect("deregister");
        assert!(registry.match_path("/calc/v1/items/7").is_none());
    }

    #[tokio::test]
    async fn apply_snapshot_registers_all_co_located_instances_of_a_gear() {
        // Two instances of the same gear share an identical spec. Both must be
        // registered even though the spec is parsed only once (per-gear cache),
        // and the resulting routes must be present for the gear.
        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        let gear = GearName::from("calc");

        let instance = |id: &str, authority: &str| InstanceSpec {
            gear: gear.clone(),
            instance_id: id.to_owned(),
            spec: OpenApiSpec::SerializedJson(sample_spec_json()),
            endpoint: Endpoint::parse(authority).expect("endpoint"),
        };

        provider
            .apply_snapshot(
                vec![
                    instance("calc-a", "http://calc-a:8080"),
                    instance("calc-b", "http://calc-b:8080"),
                ],
                &[],
            )
            .await;

        assert_eq!(registry.instance_count(), 2);
        assert!(registry.contains_instance(&gear, "calc-a"));
        assert!(registry.contains_instance(&gear, "calc-b"));
        assert!(registry.match_path("/calc/v1/items/7").is_some());
    }

    #[test]
    #[tracing_test::traced_test]
    fn a_platform_route_is_reported_once_per_gear_however_often_it_is_seen() {
        let provider = ToolKitGatewayProvider::new(Arc::new(ProxyRegistry::new()));
        let gear = GearName::new("calc");
        let platform = [(http::Method::GET, "/token".to_owned())];

        provider.report_platform(&gear, "calc-a", &platform);
        provider.report_platform(&gear, "calc-b", &platform);
        provider.report_platform(&GearName::new("other"), "other-a", &platform);

        logs_assert(|lines: &[&str]| {
            let warned: Vec<&&str> = lines
                .iter()
                .filter(|line| line.contains("not published as a proxied route"))
                .collect();
            if warned.len() != 2 {
                return Err(format!("expected two warnings, got {warned:?}"));
            }
            let has = |gear: &str, instance: &str| {
                warned.iter().any(|line| {
                    line.contains(&format!("gear={gear}"))
                        && line.contains(&format!("instance_id=\"{instance}\""))
                        && line.contains("method=GET")
                        && line.contains("path=/token")
                })
            };
            if has("calc", "calc-a") && has("other", "other-a") {
                Ok(())
            } else {
                Err(format!("unexpected warnings: {warned:?}"))
            }
        });
    }

    #[tokio::test]
    async fn platform_warnings_follow_the_gears_current_routes_and_instances() {
        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        let gear = GearName::new("calc");
        let endpoint = Endpoint::parse("http://calc:8080").unwrap();
        let reported = |provider: &ToolKitGatewayProvider| {
            provider.reported_platform.lock().get(&gear).map(|routes| {
                routes
                    .iter()
                    .map(|(_, path)| path.clone())
                    .collect::<Vec<_>>()
            })
        };

        provider.report_platform(&gear, "calc-a", &[(http::Method::GET, "/old".to_owned())]);
        provider.report_platform(&gear, "calc-a", &[(http::Method::GET, "/new".to_owned())]);
        assert_eq!(
            reported(&provider),
            Some(vec!["/new".to_owned()]),
            "a retired route is forgotten"
        );

        for instance in ["calc-a", "calc-b"] {
            registry.register(gear.clone(), instance, endpoint.clone(), vec![]);
        }
        provider.deregister_routes(&gear, "calc-a").await.unwrap();
        assert!(reported(&provider).is_some(), "another instance remains");
        provider.deregister_routes(&gear, "calc-b").await.unwrap();
        assert_eq!(reported(&provider), None, "the last instance is gone");
    }

    #[test]
    fn registry_accessor_returns_shared_table() {
        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        // The accessor hands back the same shared registry (used to build a Forwarder).
        assert!(Arc::ptr_eq(provider.registry(), &registry));
    }

    #[test]
    fn extract_public_routes_from_owned_and_borrowed_specs() {
        // The typed (utoipa) document variants are serialized through the same
        // JSON walk as the byte variant. An empty document yields no routes but
        // still exercises the Owned/Borrowed serialization arms.
        let doc = utoipa::openapi::OpenApiBuilder::new().build();
        let borrowed = OpenApiSpec::Borrowed(&doc);
        assert!(
            extract_public_routes(&borrowed)
                .expect("parse borrowed")
                .routes
                .is_empty()
        );

        let owned = OpenApiSpec::Owned(Box::new(utoipa::openapi::OpenApiBuilder::new().build()));
        assert!(
            extract_public_routes(&owned)
                .expect("parse owned")
                .routes
                .is_empty()
        );
    }

    #[test]
    fn extract_public_routes_rejects_invalid_json() {
        let spec = OpenApiSpec::SerializedJson(Bytes::from_static(b"not json"));
        let err = extract_public_routes(&spec).expect_err("invalid json rejected");
        assert!(matches!(
            err,
            crate::error::GatewayError::InvalidSpec { .. }
        ));
    }

    #[test]
    fn extract_public_routes_tolerates_missing_paths_and_non_object_items() {
        // No `paths` object at all -> empty, not an error.
        let no_paths = OpenApiSpec::SerializedJson(Bytes::from_static(
            br#"{"openapi":"3.1.0","info":{"title":"x","version":"1.0.0"}}"#,
        ));
        assert!(
            extract_public_routes(&no_paths)
                .expect("parse")
                .routes
                .is_empty()
        );

        // A path whose value is not an object is skipped rather than erroring.
        let bad_item = OpenApiSpec::SerializedJson(Bytes::from_static(
            br#"{"openapi":"3.1.0","info":{"title":"x","version":"1.0.0"},"paths":{"/x":"nonsense"}}"#,
        ));
        assert!(
            extract_public_routes(&bad_item)
                .expect("parse")
                .routes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn apply_snapshot_skips_unparseable_spec_and_logs_removals() {
        use crate::types::GearInstance;

        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));

        // One good gear and one whose spec cannot be parsed: the bad gear is
        // skipped (its routes are not registered) while the good one lands.
        provider
            .apply_snapshot(
                vec![
                    InstanceSpec {
                        gear: GearName::from("calc"),
                        instance_id: "calc-1".to_owned(),
                        spec: OpenApiSpec::SerializedJson(sample_spec_json()),
                        endpoint: Endpoint::parse("http://calc:8080").expect("endpoint"),
                    },
                    InstanceSpec {
                        gear: GearName::from("broken"),
                        instance_id: "broken-1".to_owned(),
                        spec: OpenApiSpec::SerializedJson(Bytes::from_static(b"not json")),
                        endpoint: Endpoint::parse("http://broken:8080").expect("endpoint"),
                    },
                ],
                &[],
            )
            .await;
        assert!(registry.contains_instance(&GearName::from("calc"), "calc-1"));
        assert!(!registry.contains_gear(&GearName::from("broken")));

        // A subsequent snapshot removes the calc instance (exercises the
        // removals-logging branch); the registry ends empty.
        provider
            .apply_snapshot(
                vec![],
                &[GearInstance::new(GearName::from("calc"), "calc-1")],
            )
            .await;
        assert_eq!(registry.instance_count(), 0);
    }
}

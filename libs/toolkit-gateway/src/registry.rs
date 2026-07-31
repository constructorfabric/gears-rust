//! In-memory reverse-proxy route table.
//!
//! Maps `OpenAPI` path templates to the gear [`Endpoint`] that serves them,
//! using a [`matchit`](matchit) router for path matching. The router is rebuilt
//! on every registration change; lookups take a read lock and never block each
//! other.

use std::collections::{HashMap, HashSet};

use parking_lot::RwLock;

use crate::types::{Endpoint, GearName};

/// A single registered route: an HTTP method plus an `OpenAPI` path template
/// (e.g. `GET /calculator/v1/items/{id}`), and whether it requires
/// authentication at the edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteTemplate {
    /// HTTP method.
    pub method: http::Method,
    /// `OpenAPI` path template, using `{param}` placeholders.
    pub path: String,
    /// Whether the edge must enforce authentication for this route (derived
    /// from the operation's `OpenAPI` `security` requirement). An exposed route
    /// may still be anonymous (`authenticated == false`).
    pub authenticated: bool,
}

impl RouteTemplate {
    /// Creates a route template.
    #[must_use]
    pub fn new(method: http::Method, path: impl Into<String>, authenticated: bool) -> Self {
        Self {
            method,
            path: path.into(),
            authenticated,
        }
    }
}

/// The upstream target a matched request should be forwarded to.
#[derive(Clone, Debug)]
pub struct RouteMatch {
    /// The gear that owns the matched route (for logging / metrics).
    pub gear: GearName,
    /// The upstream endpoint to forward the request to.
    pub endpoint: Endpoint,
}

/// One gear's registration: its endpoint and the templates it exposes.
struct GearEntry {
    endpoint: Endpoint,
    templates: Vec<RouteTemplate>,
}

/// The value stored per path template in the router: the forwarding target plus
/// the per-method authentication requirement.
#[derive(Clone, Debug)]
struct RegisteredPath {
    target: RouteMatch,
    /// Per-method auth requirement for this path. A method absent from the map
    /// defaults to authenticated (the safe choice).
    authenticated: HashMap<http::Method, bool>,
}

/// Mutable state guarded by the registry's lock.
struct State {
    gears: HashMap<GearName, GearEntry>,
    router: matchit::Router<RegisteredPath>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            gears: HashMap::new(),
            router: matchit::Router::new(),
        }
    }
}

impl State {
    /// Rebuild the `matchit` router from the current gear registrations.
    ///
    /// Each distinct path template is inserted once (multiple methods on the
    /// same path share a single matchit entry, since forwarding keys off the
    /// path only). Conflicting templates from different gears are logged and the
    /// first registration wins.
    fn rebuild(&mut self) {
        let mut router = matchit::Router::new();
        let mut seen: HashSet<String> = HashSet::new();

        for (gear, entry) in &self.gears {
            // Collapse the gear's templates by path: one matchit entry per path
            // (forwarding keys off the path only), carrying a per-method auth map.
            let mut by_path: HashMap<&str, HashMap<http::Method, bool>> = HashMap::new();
            for template in &entry.templates {
                by_path
                    .entry(template.path.as_str())
                    .or_default()
                    .insert(template.method.clone(), template.authenticated);
            }

            for (path, authenticated) in by_path {
                if !seen.insert(path.to_owned()) {
                    continue;
                }
                let value = RegisteredPath {
                    target: RouteMatch {
                        gear: gear.clone(),
                        endpoint: entry.endpoint.clone(),
                    },
                    authenticated,
                };
                if let Err(err) = router.insert(path.to_owned(), value) {
                    tracing::warn!(
                        gear = %gear,
                        path = %path,
                        error = %err,
                        "skipping conflicting proxy route template",
                    );
                }
            }
        }

        self.router = router;
    }
}

/// Thread-safe reverse-proxy route table shared between the
/// [`ToolKitGatewayProvider`](crate::ToolKitGatewayProvider) (which mutates it)
/// and the [`Forwarder`](crate::Forwarder) (which reads it).
#[derive(Default)]
pub struct ProxyRegistry {
    inner: RwLock<State>,
}

impl ProxyRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers (or replaces) the routes for `gear`, backed by `endpoint`.
    ///
    /// Registering a gear that is already present atomically replaces its
    /// previous route set.
    pub fn register(&self, gear: GearName, endpoint: Endpoint, templates: Vec<RouteTemplate>) {
        let mut state = self.inner.write();
        state.gears.insert(
            gear,
            GearEntry {
                endpoint,
                templates,
            },
        );
        state.rebuild();
    }

    /// Removes the routes registered for `gear`. Returns `true` if the gear was
    /// registered.
    pub fn deregister(&self, gear: &GearName) -> bool {
        let mut state = self.inner.write();
        let removed = state.gears.remove(gear).is_some();
        if removed {
            state.rebuild();
        }
        removed
    }

    /// Looks up the upstream target for `path`, if any registered template
    /// matches it.
    #[must_use]
    pub fn match_path(&self, path: &str) -> Option<RouteMatch> {
        let state = self.inner.read();
        state
            .router
            .at(path)
            .ok()
            .map(|matched| matched.value.target.clone())
    }

    /// Returns whether a proxied `(method, path)` requires authentication at the
    /// edge, or `None` if `path` is not a proxied route.
    ///
    /// A matched path whose specific `method` was not registered defaults to
    /// `true` (authenticated) — the safe choice.
    #[must_use]
    pub fn requires_auth(&self, method: &http::Method, path: &str) -> Option<bool> {
        let state = self.inner.read();
        let matched = state.router.at(path).ok()?;
        Some(
            matched
                .value
                .authenticated
                .get(method)
                .copied()
                .unwrap_or(true),
        )
    }

    /// Returns `true` if `gear` currently has a registration.
    #[must_use]
    pub fn contains_gear(&self, gear: &GearName) -> bool {
        self.inner.read().gears.contains_key(gear)
    }

    /// Returns the number of registered gears.
    #[must_use]
    pub fn gear_count(&self) -> usize {
        self.inner.read().gears.len()
    }

    /// Returns a snapshot of the gears currently registered.
    ///
    /// Used by directory-sync callers to compute which gears have disappeared
    /// and should be deregistered.
    #[must_use]
    pub fn registered_gears(&self) -> Vec<GearName> {
        self.inner.read().gears.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{ProxyRegistry, RouteTemplate};
    use crate::types::{Endpoint, GearName};

    fn endpoint(uri: &str) -> Endpoint {
        Endpoint::parse(uri).expect("valid endpoint")
    }

    fn templates() -> Vec<RouteTemplate> {
        vec![
            RouteTemplate::new(http::Method::GET, "/calc/v1/items/{id}", true),
            RouteTemplate::new(http::Method::POST, "/calc/v1/items/{id}", true),
            // Anonymous public route (e.g. a health probe).
            RouteTemplate::new(http::Method::GET, "/calc/v1/health", false),
        ]
    }

    #[test]
    fn register_then_match_static_and_param_paths() {
        let registry = ProxyRegistry::new();
        registry.register(
            GearName::from("calculator"),
            endpoint("http://calculator:8080"),
            templates(),
        );

        let matched = registry
            .match_path("/calc/v1/items/42")
            .expect("param match");
        assert_eq!(matched.gear.as_str(), "calculator");
        assert_eq!(matched.endpoint.authority().as_str(), "calculator:8080");

        assert!(registry.match_path("/calc/v1/health").is_some());
        assert!(registry.match_path("/calc/v1/unknown").is_none());
    }

    #[test]
    fn deregister_removes_routes() {
        let registry = ProxyRegistry::new();
        let gear = GearName::from("calculator");
        registry.register(
            gear.clone(),
            endpoint("http://calculator:8080"),
            templates(),
        );
        assert!(registry.contains_gear(&gear));
        assert_eq!(registry.gear_count(), 1);

        assert!(registry.deregister(&gear));
        assert!(!registry.contains_gear(&gear));
        assert!(registry.match_path("/calc/v1/health").is_none());

        // Deregistering an unknown gear is a no-op returning false.
        assert!(!registry.deregister(&gear));
    }

    #[test]
    fn register_replaces_previous_routes() {
        let registry = ProxyRegistry::new();
        let gear = GearName::from("calculator");
        registry.register(
            gear.clone(),
            endpoint("http://old:8080"),
            vec![RouteTemplate::new(http::Method::GET, "/calc/v1/old", true)],
        );
        registry.register(
            gear,
            endpoint("http://new:9090"),
            vec![RouteTemplate::new(http::Method::GET, "/calc/v1/new", true)],
        );

        assert!(registry.match_path("/calc/v1/old").is_none());
        let matched = registry.match_path("/calc/v1/new").expect("new route");
        assert_eq!(matched.endpoint.authority().as_str(), "new:9090");
    }

    #[test]
    fn requires_auth_reflects_per_route_flag() {
        let registry = ProxyRegistry::new();
        registry.register(
            GearName::from("calculator"),
            endpoint("http://calculator:8080"),
            templates(),
        );

        // Authenticated route (matches the param template).
        assert_eq!(
            registry.requires_auth(&http::Method::GET, "/calc/v1/items/42"),
            Some(true)
        );
        // Anonymous public route.
        assert_eq!(
            registry.requires_auth(&http::Method::GET, "/calc/v1/health"),
            Some(false)
        );
        // A method not registered on a known path defaults to authenticated.
        assert_eq!(
            registry.requires_auth(&http::Method::DELETE, "/calc/v1/health"),
            Some(true)
        );
        // Unknown path is not a proxied route.
        assert_eq!(registry.requires_auth(&http::Method::GET, "/nope"), None);
    }
}

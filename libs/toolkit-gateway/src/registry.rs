//! In-memory reverse-proxy route table.
//!
//! Maps `OpenAPI` path templates to the gear [`Endpoint`] that serves them,
//! using a [`matchit`](matchit) router for path matching. The router is rebuilt
//! on every registration change; lookups take a read lock and never block each
//! other.

use std::collections::HashMap;

use parking_lot::RwLock;

use crate::types::{Endpoint, GearInstance, GearName};

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

/// One gear instance's registration: its endpoint and the templates it exposes.
struct GearEntry {
    endpoint: Endpoint,
    templates: Vec<RouteTemplate>,
}

/// A single instance's desired routes, used by [`ProxyRegistry::apply`] to
/// batch many registrations into one router rebuild.
pub struct InstanceRoutes {
    /// Stable gear name.
    pub gear: GearName,
    /// Opaque, unique instance identifier.
    pub instance_id: String,
    /// Upstream endpoint serving this instance's routes.
    pub endpoint: Endpoint,
    /// Public route templates this instance exposes at the edge.
    pub templates: Vec<RouteTemplate>,
}

/// Accumulated auth map and the selected upstream endpoint for one
/// `(gear, path)` while rebuilding the router.
struct PathAggregate {
    /// Per-method auth requirement for this path.
    authenticated: HashMap<http::Method, bool>,
    /// The endpoint selected for this path: the first instance of the owning
    /// gear seen in deterministic order (see [`State::rebuild`]).
    endpoint: Endpoint,
}

/// The value stored per path template in the router: the owning gear, its
/// selected upstream endpoint, and the per-method authentication requirement.
#[derive(Clone, Debug)]
struct RegisteredPath {
    /// The gear that owns the matched path.
    gear: GearName,
    /// The upstream endpoint a matched request forwards to.
    ///
    /// **Profile 3 selection.** A gear resolves to a single, stable endpoint
    /// (the first instance in deterministic order); cross-replica load balancing
    /// is delegated to the gear's k8s Service DNS. The instance-keyed registry
    /// ([`State::instances`]) retains *every* instance, so Profile 2
    /// (Host + Workers) multi-worker selection — where the proxy is the only
    /// load balancer — and future metadata routing can select differently by
    /// changing [`State::rebuild`]/[`ProxyRegistry::match_path`], without a
    /// change to how routes are stored.
    endpoint: Endpoint,
    /// Per-method auth requirement for this path. A method absent from the map
    /// defaults to authenticated (the safe choice).
    authenticated: HashMap<http::Method, bool>,
}

/// Mutable state guarded by the registry's lock.
struct State {
    instances: HashMap<GearInstance, GearEntry>,
    router: matchit::Router<RegisteredPath>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            instances: HashMap::new(),
            router: matchit::Router::new(),
        }
    }
}

impl State {
    /// Rebuild the `matchit` router from the current instance registrations.
    ///
    /// Instances are visited in a stable, deterministic order (sorted by gear
    /// name then instance identifier). Each `(gear, path)` resolves to the first
    /// instance's endpoint in that order (Profile 3 single-endpoint selection).
    /// The first gear (lexicographically) to claim a path wins a duplicate path
    /// across gears, and a conflicting insertion is logged.
    fn rebuild(&mut self) {
        let mut router = matchit::Router::new();

        // Selected endpoint and auth map per (gear, path).
        let mut path_map: HashMap<(GearName, String), PathAggregate> = HashMap::new();

        // Deterministic instance ordering.
        let mut instances: Vec<_> = self.instances.iter().collect();
        instances.sort_by(|(a, _), (b, _)| {
            a.gear()
                .as_str()
                .cmp(b.gear().as_str())
                .then(a.instance_id().cmp(b.instance_id()))
        });

        for (instance, entry) in instances {
            // Collapse this instance's templates by path.
            let mut by_path: HashMap<&str, HashMap<http::Method, bool>> = HashMap::new();
            for template in &entry.templates {
                by_path
                    .entry(template.path.as_str())
                    .or_default()
                    .insert(template.method.clone(), template.authenticated);
            }

            for (path, authenticated) in by_path {
                let key = (instance.gear().clone(), path.to_owned());
                // Profile 3 selection: keep the first instance's endpoint in
                // deterministic order and ignore later ones. The instance-keyed
                // registry still holds every instance, so Profile 2 multi-worker
                // selection is a future change here (and in `match_path`), not a
                // storage change.
                let aggregate = path_map.entry(key).or_insert_with(|| PathAggregate {
                    authenticated: HashMap::new(),
                    endpoint: entry.endpoint.clone(),
                });
                for (method, is_auth) in authenticated {
                    aggregate.authenticated.entry(method).or_insert(is_auth);
                }
            }
        }

        // Deterministic insertion order: sort by gear name then path.
        let mut path_entries: Vec<_> = path_map.into_iter().collect();
        path_entries.sort_by(|((a_gear, a_path), _), ((b_gear, b_path), _)| {
            a_gear
                .as_str()
                .cmp(b_gear.as_str())
                .then(a_path.cmp(b_path))
        });

        // First-gear-winner for duplicate paths across gears.
        let mut seen: HashMap<String, GearName> = HashMap::new();
        for ((gear, path), aggregate) in path_entries {
            if let Some(winner) = seen.get(&path) {
                if winner.as_str() != gear.as_str() {
                    tracing::warn!(
                        gear = %gear,
                        winning_gear = %winner,
                        path = %path,
                        "ignoring duplicate proxy route path already claimed by another gear",
                    );
                }
                continue;
            }
            seen.insert(path.clone(), gear.clone());
            let value = RegisteredPath {
                gear,
                endpoint: aggregate.endpoint,
                authenticated: aggregate.authenticated,
            };
            if let Err(err) = router.insert(path.clone(), value) {
                tracing::warn!(
                    path = %path,
                    error = %err,
                    "skipping conflicting proxy route template",
                );
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

    /// Registers (or replaces) the routes for `(gear, instance_id)`, backed by
    /// `endpoint`.
    ///
    /// Registering an instance that is already present atomically replaces its
    /// previous route set, leaving other instances of the same gear untouched.
    pub fn register(
        &self,
        gear: GearName,
        instance_id: &str,
        endpoint: Endpoint,
        templates: Vec<RouteTemplate>,
    ) {
        let mut state = self.inner.write();
        state.instances.insert(
            GearInstance::new(gear, instance_id),
            GearEntry {
                endpoint,
                templates,
            },
        );
        state.rebuild();
    }

    /// Removes the routes registered for `(gear, instance_id)`. Returns `true` if
    /// that instance was registered. Other instances of the same gear are left
    /// intact.
    pub fn deregister(&self, gear: &GearName, instance_id: &str) -> bool {
        let mut state = self.inner.write();
        let removed = state
            .instances
            .remove(&GearInstance::new(gear.clone(), instance_id))
            .is_some();
        if removed {
            state.rebuild();
        }
        removed
    }

    /// Applies a batch of `additions` and `removals` under a single write lock,
    /// rebuilding the router **exactly once**.
    ///
    /// This is the reconciliation entry point for directory-sync callers: one
    /// poll produces at most one rebuild regardless of how many instances
    /// changed, instead of one rebuild per instance.
    ///
    /// - `additions` insert or replace the named instances.
    /// - `removals` drop the named instances.
    /// - Instances mentioned in neither list are left untouched, so a transient
    ///   failure to re-register a still-present instance retains its existing
    ///   routes.
    pub fn apply(&self, additions: Vec<InstanceRoutes>, removals: &[GearInstance]) {
        if additions.is_empty() && removals.is_empty() {
            return;
        }
        let mut state = self.inner.write();
        for add in additions {
            state.instances.insert(
                GearInstance::new(add.gear, &add.instance_id),
                GearEntry {
                    endpoint: add.endpoint,
                    templates: add.templates,
                },
            );
        }
        for instance in removals {
            state.instances.remove(instance);
        }
        state.rebuild();
    }

    /// Looks up the upstream target for `path`, if any registered template
    /// matches it.
    ///
    /// **Profile 3 selection:** a matched path resolves to a single, stable
    /// endpoint (the first instance in deterministic order); cross-replica load
    /// balancing is delegated to the gear's stable k8s Service DNS. The
    /// instance-keyed registry retains every instance, so Profile 2 (Host +
    /// Workers) multi-worker selection is a future change to `rebuild` +
    /// `match_path`, not a storage change.
    #[must_use]
    pub fn match_path(&self, path: &str) -> Option<RouteMatch> {
        let state = self.inner.read();
        let registered = state.router.at(path).ok()?.value;
        Some(RouteMatch {
            gear: registered.gear.clone(),
            endpoint: registered.endpoint.clone(),
        })
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

    /// Returns `true` if any instance of `gear` is currently registered.
    #[must_use]
    pub fn contains_gear(&self, gear: &GearName) -> bool {
        self.inner.read().instances.keys().any(|k| k.gear() == gear)
    }

    /// Returns `true` if the specific `(gear, instance_id)` is registered.
    #[must_use]
    pub fn contains_instance(&self, gear: &GearName, instance_id: &str) -> bool {
        self.inner
            .read()
            .instances
            .contains_key(&GearInstance::new(gear.clone(), instance_id))
    }

    /// Returns the number of registered gear instances.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.inner.read().instances.len()
    }

    /// Returns a snapshot of the gear instances currently registered.
    ///
    /// Used by directory-sync callers to compute which instances have
    /// disappeared and should be deregistered.
    #[must_use]
    pub fn registered_instances(&self) -> Vec<GearInstance> {
        self.inner.read().instances.keys().cloned().collect()
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
            "calc-1",
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
            "calc-1",
            endpoint("http://calculator:8080"),
            templates(),
        );
        assert!(registry.contains_gear(&gear));
        assert_eq!(registry.instance_count(), 1);

        assert!(registry.deregister(&gear, "calc-1"));
        assert!(!registry.contains_gear(&gear));
        assert!(registry.match_path("/calc/v1/health").is_none());

        // Deregistering an unknown instance is a no-op returning false.
        assert!(!registry.deregister(&gear, "calc-1"));
    }

    #[test]
    fn register_replaces_previous_routes() {
        let registry = ProxyRegistry::new();
        let gear = GearName::from("calculator");
        registry.register(
            gear.clone(),
            "calc-1",
            endpoint("http://old:8080"),
            vec![RouteTemplate::new(http::Method::GET, "/calc/v1/old", true)],
        );
        registry.register(
            gear,
            "calc-1",
            endpoint("http://new:9090"),
            vec![RouteTemplate::new(http::Method::GET, "/calc/v1/new", true)],
        );

        assert!(registry.match_path("/calc/v1/old").is_none());
        let matched = registry.match_path("/calc/v1/new").expect("new route");
        assert_eq!(matched.endpoint.authority().as_str(), "new:9090");
    }

    #[test]
    fn duplicate_path_resolves_to_deterministic_winner() {
        // Two gears expose the same path template. The winner is the
        // lexicographically-first gear name and is stable regardless of the
        // order in which the gears were registered.
        for (first, second) in [("alpha", "bravo"), ("bravo", "alpha")] {
            let registry = ProxyRegistry::new();
            registry.register(
                GearName::from(first),
                &format!("{first}-i"),
                endpoint(&format!("http://{first}:8080")),
                vec![RouteTemplate::new(
                    http::Method::GET,
                    "/shared/v1/thing",
                    true,
                )],
            );
            registry.register(
                GearName::from(second),
                &format!("{second}-i"),
                endpoint(&format!("http://{second}:8080")),
                vec![RouteTemplate::new(
                    http::Method::GET,
                    "/shared/v1/thing",
                    true,
                )],
            );

            let matched = registry.match_path("/shared/v1/thing").expect("match");
            assert_eq!(
                matched.gear.as_str(),
                "alpha",
                "registration order ({first}, {second}) must not change the winner"
            );
            assert_eq!(matched.endpoint.authority().as_str(), "alpha:8080");
        }
    }

    #[test]
    fn requires_auth_reflects_per_route_flag() {
        let registry = ProxyRegistry::new();
        registry.register(
            GearName::from("calculator"),
            "calc-1",
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

    #[test]
    fn multiple_instances_same_gear_deregister_one() {
        let registry = ProxyRegistry::new();
        let gear = GearName::from("calculator");
        registry.register(
            gear.clone(),
            "calc-a",
            endpoint("http://calc-a:8080"),
            templates(),
        );
        registry.register(
            gear.clone(),
            "calc-b",
            endpoint("http://calc-b:8080"),
            templates(),
        );

        assert_eq!(registry.instance_count(), 2);
        assert!(registry.contains_instance(&gear, "calc-a"));
        assert!(registry.contains_instance(&gear, "calc-b"));

        // Profile 3 selection: a gear resolves to a single, stable endpoint
        // (the first in deterministic instance order, i.e. the lowest instance
        // id). All instance endpoints are retained so Profile 2 can later
        // balance across them without a storage change.
        let matched = registry
            .match_path("/calc/v1/items/42")
            .expect("gear match");
        assert_eq!(matched.gear.as_str(), "calculator");
        assert_eq!(matched.endpoint.authority().as_str(), "calc-a:8080");

        // Deregister the selected instance: the gear stays routable and falls
        // back to the surviving instance's endpoint.
        assert!(registry.deregister(&gear, "calc-a"));
        assert!(!registry.contains_instance(&gear, "calc-a"));
        assert!(registry.contains_instance(&gear, "calc-b"));
        assert!(registry.contains_gear(&gear));
        assert_eq!(registry.instance_count(), 1);

        let remaining = registry
            .match_path("/calc/v1/items/42")
            .expect("remaining match");
        assert_eq!(remaining.endpoint.authority().as_str(), "calc-b:8080");
    }

    #[test]
    fn apply_batches_additions_and_removals() {
        use super::InstanceRoutes;
        use crate::types::GearInstance;

        let registry = ProxyRegistry::new();
        let calc = GearName::from("calculator");
        let bill = GearName::from("billing");

        // Seed two gears in one apply (additions only, one rebuild).
        registry.apply(
            vec![
                InstanceRoutes {
                    gear: calc.clone(),
                    instance_id: "calc-a".to_owned(),
                    endpoint: endpoint("http://calc-a:8080"),
                    templates: templates(),
                },
                InstanceRoutes {
                    gear: bill.clone(),
                    instance_id: "bill-a".to_owned(),
                    endpoint: endpoint("http://bill-a:9090"),
                    templates: vec![RouteTemplate::new(http::Method::GET, "/bill/v1/pay", true)],
                },
            ],
            &[],
        );
        assert_eq!(registry.instance_count(), 2);
        assert!(registry.match_path("/calc/v1/items/1").is_some());
        assert!(registry.match_path("/bill/v1/pay").is_some());

        // A single apply that simultaneously adds a new calc instance and removes
        // the billing instance.
        registry.apply(
            vec![InstanceRoutes {
                gear: calc.clone(),
                instance_id: "calc-b".to_owned(),
                endpoint: endpoint("http://calc-b:8080"),
                templates: templates(),
            }],
            &[GearInstance::new(bill.clone(), "bill-a")],
        );
        assert!(registry.contains_instance(&calc, "calc-a"));
        assert!(registry.contains_instance(&calc, "calc-b"));
        assert!(!registry.contains_gear(&bill));
        assert!(registry.match_path("/bill/v1/pay").is_none());
        assert_eq!(registry.instance_count(), 2);

        // An empty apply is a no-op: instances mentioned in neither list are
        // retained (a transient poll that resolves nothing must not drop routes).
        registry.apply(vec![], &[]);
        assert_eq!(registry.instance_count(), 2);
        assert!(registry.contains_instance(&calc, "calc-a"));
        assert!(registry.match_path("/calc/v1/items/1").is_some());
    }

    #[test]
    fn reregister_updates_auth_flag() {
        let registry = ProxyRegistry::new();
        let gear = GearName::from("calculator");
        registry.register(
            gear.clone(),
            "calc-1",
            endpoint("http://calc:8080"),
            vec![RouteTemplate::new(http::Method::GET, "/calc/v1/ping", true)],
        );
        assert_eq!(
            registry.requires_auth(&http::Method::GET, "/calc/v1/ping"),
            Some(true)
        );

        // The same instance re-registers the route as anonymous; the flag must
        // follow the latest registration, not stick at the previous value.
        registry.register(
            gear,
            "calc-1",
            endpoint("http://calc:8080"),
            vec![RouteTemplate::new(
                http::Method::GET,
                "/calc/v1/ping",
                false,
            )],
        );
        assert_eq!(
            registry.requires_auth(&http::Method::GET, "/calc/v1/ping"),
            Some(false)
        );
    }

    #[test]
    fn registered_instances_snapshots_current_keys() {
        let registry = ProxyRegistry::new();
        assert!(registry.registered_instances().is_empty());

        registry.register(
            GearName::from("calculator"),
            "calc-1",
            endpoint("http://calc:8080"),
            templates(),
        );
        let instances = registry.registered_instances();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].gear().as_str(), "calculator");
        assert_eq!(instances[0].instance_id(), "calc-1");
    }

    #[test]
    fn conflicting_matchit_templates_are_skipped() {
        // Two templates that differ only by their parameter name at the same
        // position conflict in `matchit`; the second insertion is logged and
        // skipped rather than panicking, and the first remains routable.
        let registry = ProxyRegistry::new();
        registry.register(
            GearName::from("calc"),
            "calc-1",
            endpoint("http://calc:8080"),
            vec![
                RouteTemplate::new(http::Method::GET, "/calc/v1/{x}", true),
                RouteTemplate::new(http::Method::GET, "/calc/v1/{y}", true),
            ],
        );
        // Exactly one of the conflicting templates wins; the path is routable.
        assert!(registry.match_path("/calc/v1/anything").is_some());
    }
}

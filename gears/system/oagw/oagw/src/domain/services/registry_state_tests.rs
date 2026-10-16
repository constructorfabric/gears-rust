//! The content hash, and the gate on every registry-state store (memory,
//! `SQLite`, and `PostgreSQL` and `MySQL` behind the `integration` feature):
//! generation, hash and lease decide who reconciles. The registry writes go
//! to an in-memory Control Plane; only the state row is on the backend.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use toolkit_db::DBProvider;
use uuid::Uuid;

use super::*;
use crate::domain::model::{
    BudgetConfig, BudgetMode, CorsHttpMethod, HeadersConfig, HttpMethod, PassthroughMode,
    PathSuffixMode, RateLimitAlgorithm, RateLimitScope, RateLimitStrategy, RequestHeaderRules,
    Route, Scheme, SharingMode, SustainedRate, Upstream, Window,
};
use crate::domain::repo::{DeleteOutcome, RegistryRowRepository, RegistryState, RepositoryError};
use crate::domain::services::registry_reconcile::tests::{
    in_memory_planes, registry_instances, upstream_at,
};
use crate::domain::services::{ControlPlaneServiceImpl, RegistryProvisionerImpl};
use crate::domain::test_support::ProvisioningMetrics;
use crate::infra::storage::registry_state_in_memory::InMemoryRegistryStateStore;
use crate::infra::storage::{DbRegistryStateStore, testing};

// ---------------------------------------------------------------------------
// Content hash
// ---------------------------------------------------------------------------

fn rate_limit(ratio: f64) -> RateLimitConfig {
    RateLimitConfig {
        sharing: crate::domain::model::SharingMode::Private,
        algorithm: RateLimitAlgorithm::default(),
        sustained: SustainedRate {
            rate: 10,
            window: Window::Second,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::default(),
            total: Some(100),
            overcommit_ratio: Some(ratio),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::default(),
        cost: 1,
        response_headers: false,
        pool_owner_id: None,
    }
}

fn headers(pairs: &[(&str, &str)]) -> HeadersConfig {
    HeadersConfig {
        request: Some(RequestHeaderRules {
            set: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            add: HashMap::new(),
            remove: vec![],
            passthrough: Default::default(),
            passthrough_allowlist: vec![],
        }),
        response: None,
    }
}

/// Listing order and `HashMap` order do not change the hash; content,
/// tenant and ID do; `pool_owner_id` is not content.
///
/// ADR-0018: PERS-GATE-013.
#[test]
fn the_content_hash_is_canonical() {
    let root = Uuid::new_v4();
    let (mut upstreams, routes) = registry_instances(root);
    upstreams.push(upstream_at(root, "other.example.com"));
    upstreams[0].request.headers = Some(headers(&[("a", "1"), ("b", "2"), ("c", "3")]));
    upstreams[0].request.rate_limit = Some(rate_limit(1.5));
    let base = content_hash(&upstreams, &routes, root);
    assert_eq!(base.len(), 64);
    assert!(base.bytes().all(|b| b.is_ascii_hexdigit()));

    let mut reordered = upstreams.clone();
    reordered.reverse();
    reordered[1].request.headers = Some(headers(&[("c", "3"), ("a", "1"), ("b", "2")]));
    assert_eq!(content_hash(&reordered, &routes, root), base);

    let mut owned = upstreams.clone();
    owned[0].request.rate_limit.as_mut().unwrap().pool_owner_id = Some(Uuid::new_v4());
    assert_eq!(content_hash(&owned, &routes, root), base);

    // The root tenant fills in instances that name none, as the writes do.
    let mut named = upstreams.clone();
    named[0].tenant_id = None;
    assert_eq!(content_hash(&named, &routes, root), base);
    let mut unnamed = routes.clone();
    unnamed[0].tenant_id = None;
    assert_eq!(content_hash(&upstreams, &unnamed, root), base);
    // ...so another root tenant changes them.
    let other_root = Uuid::new_v4();
    assert_ne!(content_hash(&named, &routes, other_root), base, "upstream");
    assert_ne!(
        content_hash(&upstreams, &unnamed, other_root),
        base,
        "route"
    );

    type Change = Box<dyn Fn(&mut Vec<ProvisionedUpstream>, &mut Vec<ProvisionedRoute>)>;
    let changes: Vec<Change> = vec![
        Box::new(|u, _| u[0].request.tags = vec!["t".into()]),
        Box::new(|u, _| u[0].request.rate_limit = Some(rate_limit(1.5000001))),
        Box::new(|u, _| {
            u[0].request.headers = Some(headers(&[("a", "1"), ("b", "2"), ("c", "4")]))
        }),
        Box::new(|u, _| {
            u[0].request.headers = Some(headers(&[("a", "1b"), ("", "2"), ("c", "3")]))
        }),
        Box::new(|u, _| u[1].tenant_id = Some(Uuid::new_v4())),
        Box::new(|u, _| u[1].request.id = Some(Uuid::new_v4())),
        Box::new(|u, _| u[1].request.enabled = false),
        Box::new(|_, r| r[0].tenant_id = Some(Uuid::new_v4())),
        Box::new(|_, r| r[0].request.id = Some(Uuid::new_v4())),
        Box::new(|_, r| r[0].request.priority = 1),
        Box::new(|_, r| {
            r[0].request.match_rules.http.as_mut().unwrap().path = "/v2".into();
        }),
        Box::new(|_, r| r.clear()),
    ];
    for (i, change) in changes.iter().enumerate() {
        let (mut u, mut r) = (upstreams.clone(), routes.clone());
        change(&mut u, &mut r);
        assert_ne!(content_hash(&u, &r, root), base, "change {i}");
    }
}

/// A map whose entries are inserted in the given order, or reversed.
fn map_of(pairs: &[(&str, &str)], reversed: bool) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut insert = |(k, v): &(&str, &str)| {
        map.insert((*k).to_owned(), (*v).to_owned());
    };
    if reversed {
        pairs.iter().rev().for_each(&mut insert);
    } else {
        pairs.iter().for_each(&mut insert);
    }
    map
}

const PAIRS: &[(&str, &str)] = &[("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")];

fn full_plugins(reversed: bool) -> PluginsConfig {
    PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![
            PluginBinding {
                plugin_ref: "gts.x.core.oagw.plugin.v1~x.one.v1".into(),
                config: map_of(PAIRS, reversed),
            },
            PluginBinding {
                plugin_ref: "gts.x.core.oagw.plugin.v1~x.two.v1".into(),
                config: HashMap::new(),
            },
        ],
    }
}

fn full_rate_limit() -> RateLimitConfig {
    RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 10,
            window: Window::Minute,
        },
        burst: Some(BurstConfig { capacity: 20 }),
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(100),
            overcommit_ratio: Some(1.5),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        pool_owner_id: None,
    }
}

fn full_cors() -> CorsConfig {
    CorsConfig {
        sharing: SharingMode::Inherit,
        enabled: true,
        allowed_origins: vec!["https://a.example.com".into()],
        allowed_methods: vec![CorsHttpMethod::Get, CorsHttpMethod::Post],
        expose_headers: vec!["x-one".into()],
        allow_credentials: false,
    }
}

/// One upstream and two routes (HTTP and gRPC) with every optional part set
/// and every map filled; `reversed` inserts each map's entries in reverse.
fn full_instances(root: Uuid, reversed: bool) -> (Vec<ProvisionedUpstream>, Vec<ProvisionedRoute>) {
    let upstream = ProvisionedUpstream {
        tenant_id: Some(root),
        request: CreateUpstreamRequest {
            id: Some(Uuid::from_u128(1)),
            server: Server {
                endpoints: vec![
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "a.example.com".into(),
                        port: 443,
                    },
                    Endpoint {
                        scheme: Scheme::Https,
                        host: "b.example.com".into(),
                        port: 443,
                    },
                ],
            },
            protocol: oagw_sdk::HTTP_PROTOCOL_ID.into(),
            alias: Some("full".into()),
            auth: Some(AuthConfig {
                plugin_type: "gts.x.core.oagw.auth_plugin.v1~x.apikey.v1".into(),
                sharing: SharingMode::Inherit,
                config: Some(map_of(PAIRS, reversed)),
            }),
            headers: Some(HeadersConfig {
                request: Some(RequestHeaderRules {
                    set: map_of(PAIRS, reversed),
                    add: map_of(PAIRS, !reversed),
                    remove: vec!["x-drop".into()],
                    passthrough: PassthroughMode::Allowlist,
                    passthrough_allowlist: vec!["x-keep".into()],
                }),
                response: Some(ResponseHeaderRules {
                    set: map_of(PAIRS, reversed),
                    add: map_of(PAIRS, !reversed),
                    remove: vec!["x-drop".into()],
                }),
            }),
            plugins: Some(full_plugins(reversed)),
            rate_limit: Some(full_rate_limit()),
            cors: Some(full_cors()),
            tags: vec!["t1".into(), "t2".into()],
            enabled: true,
        },
    };
    let route = |id: u128, match_rules: MatchRules| ProvisionedRoute {
        tenant_id: Some(root),
        request: CreateRouteRequest {
            id: Some(Uuid::from_u128(id)),
            upstream_id: Uuid::from_u128(1),
            match_rules,
            plugins: Some(full_plugins(reversed)),
            rate_limit: Some(full_rate_limit()),
            cors: Some(full_cors()),
            tags: vec!["t1".into()],
            priority: 5,
            enabled: true,
        },
    };
    let http = route(
        2,
        MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Get, HttpMethod::Post],
                path: "/v1".into(),
                query_allowlist: vec!["q".into()],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
    );
    let grpc = route(
        3,
        MatchRules {
            http: None,
            grpc: Some(GrpcMatch {
                service: "x.v1.Svc".into(),
                method: "Call".into(),
            }),
        },
    );
    (vec![upstream], vec![http, grpc])
}

type Mutation<T> = (&'static str, fn(&mut T));

/// A named change to the instances, applied to a fresh copy of them.
type Case = (
    String,
    Box<dyn Fn(&mut [ProvisionedUpstream], &mut [ProvisionedRoute])>,
);

fn plugins_mutations() -> Vec<Mutation<PluginsConfig>> {
    vec![
        ("sharing", |p| p.sharing = SharingMode::Enforce),
        ("items.len", |p| {
            p.items.pop();
        }),
        ("items.order", |p| p.items.reverse()),
        ("items.plugin_ref", |p| p.items[0].plugin_ref.push('x')),
        ("items.config", |p| {
            p.items[0].config.insert("a".into(), "9".into());
        }),
    ]
}

fn rate_limit_mutations() -> Vec<Mutation<RateLimitConfig>> {
    vec![
        ("sharing", |r| r.sharing = SharingMode::Enforce),
        ("algorithm", |r| {
            r.algorithm = RateLimitAlgorithm::SlidingWindow;
        }),
        ("sustained.rate", |r| r.sustained.rate = 11),
        ("sustained.window", |r| r.sustained.window = Window::Hour),
        ("burst none", |r| r.burst = None),
        ("burst.capacity", |r| {
            r.burst = Some(BurstConfig { capacity: 21 })
        }),
        ("budget none", |r| r.budget = None),
        ("budget.mode", |r| {
            r.budget.as_mut().unwrap().mode = BudgetMode::Shared;
        }),
        ("budget.total", |r| {
            r.budget.as_mut().unwrap().total = Some(101)
        }),
        ("budget.total none", |r| {
            r.budget.as_mut().unwrap().total = None
        }),
        ("budget.overcommit_ratio", |r| {
            r.budget.as_mut().unwrap().overcommit_ratio = Some(1.500_000_1);
        }),
        ("budget.overcommit_ratio none", |r| {
            r.budget.as_mut().unwrap().overcommit_ratio = None;
        }),
        ("scope", |r| r.scope = RateLimitScope::User),
        ("strategy", |r| r.strategy = RateLimitStrategy::Queue),
        ("cost", |r| r.cost = 2),
        ("response_headers", |r| r.response_headers = false),
    ]
}

fn cors_mutations() -> Vec<Mutation<CorsConfig>> {
    vec![
        ("sharing", |c| c.sharing = SharingMode::Enforce),
        ("enabled", |c| c.enabled = false),
        ("allowed_origins", |c| c.allowed_origins.clear()),
        ("allowed_methods", |c| {
            c.allowed_methods = vec![CorsHttpMethod::Get, CorsHttpMethod::Put];
        }),
        ("expose_headers", |c| c.expose_headers.push("x-two".into())),
        ("allow_credentials", |c| c.allow_credentials = true),
    ]
}

fn upstream_mutations() -> Vec<Mutation<CreateUpstreamRequest>> {
    vec![
        ("id", |u| u.id = Some(Uuid::from_u128(9))),
        ("endpoints.len", |u| {
            u.server.endpoints.pop();
        }),
        ("endpoints.order", |u| u.server.endpoints.reverse()),
        ("endpoints.scheme", |u| {
            u.server.endpoints[0].scheme = Scheme::Wss
        }),
        ("endpoints.host", |u| u.server.endpoints[0].host.push('x')),
        ("endpoints.port", |u| u.server.endpoints[0].port = 8443),
        ("protocol", |u| u.protocol.push('x')),
        ("alias", |u| u.alias = Some("other".into())),
        ("alias none", |u| u.alias = None),
        ("auth none", |u| u.auth = None),
        ("auth.plugin_type", |u| {
            u.auth.as_mut().unwrap().plugin_type.push('x');
        }),
        ("auth.sharing", |u| {
            u.auth.as_mut().unwrap().sharing = SharingMode::Private;
        }),
        ("auth.config none", |u| {
            u.auth.as_mut().unwrap().config = None
        }),
        ("auth.config", |u| {
            let config = u.auth.as_mut().unwrap().config.as_mut().unwrap();
            config.insert("e".into(), "5".into());
        }),
        ("headers none", |u| u.headers = None),
        ("headers.request none", |u| {
            u.headers.as_mut().unwrap().request = None;
        }),
        ("headers.request.set", |u| {
            let r = u.headers.as_mut().unwrap().request.as_mut().unwrap();
            r.set.insert("a".into(), "9".into());
        }),
        ("headers.request.add", |u| {
            let r = u.headers.as_mut().unwrap().request.as_mut().unwrap();
            r.add.remove("a");
        }),
        ("headers.request.remove", |u| {
            let r = u.headers.as_mut().unwrap().request.as_mut().unwrap();
            r.remove.clear();
        }),
        ("headers.request.passthrough", |u| {
            let r = u.headers.as_mut().unwrap().request.as_mut().unwrap();
            r.passthrough = PassthroughMode::All;
        }),
        ("headers.request.passthrough_allowlist", |u| {
            let r = u.headers.as_mut().unwrap().request.as_mut().unwrap();
            r.passthrough_allowlist.push("x-two".into());
        }),
        ("headers.response none", |u| {
            u.headers.as_mut().unwrap().response = None;
        }),
        ("headers.response.set", |u| {
            let r = u.headers.as_mut().unwrap().response.as_mut().unwrap();
            r.set.insert("b".into(), "9".into());
        }),
        ("headers.response.add", |u| {
            let r = u.headers.as_mut().unwrap().response.as_mut().unwrap();
            r.add.insert("e".into(), "5".into());
        }),
        ("headers.response.remove", |u| {
            let r = u.headers.as_mut().unwrap().response.as_mut().unwrap();
            r.remove.push("x-two".into());
        }),
        ("plugins none", |u| u.plugins = None),
        ("rate_limit none", |u| u.rate_limit = None),
        ("cors none", |u| u.cors = None),
        ("tags", |u| u.tags.reverse()),
        ("enabled", |u| u.enabled = false),
    ]
}

fn route_mutations() -> Vec<Mutation<CreateRouteRequest>> {
    vec![
        ("id", |r| r.id = Some(Uuid::from_u128(9))),
        ("upstream_id", |r| r.upstream_id = Uuid::from_u128(9)),
        ("match.http none", |r| r.match_rules.http = None),
        ("match.http.methods", |r| {
            r.match_rules.http.as_mut().unwrap().methods.pop();
        }),
        ("match.http.path", |r| {
            r.match_rules.http.as_mut().unwrap().path = "/v2".into();
        }),
        ("match.http.query_allowlist", |r| {
            r.match_rules.http.as_mut().unwrap().query_allowlist.clear();
        }),
        ("match.http.path_suffix_mode", |r| {
            r.match_rules.http.as_mut().unwrap().path_suffix_mode = PathSuffixMode::Disabled;
        }),
        ("match.grpc some", |r| {
            r.match_rules.grpc = Some(GrpcMatch {
                service: "x.v1.Svc".into(),
                method: "Call".into(),
            });
        }),
        ("plugins none", |r| r.plugins = None),
        ("rate_limit none", |r| r.rate_limit = None),
        ("cors none", |r| r.cors = None),
        ("tags", |r| r.tags.push("t2".into())),
        ("priority", |r| r.priority = 6),
        ("enabled", |r| r.enabled = false),
    ]
}

fn grpc_route_mutations() -> Vec<Mutation<CreateRouteRequest>> {
    vec![
        ("match.grpc.service", |r| {
            r.match_rules.grpc.as_mut().unwrap().service.push('x');
        }),
        ("match.grpc.method", |r| {
            r.match_rules.grpc.as_mut().unwrap().method.push('x');
        }),
    ]
}

/// Every hashed field changes the hash, in both requests and in every
/// nested config; equal content with its maps filled in another order
/// hashes the same.
///
/// ADR-0018: PERS-GATE-013.
#[test]
fn every_hashed_field_changes_the_content_hash() {
    let root = Uuid::new_v4();
    let (upstreams, routes) = full_instances(root, false);
    let base = content_hash(&upstreams, &routes, root);

    let (u, r) = full_instances(root, true);
    assert_eq!(content_hash(&u, &r, root), base, "map order");

    let mut cases: Vec<Case> = Vec::new();
    for (name, f) in upstream_mutations() {
        cases.push((
            format!("upstream.{name}"),
            Box::new(move |u, _| f(&mut u[0].request)),
        ));
    }
    for (name, f) in route_mutations() {
        cases.push((
            format!("route.{name}"),
            Box::new(move |_, r| f(&mut r[0].request)),
        ));
    }
    for (name, f) in grpc_route_mutations() {
        cases.push((
            format!("route.{name}"),
            Box::new(move |_, r| f(&mut r[1].request)),
        ));
    }
    for (name, f) in plugins_mutations() {
        cases.push((
            format!("upstream.plugins.{name}"),
            Box::new(move |u, _| f(u[0].request.plugins.as_mut().unwrap())),
        ));
        cases.push((
            format!("route.plugins.{name}"),
            Box::new(move |_, r| f(r[0].request.plugins.as_mut().unwrap())),
        ));
    }
    for (name, f) in rate_limit_mutations() {
        cases.push((
            format!("upstream.rate_limit.{name}"),
            Box::new(move |u, _| f(u[0].request.rate_limit.as_mut().unwrap())),
        ));
        cases.push((
            format!("route.rate_limit.{name}"),
            Box::new(move |_, r| f(r[0].request.rate_limit.as_mut().unwrap())),
        ));
    }
    for (name, f) in cors_mutations() {
        cases.push((
            format!("upstream.cors.{name}"),
            Box::new(move |u, _| f(u[0].request.cors.as_mut().unwrap())),
        ));
        cases.push((
            format!("route.cors.{name}"),
            Box::new(move |_, r| f(r[0].request.cors.as_mut().unwrap())),
        ));
    }

    let mut seen = HashMap::from([(base.clone(), "base".to_owned())]);
    for (name, change) in &cases {
        let (mut u, mut r) = (upstreams.clone(), routes.clone());
        change(&mut u, &mut r);
        let hash = content_hash(&u, &r, root);
        assert_ne!(hash, base, "{name} left the hash unchanged");
        // Each mutation is distinct content, so no two may collide either.
        if let Some(other) = seen.insert(hash, name.clone()) {
            panic!("{name} hashes like {other}");
        }
    }
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

const FAST: Duration = Duration::from_millis(50);

fn gate(state: &Arc<dyn RegistryStateStore>, generation: Option<u64>) -> Gate {
    Gate {
        state: state.clone(),
        owner: Uuid::new_v4(),
        generation,
        lease_ttl: LEASE_TTL,
        poll: FAST,
        jitter: |d| d,
    }
}

/// One pod: a Control Plane (in memory), its registry write path and its
/// gate.
struct Pod {
    cp: Arc<ControlPlaneServiceImpl>,
    provisioner: Arc<RegistryProvisionerImpl>,
    gate: Gate,
}

impl Pod {
    fn new(state: &Arc<dyn RegistryStateStore>, generation: Option<u64>) -> Self {
        let (cp, provisioner) = in_memory_planes();
        Self {
            cp,
            provisioner,
            gate: gate(state, generation),
        }
    }

    async fn run(
        &self,
        upstreams: &[ProvisionedUpstream],
        routes: &[ProvisionedRoute],
        root: Uuid,
    ) -> RunOutcome {
        gated_reconcile(
            &self.gate,
            &*self.provisioner,
            &ProvisioningMetrics::default(),
            &CancellationToken::new(),
            upstreams,
            routes,
            root,
        )
        .await
        .expect("a gated run")
    }

    /// Run in a task, as a second pod would.
    fn spawn(
        &self,
        upstreams: Vec<ProvisionedUpstream>,
        routes: Vec<ProvisionedRoute>,
        root: Uuid,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<anyhow::Result<RunOutcome>> {
        let (gate, provisioner) = (self.gate.clone(), self.provisioner.clone());
        tokio::spawn(async move {
            gated_reconcile(
                &gate,
                &*provisioner,
                &ProvisioningMetrics::default(),
                &cancel,
                &upstreams,
                &routes,
                root,
            )
            .await
        })
    }

    async fn upstream_count(&self, root: Uuid) -> usize {
        use crate::domain::services::ControlPlaneService;
        self.cp
            .list_upstreams(
                &super::super::registry_reconcile::provisioning_ctx(root).unwrap(),
                &crate::domain::model::ListQuery { top: 100, skip: 0 },
            )
            .await
            .unwrap()
            .len()
    }
}

/// A store whose refused `try_acquire` signals `waiting`: a pod that got
/// it is in its poll loop, waiting on the lease.
struct SignalsWait {
    inner: Arc<dyn RegistryStateStore>,
    waiting: Arc<tokio::sync::Notify>,
}

/// `state` for a waiting pod, and the signal that it is waiting.
fn signals_wait(
    state: &Arc<dyn RegistryStateStore>,
) -> (Arc<dyn RegistryStateStore>, Arc<tokio::sync::Notify>) {
    let waiting = Arc::new(tokio::sync::Notify::new());
    let store = Arc::new(SignalsWait {
        inner: state.clone(),
        waiting: waiting.clone(),
    });
    (store, waiting)
}

#[async_trait::async_trait]
impl RegistryStateStore for SignalsWait {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        self.inner.read().await
    }
    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        self.inner.claim(generation).await
    }
    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        let acquired = self.inner.try_acquire(owner, ttl, generation).await?;
        if !acquired {
            // Stores a permit when nobody waits yet, so the signal is not lost.
            self.waiting.notify_one();
        }
        Ok(acquired)
    }
    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError> {
        self.inner.renew(owner, ttl).await
    }
    async fn finish(&self, owner: Uuid, hash: Option<&str>) -> Result<bool, RepositoryError> {
        self.inner.finish(owner, hash).await
    }
}

/// Registry rows whose listing fails with an outage.
struct UnlistableRows(Arc<dyn RegistryRowRepository>);

#[async_trait::async_trait]
impl RegistryRowRepository for UnlistableRows {
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError> {
        Err(RepositoryError::Internal("storage unavailable".into()))
    }
    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError> {
        self.0.list_routes().await
    }
    async fn delete_upstream_if_unused(
        &self,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError> {
        self.0.delete_upstream_if_unused(tenant_id, id).await
    }
}

fn reconciled_clean(outcome: &RunOutcome) -> bool {
    matches!(outcome, RunOutcome::Reconciled(counts) if counts.is_clean())
}

/// Instantiate every gate case on every store, each on a fresh row.
macro_rules! gate_cases {
    ($($case:ident),+ $(,)?) => {
        mod in_memory {
            $(
                #[tokio::test]
                async fn $case() {
                    let store: std::sync::Arc<dyn super::RegistryStateStore> =
                        std::sync::Arc::new(super::InMemoryRegistryStateStore::new());
                    super::$case(store).await;
                }
            )+
        }

        mod sqlite {
            $(
                #[tokio::test]
                async fn $case() {
                    let db = super::testing::shared_sqlite().await;
                    super::$case(super::db_store(db)).await;
                }
            )+
        }

        #[cfg(feature = "integration")]
        mod postgres {
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn all_cases() {
                let server = super::testing::postgres_server().await;
                $(super::$case(super::db_store(server.fresh().await)).await;)+
            }
        }

        #[cfg(feature = "integration")]
        mod mysql {
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn all_cases() {
                let server = super::testing::mysql_server().await;
                $(super::$case(super::db_store(server.fresh().await)).await;)+
            }
        }
    };
}

fn db_store(db: toolkit_db::Db) -> Arc<dyn RegistryStateStore> {
    Arc::new(DbRegistryStateStore::new(
        DBProvider::<RepositoryError>::new(db),
    ))
}

gate_cases!(
    an_unchanged_registry_is_not_reconciled_again,
    a_waiting_pod_finishes_on_the_holders_hash,
    an_expired_lease_is_taken_over,
    skips_and_kept_upstreams_leave_the_hash_unrecorded,
    an_older_generation_writes_nothing,
    a_newer_generation_with_the_same_content_only_claims,
    the_stored_generation_never_decreases,
    a_waiting_pod_stops_when_a_newer_generation_claims,
    an_unset_generation_leaves_the_stored_one_alone,
    a_cancelled_wait_writes_nothing,
    a_failed_run_releases_the_lease_at_once,
    a_rollback_after_an_unclean_run_reconciles,
);

/// ADR-0018: PERS-GATE-001.
async fn an_unchanged_registry_is_not_reconciled_again(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let pod = Pod::new(&state, None);
    assert!(reconciled_clean(&pod.run(&upstreams, &routes, root).await));
    assert_eq!(
        state.read().await.unwrap().content_hash,
        Some(content_hash(&upstreams, &routes, root))
    );
    assert_eq!(
        pod.run(&upstreams, &routes, root).await,
        RunOutcome::UpToDate { waited: false }
    );
    // Another pod with the same content writes nothing at all.
    let other = Pod::new(&state, None);
    assert_eq!(
        other.run(&upstreams, &routes, root).await,
        RunOutcome::UpToDate { waited: false }
    );
    assert_eq!(other.upstream_count(root).await, 0);
}

/// ADR-0018: PERS-GATE-002.
async fn a_waiting_pod_finishes_on_the_holders_hash(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let holder = Uuid::new_v4();
    assert!(state.try_acquire(holder, LEASE_TTL, None).await.unwrap());

    let (store, wait) = signals_wait(&state);
    let waiting = Pod::new(&store, None);
    let task = waiting.spawn(
        upstreams.clone(),
        routes.clone(),
        root,
        CancellationToken::new(),
    );
    wait.notified().await;
    assert!(!task.is_finished(), "a held lease is waited on");

    let hash = content_hash(&upstreams, &routes, root);
    assert!(state.finish(holder, Some(&hash)).await.unwrap());
    let outcome = task.await.unwrap().unwrap();
    assert_eq!(outcome, RunOutcome::UpToDate { waited: true });
    assert_eq!(waiting.upstream_count(root).await, 0);
}

/// ADR-0018: PERS-GATE-003.
async fn an_expired_lease_is_taken_over(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    // A holder that died: its one-second lease is never renewed.
    assert!(
        state
            .try_acquire(Uuid::new_v4(), Duration::from_secs(1), None)
            .await
            .unwrap()
    );
    let pod = Pod::new(&state, None);
    let outcome = tokio::time::timeout(Duration::from_secs(10), pod.run(&upstreams, &routes, root))
        .await
        .expect("the lapsed lease is taken over");
    assert!(reconciled_clean(&outcome), "{outcome:?}");
    assert_eq!(pod.upstream_count(root).await, 1);
}

/// ADR-0018: PERS-GATE-008.
async fn skips_and_kept_upstreams_leave_the_hash_unrecorded(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (mut upstreams, routes) = registry_instances(root);
    upstreams.push(upstream_at(root, "10.0.0.1")); // no alias: skipped
    let pod = Pod::new(&state, None);
    let outcome = pod.run(&upstreams, &routes, root).await;
    assert!(
        matches!(&outcome, RunOutcome::Reconciled(c) if c.upstreams.failed == 1),
        "{outcome:?}"
    );
    assert_eq!(state.read().await.unwrap().content_hash, None);
    assert!(
        matches!(
            pod.run(&upstreams, &routes, root).await,
            RunOutcome::Reconciled(_)
        ),
        "every check retries a run with skips"
    );
    assert_eq!(state.read().await.unwrap().content_hash, None);
}

/// A run that is not clean forgets the last clean run's hash: rolling back
/// to that content reconciles the half-applied rows instead of only claiming.
///
/// ADR-0018: PERS-GATE-009.
async fn a_rollback_after_an_unclean_run_reconciles(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let first = Pod::new(&state, Some(1));
    assert!(reconciled_clean(
        &first.run(&upstreams, &routes, root).await
    ));

    let mut changed = upstreams.clone();
    changed.push(upstream_at(root, "10.0.0.1")); // no alias: skipped
    let next = Pod::new(&state, Some(2));
    let outcome = next.run(&changed, &routes, root).await;
    assert!(
        matches!(&outcome, RunOutcome::Reconciled(c) if !c.is_clean()),
        "{outcome:?}"
    );
    assert_eq!(state.read().await.unwrap().content_hash, None);

    let rollback = Pod::new(&state, Some(3));
    let outcome = rollback.run(&upstreams, &routes, root).await;
    assert!(reconciled_clean(&outcome), "{outcome:?}");
    assert_eq!(rollback.upstream_count(root).await, 1);
    assert_eq!(
        state.read().await.unwrap(),
        RegistryState {
            generation: Some(3),
            content_hash: Some(content_hash(&upstreams, &routes, root)),
        }
    );
}

/// ADR-0018: PERS-GATE-010.
async fn an_older_generation_writes_nothing(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    assert!(state.claim(5).await.unwrap());
    let old = Pod::new(&state, Some(4));
    assert_eq!(
        old.run(&upstreams, &routes, root).await,
        RunOutcome::Stale { stored: 5 }
    );
    assert_eq!(old.upstream_count(root).await, 0);
    assert_eq!(
        state.read().await.unwrap(),
        RegistryState {
            generation: Some(5),
            content_hash: None
        }
    );
}

/// ADR-0018: PERS-GATE-011.
async fn a_newer_generation_with_the_same_content_only_claims(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let first = Pod::new(&state, Some(1));
    assert!(reconciled_clean(
        &first.run(&upstreams, &routes, root).await
    ));
    assert_eq!(
        state.read().await.unwrap().generation,
        Some(1),
        "acquire claims"
    );

    let next = Pod::new(&state, Some(2));
    assert_eq!(
        next.run(&upstreams, &routes, root).await,
        RunOutcome::Claimed
    );
    assert_eq!(next.upstream_count(root).await, 0, "a claim writes no rows");
    assert_eq!(state.read().await.unwrap().generation, Some(2));
    assert_eq!(
        next.run(&upstreams, &routes, root).await,
        RunOutcome::UpToDate { waited: false }
    );
}

/// ADR-0018: PERS-GATE-012.
async fn the_stored_generation_never_decreases(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let newer = Pod::new(&state, Some(3));
    assert!(reconciled_clean(
        &newer.run(&upstreams, &routes, root).await
    ));

    let (older_content, older_routes) = registry_instances(root);
    let older = Pod::new(&state, Some(2));
    assert_eq!(
        older.run(&older_content, &older_routes, root).await,
        RunOutcome::Stale { stored: 3 }
    );
    let stored = state.read().await.unwrap();
    assert_eq!(stored.generation, Some(3));
    assert_eq!(
        stored.content_hash,
        Some(content_hash(&upstreams, &routes, root))
    );
}

/// ADR-0018: PERS-GATE-010.
async fn a_waiting_pod_stops_when_a_newer_generation_claims(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let holder = Uuid::new_v4();
    assert!(state.try_acquire(holder, LEASE_TTL, Some(1)).await.unwrap());

    let (store, wait) = signals_wait(&state);
    let waiting = Pod::new(&store, Some(2));
    let task = waiting.spawn(upstreams, routes, root, CancellationToken::new());
    wait.notified().await;
    assert!(!task.is_finished());
    assert!(state.claim(3).await.unwrap());
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the waiting pod sees the newer generation")
        .unwrap()
        .unwrap();
    assert_eq!(outcome, RunOutcome::Stale { stored: 3 });
    assert_eq!(waiting.upstream_count(root).await, 0);
}

/// ADR-0018: PERS-GATE-012.
async fn an_unset_generation_leaves_the_stored_one_alone(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    assert!(state.claim(5).await.unwrap());
    let unguarded = Pod::new(&state, None);
    assert!(reconciled_clean(
        &unguarded.run(&upstreams, &routes, root).await
    ));
    assert_eq!(state.read().await.unwrap().generation, Some(5));
}

/// ADR-0018: PERS-LOOP-006.
async fn a_cancelled_wait_writes_nothing(state: Arc<dyn RegistryStateStore>) {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    assert!(
        state
            .try_acquire(Uuid::new_v4(), LEASE_TTL, None)
            .await
            .unwrap()
    );
    let cancel = CancellationToken::new();
    let (store, wait) = signals_wait(&state);
    let waiting = Pod::new(&store, None);
    let task = waiting.spawn(upstreams, routes, root, cancel.clone());
    wait.notified().await;
    cancel.cancel();
    assert_eq!(task.await.unwrap().unwrap(), RunOutcome::Cancelled);
    assert_eq!(waiting.upstream_count(root).await, 0);
}

/// A run that fails on an outage releases its lease on the way out: another
/// pod takes it at once instead of waiting out the TTL, and no hash is
/// recorded.
///
/// ADR-0018: PERS-GATE-006.
async fn a_failed_run_releases_the_lease_at_once(state: Arc<dyn RegistryStateStore>) {
    use crate::domain::services::registry_reconcile::tests::{Backend, Stores, planes_over};
    use crate::domain::test_support::{MockCredStoreClient, MockTenantResolverClient};

    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let stores = Stores::new(Backend::Memory);
    let stores = Stores {
        rows: Arc::new(UnlistableRows(stores.rows.clone())),
        ..stores
    };
    let (cp, provisioner) = planes_over(
        stores,
        Arc::new(MockTenantResolverClient::single_tenant()),
        Arc::new(MockCredStoreClient::empty()),
    );
    let pod = Pod {
        cp,
        provisioner,
        gate: gate(&state, None),
    };
    let result = gated_reconcile(
        &pod.gate,
        &*pod.provisioner,
        &ProvisioningMetrics::default(),
        &CancellationToken::new(),
        &upstreams,
        &routes,
        root,
    )
    .await;
    assert!(result.is_err(), "{result:?}");
    assert!(
        state
            .try_acquire(Uuid::new_v4(), LEASE_TTL, None)
            .await
            .unwrap(),
        "the lease was released"
    );
    assert_eq!(state.read().await.unwrap().content_hash, None);
}

/// Registry rows whose listing never answers.
struct HangingRows(Arc<dyn RegistryRowRepository>);

#[async_trait::async_trait]
impl RegistryRowRepository for HangingRows {
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError> {
        std::future::pending().await
    }
    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError> {
        self.0.list_routes().await
    }
    async fn delete_upstream_if_unused(
        &self,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError> {
        self.0.delete_upstream_if_unused(tenant_id, id).await
    }
}

/// A gated run over in-memory stores whose registry rows are `rows`.
async fn gated_run_over(
    state: &Arc<dyn RegistryStateStore>,
    rows: impl FnOnce(Arc<dyn RegistryRowRepository>) -> Arc<dyn RegistryRowRepository>,
) -> anyhow::Result<RunOutcome> {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    gated_run_of(state, None, root, &upstreams, &routes, rows).await
}

/// A gated run of `upstreams` and `routes` at `generation`, over in-memory
/// stores whose registry rows are `rows`.
async fn gated_run_of(
    state: &Arc<dyn RegistryStateStore>,
    generation: Option<u64>,
    root: Uuid,
    upstreams: &[ProvisionedUpstream],
    routes: &[ProvisionedRoute],
    rows: impl FnOnce(Arc<dyn RegistryRowRepository>) -> Arc<dyn RegistryRowRepository>,
) -> anyhow::Result<RunOutcome> {
    use crate::domain::services::registry_reconcile::tests::{Backend, Stores, planes_over};
    use crate::domain::test_support::{MockCredStoreClient, MockTenantResolverClient};

    let stores = Stores::new(Backend::Memory);
    let stores = Stores {
        rows: rows(stores.rows.clone()),
        ..stores
    };
    let (_, provisioner) = planes_over(
        stores,
        Arc::new(MockTenantResolverClient::single_tenant()),
        Arc::new(MockCredStoreClient::empty()),
    );
    gated_reconcile(
        &gate(state, generation),
        &*provisioner,
        &ProvisioningMetrics::default(),
        &CancellationToken::new(),
        upstreams,
        routes,
        root,
    )
    .await
}

/// A run that ends on a call timeout keeps its lease until the TTL lapses:
/// the call may still complete on the database, and must land under this
/// pod's lease, not under the next owner's.
///
/// ADR-0018: PERS-GATE-006.
#[tokio::test(start_paused = true)]
async fn a_timed_out_run_keeps_its_lease_until_the_ttl() {
    use crate::domain::services::registry_reconcile::CallTimedOut;

    let state: Arc<dyn RegistryStateStore> = Arc::new(InMemoryRegistryStateStore::new());
    let result = gated_run_over(&state, |rows| Arc::new(HangingRows(rows))).await;
    let e = result.expect_err("the hung listing aborts the run");
    assert!(e.is::<CallTimedOut>(), "{e:#}");

    let other = Uuid::new_v4();
    assert!(
        !state.try_acquire(other, LEASE_TTL, None).await.unwrap(),
        "the lease is kept"
    );
    tokio::time::advance(LEASE_TTL).await;
    assert!(
        state.try_acquire(other, LEASE_TTL, None).await.unwrap(),
        "the TTL frees it"
    );
    assert_eq!(state.read().await.unwrap().content_hash, None);
}

/// The registry-state statement that never answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hung {
    Read,
    Claim,
    TryAcquire,
    Renew,
    Finish,
}

/// A store whose statement `.1` never answers.
struct HangsOn(InMemoryRegistryStateStore, Hung);

impl HangsOn {
    async fn hang_on(&self, statement: Hung) {
        if self.1 == statement {
            std::future::pending::<()>().await;
        }
    }
}

#[async_trait]
impl RegistryStateStore for HangsOn {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        self.hang_on(Hung::Read).await;
        self.0.read().await
    }
    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        self.hang_on(Hung::Claim).await;
        self.0.claim(generation).await
    }
    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        self.hang_on(Hung::TryAcquire).await;
        self.0.try_acquire(owner, ttl, generation).await
    }
    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError> {
        self.hang_on(Hung::Renew).await;
        self.0.renew(owner, ttl).await
    }
    async fn finish(&self, owner: Uuid, hash: Option<&str>) -> Result<bool, RepositoryError> {
        self.hang_on(Hung::Finish).await;
        self.0.finish(owner, hash).await
    }
}

/// A registry-state statement that never answers fails the run once the
/// call timeout has passed: the read before the gate, the claim of a newer
/// generation, the lease acquire, a renewal while the lease is held, and the
/// finish.
///
/// ADR-0018: PERS-GATE-007.
#[tokio::test(start_paused = true)]
async fn a_hung_state_statement_fails_the_run_after_the_call_timeout() {
    use crate::domain::services::registry_reconcile::{CALL_TIMEOUT, CallTimedOut};

    for hung in [
        Hung::Read,
        Hung::Claim,
        Hung::TryAcquire,
        Hung::Renew,
        Hung::Finish,
    ] {
        let root = Uuid::new_v4();
        let (upstreams, routes) = registry_instances(root);
        let inner = InMemoryRegistryStateStore::new();
        if hung == Hung::Claim {
            // A claim needs the content recorded and a newer generation.
            let owner = Uuid::new_v4();
            assert!(inner.try_acquire(owner, LEASE_TTL, None).await.unwrap());
            let hash = content_hash(&upstreams, &routes, root);
            assert!(inner.finish(owner, Some(&hash)).await.unwrap());
        }
        let state: Arc<dyn RegistryStateStore> = Arc::new(HangsOn(inner, hung));
        let started = Instant::now();
        let e = gated_run_of(&state, Some(1), root, &upstreams, &routes, |rows| rows)
            .await
            .expect_err("a hung statement fails the run");
        assert!(e.is::<CallTimedOut>(), "{hung:?}: {e:#}");
        let waited = started.elapsed();
        assert!(
            waited >= CALL_TIMEOUT && waited < CALL_TIMEOUT * 2,
            "{hung:?}: failed after {waited:?}"
        );
    }
}

/// An unchanged registry costs one statement: the read of the state row.
///
/// ADR-0018: PERS-GATE-001.
#[tokio::test]
async fn an_up_to_date_check_is_one_select() {
    let (db, recorder) = testing::recorded_sqlite().await;
    let state = db_store(db);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let pod = Pod::new(&state, None);
    assert!(reconciled_clean(&pod.run(&upstreams, &routes, root).await));
    recorder.clear();

    assert_eq!(
        pod.run(&upstreams, &routes, root).await,
        RunOutcome::UpToDate { waited: false }
    );
    let events = recorder.events();
    assert_eq!(events.len(), 1, "{}", recorder.dump());
    assert_eq!(events[0].table.as_deref(), Some("oagw_registry_state"));
}

/// A store whose lease is lost at the first renewal.
struct LosesLease(InMemoryRegistryStateStore);

#[async_trait]
impl RegistryStateStore for LosesLease {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        self.0.read().await
    }
    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        self.0.claim(generation).await
    }
    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        self.0.try_acquire(owner, ttl, generation).await
    }
    async fn renew(&self, _: Uuid, _: Duration) -> Result<bool, RepositoryError> {
        Ok(false)
    }
    async fn finish(&self, owner: Uuid, hash: Option<&str>) -> Result<bool, RepositoryError> {
        self.0.finish(owner, hash).await
    }
}

/// A pod that lost its lease mid-run stops writing and records nothing.
///
/// ADR-0018: PERS-GATE-004.
#[tokio::test]
async fn a_lost_lease_stops_the_writes_and_records_nothing() {
    let state: Arc<dyn RegistryStateStore> =
        Arc::new(LosesLease(InMemoryRegistryStateStore::new()));
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let pod = Pod::new(&state, None);
    let outcome = pod.run(&upstreams, &routes, root).await;
    assert!(
        matches!(&outcome, RunOutcome::Reconciled(c) if c.stopped),
        "{outcome:?}"
    );
    assert_eq!(outcome.metric_label(), provisioning_outcome::NOT_CLEAN);
    assert_eq!(state.read().await.unwrap().content_hash, None);
    assert_eq!(pod.upstream_count(root).await, 0, "stopped before phase 4");
}

/// The lease's `finish` finds it gone: another pod took over at the very
/// end.
struct FinishFindsLeaseGone(InMemoryRegistryStateStore);

#[async_trait::async_trait]
impl RegistryStateStore for FinishFindsLeaseGone {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        self.0.read().await
    }
    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        self.0.claim(generation).await
    }
    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        self.0.try_acquire(owner, ttl, generation).await
    }
    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError> {
        self.0.renew(owner, ttl).await
    }
    async fn finish(&self, _: Uuid, _: Option<&str>) -> Result<bool, RepositoryError> {
        Ok(false)
    }
}

/// A run that lost its lease before recording the hash is not clean, even
/// with nothing skipped: another pod may have written meanwhile.
///
/// ADR-0018: PERS-GATE-004.
#[tokio::test]
async fn a_lease_lost_before_the_hash_is_recorded_is_not_clean() {
    let state: Arc<dyn RegistryStateStore> =
        Arc::new(FinishFindsLeaseGone(InMemoryRegistryStateStore::new()));
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let outcome = Pod::new(&state, None).run(&upstreams, &routes, root).await;
    assert!(
        matches!(&outcome, RunOutcome::Reconciled(c) if c.stopped && c.upstreams.created == 1),
        "{outcome:?}"
    );
    assert_eq!(outcome.metric_label(), provisioning_outcome::NOT_CLEAN);
    assert!(!outcome.is_settled());
    assert_eq!(state.read().await.unwrap().content_hash, None);
}

/// Before a write the lease is renewed once a third of its TTL has passed
/// since the last renewal, and not before; a renewal that finds the lease
/// lost stops the writes.
///
/// ADR-0018: PERS-GATE-005.
#[tokio::test(start_paused = true)]
async fn a_write_renews_the_lease_once_a_third_of_the_ttl_has_passed() {
    let cancel = CancellationToken::new();

    let healthy: Arc<dyn RegistryStateStore> = Arc::new(InMemoryRegistryStateStore::new());
    let g = gate(&healthy, None);
    assert!(healthy.try_acquire(g.owner, LEASE_TTL, None).await.unwrap());
    let control = LeaseControl {
        gate: &g,
        cancel: &cancel,
        renewed_at: parking_lot::Mutex::new(Instant::now()),
    };
    tokio::time::advance(LEASE_TTL / 3).await;
    assert!(control.before_write().await.unwrap());
    assert_eq!(
        control.renewed_at.lock().elapsed(),
        Duration::ZERO,
        "renewed when due"
    );

    let losing: Arc<dyn RegistryStateStore> =
        Arc::new(LosesLease(InMemoryRegistryStateStore::new()));
    let g = gate(&losing, None);
    let control = LeaseControl {
        gate: &g,
        cancel: &cancel,
        renewed_at: parking_lot::Mutex::new(Instant::now()),
    };
    tokio::time::advance(LEASE_TTL / 3 - Duration::from_millis(1)).await;
    assert!(
        control.before_write().await.unwrap(),
        "not due: no renewal, so the lost lease is not seen yet"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(
        !control.before_write().await.unwrap(),
        "due: the renewal finds the lease lost"
    );

    cancel.cancel();
    assert!(
        !control.before_write().await.unwrap(),
        "a stop stops the writes"
    );
}

#![allow(clippy::expect_used, clippy::unwrap_used)]

//! A call about a tenant the caller is not a subject of (gears-rust #5623).
//!
//! The caller here is a service identity whose own tenant is the platform
//! root, as Construct's is. It names the tenant every call is about through
//! `GraphStorageClientV1::in_tenant`, and the PDP decides. The stub PDP
//! answers the way a tenant-resolving plugin does in `RootOnly` mode: a
//! granted target comes back as `owner_tenant_id IN (target)`, a refused one
//! as a denial. Variants answer more loosely than that, to show the gear
//! holds its own line whatever the PDP returns.

#[allow(dead_code)]
mod conformance;
mod support;

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::api::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, TenantMode,
};
use graph_storage::domain::local_client::GraphStorageLocalClient;
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::{
    NeighborhoodRequest, SearchMode, SearchRequest, TraverseRequest, TypeQuery,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use support::Harness;

/// How the stub answers a call that names a target tenant.
#[derive(Clone)]
enum Answer {
    /// `IN (target)` for a granted target, a denial otherwise.
    Exact,
    /// `IN (target, extra...)`: a PDP that returns a subtree although it was
    /// asked for the root only.
    Subtree(Vec<Uuid>),
    /// `IN (other)`: a grant that names some other tenant.
    Elsewhere(Uuid),
}

struct ActsIn {
    granted: Vec<Uuid>,
    answer: Answer,
}

fn allow(tenants: Vec<Uuid>) -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    tenants,
                ))],
            }],
            deny_reason: None,
        },
    }
}

#[async_trait]
impl AuthZResolverApi for ActsIn {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let Some(tenant_context) = request.context.tenant_context.clone() else {
            return support::AllowInOwnTenant.evaluate(ctx, request).await;
        };
        let Some(target) = tenant_context.root_id else {
            return support::AllowInOwnTenant.evaluate(ctx, request).await;
        };
        assert!(
            matches!(tenant_context.mode, TenantMode::RootOnly),
            "a target tenant is asked about in root-only mode"
        );
        if !self.granted.contains(&target) {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            });
        }
        Ok(match &self.answer {
            Answer::Exact => allow(vec![target]),
            Answer::Subtree(extra) => {
                let mut tenants = vec![target];
                tenants.extend(extra.iter().copied());
                allow(tenants)
            }
            Answer::Elsewhere(other) => allow(vec![*other]),
        })
    }
}

/// The service identity: a subject of the platform root, never of the
/// tenants it acts in.
fn service_identity(root: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(root)
        .build()
        .expect("a valid security context")
}

fn member_of(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("a valid security context")
}

fn client_with(answer: Answer, granted: Vec<Uuid>) -> (Harness, Arc<dyn GraphStorageClientV1>) {
    let harness = Harness::with(Arc::new(ActsIn { granted, answer }));
    let client: Arc<dyn GraphStorageClientV1> =
        Arc::new(GraphStorageLocalClient::new(Arc::clone(&harness.services)));
    (harness, client)
}

async fn seed(client: &dyn GraphStorageClientV1, ctx: &SecurityContext, keys: &[&str]) {
    client
        .register_types(ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    client
        .ingest(
            ctx,
            conformance::batch(
                keys.iter().map(|key| conformance::node(key, key)).collect(),
                Vec::new(),
            ),
        )
        .await
        .expect("the batch commits");
}

/// A granted target receives the writes, a member of that tenant reads them,
/// and the service identity's own tenant has none of them. The acting
/// subject recorded on the row is the service identity.
#[tokio::test]
async fn a_granted_target_tenant_owns_what_the_call_writes() {
    let (root, target) = (Uuid::now_v7(), Uuid::now_v7());
    let (_harness, client) = client_with(Answer::Exact, vec![target]);
    let service = service_identity(root);
    let scoped = client.in_tenant(target);

    seed(scoped.as_ref(), &service, &["fact-1"]).await;

    let view = scoped
        .get_node(&service, &"fact-1".to_owned(), None)
        .await
        .expect("the target's node reads through the target");
    assert_eq!(view.envelope.tenant_id, target, "the row is the target's");
    assert_eq!(
        view.envelope.created_by.subject_id,
        service.subject_id(),
        "the actor is the caller, not a subject of the target"
    );

    let member = member_of(target);
    client
        .get_node(&member, &"fact-1".to_owned(), None)
        .await
        .expect("a member of the target reads the same row in its own tenant");

    let unscoped = client
        .get_node(&service, &"fact-1".to_owned(), None)
        .await
        .expect_err("the caller's own tenant holds nothing of it");
    assert_eq!(unscoped.status_code(), 404);
}

/// A read about the target answers the target alone, even when the PDP
/// returns the target's subtree: the scope is pinned to the target.
#[tokio::test]
async fn a_read_about_a_target_never_returns_another_tenants_rows() {
    let (root, target, child) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (_harness, client) = client_with(Answer::Subtree(vec![child]), vec![target, child]);
    let service = service_identity(root);

    seed(
        client.in_tenant(target).as_ref(),
        &service,
        &["shared", "only-target"],
    )
    .await;
    seed(
        client.in_tenant(child).as_ref(),
        &service,
        &["shared", "only-child"],
    )
    .await;

    let scoped = client.in_tenant(target);
    let page = scoped
        .project_nodes(&service, &[], toolkit_odata::ODataQuery::default())
        .await
        .expect("the target lists");
    let mut keys: Vec<_> = page.items.iter().map(|row| row.node_key.clone()).collect();
    keys.sort();
    assert_eq!(keys, ["only-target", "shared"], "the target's rows alone");
    assert!(
        page.items
            .iter()
            .all(|row| row.envelope.tenant_id == target),
        "no row of the child: {page:?}"
    );
    let count = scoped
        .count_nodes(&service, &[], toolkit_odata::ODataQuery::default())
        .await
        .expect("the target counts");
    assert_eq!(count.count, 2);

    let types = scoped
        .list_types(&service, TypeQuery::default())
        .await
        .expect("the target's types list");
    assert!(!types.items.is_empty());
}

/// The scope a target's call runs under names the target in every
/// constraint, whatever the PDP returned: a subtree for a root-only question
/// is narrowed, never served. The in-memory store keys rows by the call's
/// tenant as well, so only the scope itself shows the difference; on the
/// built-in store the scope is the filter.
#[tokio::test]
async fn a_subtree_the_pdp_returns_for_a_target_is_pinned_to_the_target() {
    use graph_storage::domain::authz;
    use toolkit_security::ScopeFilter;

    let (root, target, child) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let enforcer = authz_resolver_sdk::pep::PolicyEnforcer::new(Arc::new(ActsIn {
        granted: vec![target],
        answer: Answer::Subtree(vec![child]),
    }));
    let service = service_identity(root);
    for action in [authz::actions::READ, authz::actions::WRITE] {
        let scope = authz::scope_for(
            &enforcer,
            &service,
            Some(target),
            &authz::node_resource(),
            action,
        )
        .await
        .expect("the target is granted");
        let pinned = ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, target);
        assert!(
            !scope.constraints().is_empty()
                && scope
                    .constraints()
                    .iter()
                    .all(|constraint| constraint.filters().contains(&pinned)),
            "{action}: every constraint is narrowed to the target: {scope:?}"
        );
    }
}

/// A refused target tenant is logged with the caller and the tenant it asked
/// for: one service identity acts in many tenants, and the 404 the caller
/// gets says nothing to an operator.
#[tokio::test(flavor = "current_thread")]
async fn a_refused_target_tenant_is_logged_with_ids() {
    use std::sync::{Arc as StdArc, Mutex};
    use tracing_subscriber::layer::SubscriberExt as _;

    #[derive(Clone, Default)]
    struct Captured(StdArc<Mutex<Vec<String>>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Captured {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Fields(String);
            impl tracing::field::Visit for Fields {
                #[expect(
                    clippy::use_debug,
                    reason = "tracing hands every field to a visitor as `dyn Debug`"
                )]
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    use std::fmt::Write as _;
                    write!(self.0, "{}={value:?} ", field.name()).expect("writing to a String");
                }
            }
            let mut fields = Fields(String::new());
            event.record(&mut fields);
            self.0.lock().expect("not poisoned").push(fields.0);
        }
    }

    let captured = Captured::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(captured.clone()));

    let (root, refused) = (Uuid::now_v7(), Uuid::now_v7());
    let (_harness, client) = client_with(Answer::Exact, Vec::new());
    let service = service_identity(root);
    client
        .in_tenant(refused)
        .revision(&service)
        .await
        .expect_err("refused");

    let events = captured.0.lock().expect("not poisoned");
    let line = events
        .iter()
        .find(|line| line.contains("a call about a target tenant was refused"))
        .unwrap_or_else(|| panic!("the refusal is logged: {events:?}"));
    assert!(
        line.contains(&refused.to_string()),
        "names the target: {line}"
    );
    assert!(
        line.contains(&service.subject_id().to_string()),
        "names the caller: {line}"
    );
}

/// Every method answers a target the PDP refuses with the same 404 an
/// unknown tenant gets, and so does a grant that names another tenant.
#[tokio::test]
async fn a_refused_target_is_not_found_on_every_method() {
    let (root, granted, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let service = service_identity(root);

    let (_h1, refusing) = client_with(Answer::Exact, vec![granted]);
    let (_h2, elsewhere) = client_with(Answer::Elsewhere(other), vec![granted]);
    for (label, scoped) in [
        ("refused", refusing.in_tenant(Uuid::now_v7())),
        ("granted elsewhere", elsewhere.in_tenant(granted)),
    ] {
        let key = "any".to_owned();
        let results: Vec<(&str, Result<(), CanonicalError>)> = vec![
            (
                "register_types",
                scoped
                    .register_types(&service, conformance::ontology_batch())
                    .await
                    .map(drop),
            ),
            (
                "get_type",
                scoped
                    .get_type(&service, &conformance::OWNED.to_owned())
                    .await
                    .map(drop),
            ),
            (
                "list_types",
                scoped
                    .list_types(&service, TypeQuery::default())
                    .await
                    .map(drop),
            ),
            (
                "ingest",
                scoped
                    .ingest(
                        &service,
                        conformance::batch(vec![conformance::node("n", "n")], Vec::new()),
                    )
                    .await
                    .map(drop),
            ),
            (
                "delete_node",
                scoped.delete_node(&service, &key).await.map(drop),
            ),
            (
                "delete_edge",
                scoped.delete_edge(&service, &key).await.map(drop),
            ),
            (
                "get_node",
                scoped.get_node(&service, &key, None).await.map(drop),
            ),
            (
                "project_nodes",
                scoped
                    .project_nodes(&service, &[], toolkit_odata::ODataQuery::default())
                    .await
                    .map(drop),
            ),
            (
                "count_nodes",
                scoped
                    .count_nodes(&service, &[], toolkit_odata::ODataQuery::default())
                    .await
                    .map(drop),
            ),
            (
                "search",
                scoped
                    .search(
                        &service,
                        SearchRequest {
                            mode: SearchMode::Lexical,
                            query: Some("anything".to_owned()),
                            arm_limit: 10,
                            limit: 10,
                            type_patterns: Vec::new(),
                        },
                    )
                    .await
                    .map(drop),
            ),
            (
                "traverse",
                scoped
                    .traverse(
                        &service,
                        TraverseRequest {
                            seeds: vec![key.clone()],
                            depth: 1,
                            ..TraverseRequest::default()
                        },
                    )
                    .await
                    .map(drop),
            ),
            (
                "neighborhood",
                scoped
                    .neighborhood(
                        &service,
                        NeighborhoodRequest {
                            root: key.clone(),
                            depth: 1,
                            ..NeighborhoodRequest::default()
                        },
                    )
                    .await
                    .map(drop),
            ),
            ("revision", scoped.revision(&service).await.map(drop)),
        ];
        for (method, result) in results {
            let error = result.expect_err(method);
            assert_eq!(
                error.status_code(),
                404,
                "{label}: {method} answers not found: {error:?}"
            );
        }
    }
}

/// `in_tenant` on a scoped client replaces the tenant; it does not stack.
#[tokio::test]
async fn naming_a_tenant_again_replaces_the_first() {
    let (root, first, second) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let (_harness, client) = client_with(Answer::Exact, vec![first, second]);
    let service = service_identity(root);

    seed(
        client.in_tenant(first).in_tenant(second).as_ref(),
        &service,
        &["k"],
    )
    .await;

    let view = client
        .in_tenant(second)
        .get_node(&service, &"k".to_owned(), None)
        .await
        .expect("the row is the second tenant's");
    assert_eq!(view.envelope.tenant_id, second);
    client
        .in_tenant(first)
        .get_node(&service, &"k".to_owned(), None)
        .await
        .expect_err("and not the first's");
}

// ---------------------------------------------------------------------------
// Against the platform's tenant-hierarchy PDP
// ---------------------------------------------------------------------------

mod hierarchy {
    use super::*;
    use tenant_resolver_sdk::{
        GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
        GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverClient,
        TenantResolverError, TenantResolverPluginClient,
    };

    /// The static resolver plugin seen through the client trait the PDP
    /// consumes; the two traits are the same six reads.
    struct Tree(static_tr_plugin::domain::Service);

    #[async_trait]
    impl TenantResolverClient for Tree {
        async fn get_tenant(
            &self,
            ctx: &SecurityContext,
            id: TenantId,
        ) -> Result<TenantInfo, TenantResolverError> {
            self.0.get_tenant(ctx, id).await
        }

        async fn get_root_tenant(
            &self,
            ctx: &SecurityContext,
        ) -> Result<TenantInfo, TenantResolverError> {
            self.0.get_root_tenant(ctx).await
        }

        async fn get_tenants(
            &self,
            ctx: &SecurityContext,
            ids: &[TenantId],
            options: &GetTenantsOptions,
        ) -> Result<Vec<TenantInfo>, TenantResolverError> {
            self.0.get_tenants(ctx, ids, options).await
        }

        async fn get_ancestors(
            &self,
            ctx: &SecurityContext,
            id: TenantId,
            options: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            self.0.get_ancestors(ctx, id, options).await
        }

        async fn get_descendants(
            &self,
            ctx: &SecurityContext,
            id: TenantId,
            options: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            self.0.get_descendants(ctx, id, options).await
        }

        async fn is_ancestor(
            &self,
            ctx: &SecurityContext,
            ancestor_id: TenantId,
            descendant_id: TenantId,
            options: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            self.0
                .is_ancestor(ctx, ancestor_id, descendant_id, options)
                .await
        }
    }

    /// The tr-authz plugin's evaluation, as the resolver API the gear's
    /// enforcer calls.
    struct TrPdp(tr_authz_plugin::domain::Service);

    #[async_trait]
    impl AuthZResolverApi for TrPdp {
        async fn evaluate(
            &self,
            _ctx: PlatformSecurityContext,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, CanonicalError> {
            Ok(self.0.evaluate(&request).await)
        }
    }

    /// root -> { org, sibling }, org -> team.
    struct Tenants {
        root: Uuid,
        org: Uuid,
        team: Uuid,
        sibling: Uuid,
    }

    fn platform() -> (Tenants, Arc<dyn GraphStorageClientV1>) {
        let t = Tenants {
            root: Uuid::now_v7(),
            org: Uuid::now_v7(),
            team: Uuid::now_v7(),
            sibling: Uuid::now_v7(),
        };
        let config: static_tr_plugin::config::StaticTrPluginConfig =
            serde_json::from_value(serde_json::json!({
                "tenants": [
                    { "id": t.root, "name": "root" },
                    { "id": t.org, "name": "org", "parent_id": t.root },
                    { "id": t.team, "name": "team", "parent_id": t.org },
                    { "id": t.sibling, "name": "sibling", "parent_id": t.root },
                ]
            }))
            .expect("the tenant tree configures");
        let tree = static_tr_plugin::domain::Service::from_config(&config).expect("a valid tree");
        let pdp = tr_authz_plugin::domain::Service::new(Arc::new(Tree(tree)));
        let harness = Harness::with(Arc::new(TrPdp(pdp)));
        let client: Arc<dyn GraphStorageClientV1> =
            Arc::new(GraphStorageLocalClient::new(Arc::clone(&harness.services)));
        (t, client)
    }

    /// An identity of an ancestor reaches a descendant tenant; an identity
    /// of a sibling does not, and its answer is not found.
    #[tokio::test]
    async fn an_ancestor_reaches_a_descendant_and_a_sibling_does_not() {
        let (t, client) = platform();

        let from_root = service_identity(t.root);
        seed(client.in_tenant(t.team).as_ref(), &from_root, &["fact"]).await;
        let view = client
            .in_tenant(t.team)
            .get_node(&from_root, &"fact".to_owned(), None)
            .await
            .expect("the root's identity reads the team's node");
        assert_eq!(view.envelope.tenant_id, t.team);

        let from_org = service_identity(t.org);
        client
            .in_tenant(t.team)
            .get_node(&from_org, &"fact".to_owned(), None)
            .await
            .expect("an ancestor of the team reads it too");

        let from_sibling = service_identity(t.sibling);
        for target in [t.team, t.org] {
            let error = client
                .in_tenant(target)
                .get_node(&from_sibling, &"fact".to_owned(), None)
                .await
                .expect_err("a sibling's identity does not reach the org");
            assert_eq!(error.status_code(), 404, "{error:?}");
        }
        let error = client
            .in_tenant(t.team)
            .ingest(
                &from_sibling,
                conformance::batch(vec![conformance::node("intruder", "x")], Vec::new()),
            )
            .await
            .expect_err("nor write there");
        assert_eq!(error.status_code(), 404, "{error:?}");
    }

    /// Reads about the org stay in the org, although the hierarchy PDP would
    /// grant the org's identity its whole subtree without a target.
    #[tokio::test]
    async fn a_read_about_an_org_leaves_out_its_teams() {
        let (t, client) = platform();
        let from_root = service_identity(t.root);
        seed(client.in_tenant(t.org).as_ref(), &from_root, &["org-node"]).await;
        seed(
            client.in_tenant(t.team).as_ref(),
            &from_root,
            &["team-node"],
        )
        .await;

        let page = client
            .in_tenant(t.org)
            .project_nodes(&from_root, &[], toolkit_odata::ODataQuery::default())
            .await
            .expect("the org lists");
        let keys: Vec<_> = page.items.iter().map(|row| row.node_key.as_str()).collect();
        assert_eq!(keys, ["org-node"], "the org alone: {keys:?}");
    }
}

// ---------------------------------------------------------------------------
// Over REST: `?tenant=`
// ---------------------------------------------------------------------------

mod rest {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use graph_storage::api::rest::routes::register_routes;
    use tower::ServiceExt;

    struct NoopRegistry;

    impl toolkit::api::OpenApiRegistry for NoopRegistry {
        fn register_operation(&self, _spec: &toolkit::api::OperationSpec) {}

        fn ensure_schema_raw(
            &self,
            name: &str,
            _schemas: Vec<(
                String,
                utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
            )>,
        ) -> String {
            name.to_owned()
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    async fn send(router: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = router.clone().oneshot(request).await.expect("answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("a body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn get(path: &str) -> Request<Body> {
        Request::builder()
            .uri(format!("/graph-storage/v1{path}"))
            .body(Body::empty())
            .expect("a request")
    }

    /// Every route but readiness takes `?tenant=`: none refuses it as an
    /// unknown parameter, and each answers a refused tenant with 404. The list
    /// is the routes `register_routes` declares the parameter on; a route
    /// whose own query binding refuses parameters it does not name (`GET
    /// /types` did) answers 400 here.
    #[tokio::test]
    async fn every_route_takes_the_tenant_parameter() {
        let (root, target) = (Uuid::now_v7(), Uuid::now_v7());
        let (harness, client) = client_with(Answer::Exact, vec![target]);
        let service = service_identity(root);
        seed(client.in_tenant(target).as_ref(), &service, &["k"]).await;
        let router = register_routes(Router::new(), &NoopRegistry, Arc::clone(&harness.services))
            .layer(axum::Extension(service));

        let types: Vec<serde_json::Value> = conformance::ontology_batch()
            .into_iter()
            .map(|r| serde_json::json!({"type_id": r.type_id, "schema": r.schema}))
            .collect();
        let owned = conformance::OWNED;
        let endpoints: Vec<(&str, String, Option<serde_json::Value>)> = vec![
            (
                "POST",
                "/types".into(),
                Some(serde_json::json!({"types": types.clone()})),
            ),
            (
                "POST",
                "/types/compatibility".into(),
                Some(serde_json::json!({"types": types})),
            ),
            ("GET", "/types".into(), None),
            ("GET", format!("/types/{owned}"), None),
            ("GET", "/source-namespaces".into(), None),
            (
                "POST",
                "/source-namespaces/some-ns/owner".into(),
                Some(serde_json::json!({"owner_principal": "p"})),
            ),
            (
                "POST",
                "/ingest".into(),
                Some(serde_json::json!({"nodes": [{"node_key": "k2", "type_id": owned}]})),
            ),
            ("GET", "/nodes/k".into(), None),
            ("GET", "/edges/missing".into(), None),
            ("GET", "/nodes:count".into(), None),
            ("GET", "/nodes".into(), None),
            ("DELETE", "/nodes/k2".into(), None),
            ("DELETE", "/edges/missing".into(), None),
            (
                "POST",
                "/search".into(),
                Some(serde_json::json!({"mode": "lexical", "query": "k", "limit": 10})),
            ),
            (
                "POST",
                "/graph/traverse".into(),
                Some(serde_json::json!({"seeds": ["k"], "depth": 1})),
            ),
            (
                "POST",
                "/graph/neighborhood".into(),
                Some(serde_json::json!({"root": "k", "depth": 1})),
            ),
            ("GET", "/revision".into(), None),
        ];
        let request = |method: &str, path: &str, tenant: Uuid, body: Option<&serde_json::Value>| {
            let builder = Request::builder()
                .method(method)
                .uri(format!("/graph-storage/v1{path}?tenant={tenant}"));
            match body {
                Some(body) => builder
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string())),
                None => builder.body(Body::empty()),
            }
            .expect("a request")
        };
        for (method, path, body) in &endpoints {
            let (status, answer) =
                send(&router, request(method, path, target, body.as_ref())).await;
            assert_ne!(
                status,
                StatusCode::BAD_REQUEST,
                "{method} {path} refuses ?tenant= for a granted tenant: {answer}"
            );
            let (status, answer) = send(
                &router,
                request(method, path, Uuid::now_v7(), body.as_ref()),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{method} {path} answers a refused tenant with 404: {answer}"
            );
        }
    }

    /// A refused tenant answers 404, so every operation that takes `?tenant=`
    /// declares a 404 in the `OpenAPI` document, and generated clients know the
    /// refusal. Read off the operations the routes register, not a list kept
    /// by hand.
    #[test]
    fn every_operation_that_takes_a_tenant_declares_not_found() {
        #[derive(Default)]
        struct Recorder(std::sync::Mutex<Vec<toolkit::api::OperationSpec>>);
        impl toolkit::api::OpenApiRegistry for Recorder {
            fn register_operation(&self, spec: &toolkit::api::OperationSpec) {
                self.0.lock().expect("not poisoned").push(spec.clone());
            }
            fn ensure_schema_raw(
                &self,
                name: &str,
                _schemas: Vec<(
                    String,
                    utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
                )>,
            ) -> String {
                name.to_owned()
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let harness = Harness::allowed();
        let recorder = Recorder::default();
        let _router = register_routes(Router::new(), &recorder, Arc::clone(&harness.services));
        let specs = recorder.0.lock().expect("not poisoned");
        let targeted: Vec<_> = specs
            .iter()
            .filter(|spec| spec.params.iter().any(|param| param.name == "tenant"))
            .collect();
        assert_eq!(targeted.len(), 17, "the routes that take ?tenant=");
        for spec in targeted {
            assert!(
                spec.responses.iter().any(|response| response.status == 404),
                "{} {} takes ?tenant= and declares no 404",
                spec.method,
                spec.path
            );
        }
    }

    /// The query parameter is the in-process `in_tenant` over HTTP: a granted
    /// tenant answers, a refused and an unknown one answer the same 404, and
    /// a value that is not a tenant id is the caller's mistake.
    #[tokio::test]
    async fn the_tenant_parameter_scopes_a_rest_call() {
        let (root, target) = (Uuid::now_v7(), Uuid::now_v7());
        let (harness, client) = client_with(Answer::Exact, vec![target]);
        let service = service_identity(root);
        seed(client.in_tenant(target).as_ref(), &service, &["rest-fact"]).await;

        let router = register_routes(Router::new(), &NoopRegistry, Arc::clone(&harness.services))
            .layer(axum::Extension(service));

        let (status, body) = send(&router, get(&format!("/nodes/rest-fact?tenant={target}"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["envelope"]["tenant_id"], target.to_string(), "{body}");

        let (own, _) = send(&router, get("/nodes/rest-fact")).await;
        assert_eq!(
            own,
            StatusCode::NOT_FOUND,
            "the caller's own tenant has none of it"
        );

        let (refused, refused_body) = send(
            &router,
            get(&format!("/nodes/rest-fact?tenant={}", Uuid::now_v7())),
        )
        .await;
        let (unknown, unknown_body) = send(
            &router,
            get(&format!("/nodes/no-such-key?tenant={}", Uuid::now_v7())),
        )
        .await;
        assert_eq!(refused, StatusCode::NOT_FOUND);
        assert_eq!(unknown, StatusCode::NOT_FOUND);
        let strip = |mut value: serde_json::Value| {
            if let Some(object) = value.as_object_mut() {
                object.remove("trace_id");
                object.remove("instance");
            }
            value
        };
        assert_eq!(
            strip(refused_body),
            strip(unknown_body),
            "a refused tenant and an unknown key read alike"
        );

        let (bad, _) = send(&router, get("/nodes/rest-fact?tenant=not-a-uuid")).await;
        assert_eq!(bad, StatusCode::BAD_REQUEST);
    }
}

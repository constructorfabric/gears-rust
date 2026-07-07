//! API-level (router) §11 tests for the audit surface's cross-tenant elevation
//! seam. Drives the real `audit::router` over a migrated
//! Postgres (testcontainers) with a fake `PolicyEnforcer`, asserting the
//! cross-tenant deny paths at the HTTP seam:
//!
//!   (1) a cross-tenant `tamper-status` read whose caller is NOT entitled to the
//!       TARGET tenant ⇒ 403 `CROSS_TENANT_ACCESS_DENIED` (the PDP deny flows
//!       through `cross_tenant_role_authorized` → the gateway, in a real txn);
//!   (2) an AUTHORIZED cross-tenant `erasure` with NO investigation reason ⇒ 400
//!       `MISSING_INVESTIGATION_REASON`, rejected PRE-read (before any DB write);
//!   (3) an UNAUTHORIZED cross-tenant `erasure` ⇒ 403 `CROSS_TENANT_ACCESS_DENIED`
//!       even with no reason — proving role is checked BEFORE reason (§5), the
//!       order the shared `resolve_action_scope` enforces for every write path.
//!
//! These are the §11 cases that were previously missing — they would have
//! caught the original cross-tenant BOLA. A real DB backs `ApiState`; path (2)/(3) return
//! before the txn, path (1) opens a serializable txn the gateway rolls back.
//!
//! Ignored by default; run with `cargo test -p bss-ledger -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::error::AuthZResolverError;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use bss_ledger::api::rest::audit::{ApiState, router};
use bss_ledger::infra::audit::retrieval::AuditRetrievalReader;
use bss_ledger::infra::authz::cross_tenant::CrossTenantGateway;
use bss_ledger::infra::inquiry::AuditPackExporter;
use bss_ledger::infra::pii::ErasureService;
use bss_ledger::infra::storage::migrations::Migrator;
use sea_orm::Database;
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::{SecurityContext, pep_properties};
use tower::ServiceExt;
use uuid::Uuid;

/// Flat-`In` PDP fake authorizing one or more tenants (mirrors the unit-test
/// `audit_tests::FlatInResolver`): an authorized tenant's gate passes; any other
/// `targetScope` is outside the returned `In` constraint, so the cross-tenant
/// role check resolves `Ok(false)` → `CROSS_TENANT_ACCESS_DENIED`.
struct FlatInResolver {
    allowed: Vec<Uuid>,
}

#[async_trait]
impl AuthZResolverClient for FlatInResolver {
    async fn evaluate(
        &self,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        self.allowed.clone(),
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

fn enforcer_allowing(tenant: Uuid) -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(FlatInResolver {
        allowed: vec![tenant],
    }))
}

/// PDP fake authorizing several tenants (the caller's home plus a cross-tenant
/// target), so the cross-tenant role check passes and a downstream gate (e.g. a
/// missing reason) is what rejects.
fn enforcer_allowing_many(tenants: Vec<Uuid>) -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(FlatInResolver { allowed: tenants }))
}

fn ctx_for(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type("gts.cf.core.security.subject_user.v1~")
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("authed SecurityContext must build")
}

/// Boot a container, migrate, and build the audit router with a real `ApiState`
/// over the migrated DB plus the given enforcer + caller context.
fn audit_router(
    provider: DBProvider<DbError>,
    enforcer: PolicyEnforcer,
    ctx: SecurityContext,
) -> Router {
    let state = Arc::new(ApiState {
        reader: AuditRetrievalReader::new(provider.clone()),
        gateway: CrossTenantGateway::new(),
        exporter: AuditPackExporter::new(provider.clone()),
        erasure: ErasureService::new(),
        db: provider,
    });
    let openapi = OpenApiRegistryImpl::new();
    router(state, &openapi)
        .layer(axum::Extension(enforcer))
        .layer(axum::Extension(ctx))
}

async fn provider_for(url: &str) -> DBProvider<DbError> {
    let raw = Database::connect(url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    DBProvider::<DbError>::new(tdb)
}

async fn body_string(response: axum::http::Response<Body>) -> String {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// (1) A cross-tenant tamper-status read by a caller NOT entitled to the target
/// ⇒ 403 `CROSS_TENANT_ACCESS_DENIED` at the REST seam (the BOLA deny path).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_tamper_status_denied_returns_403() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let provider = provider_for(&url).await;

    let home = Uuid::now_v7();
    let target = Uuid::now_v7();
    // The enforcer authorizes ONLY the home tenant: the home audit_read gate
    // passes, but the cross-tenant role check for `target` resolves false.
    let router = audit_router(provider, enforcer_allowing(home), ctx_for(home));

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/bss-ledger/v1/ledger/audit/tamper-status?target_scope={target}&reason_code=DISPUTE"
                ))
                .header("X-Investigation-Reason", "investigate")
                .body(Body::empty())
                .expect("build req"),
        )
        .await
        .expect("send");

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a caller not entitled to the target tenant must be denied"
    );
    let body = body_string(response).await;
    assert!(
        body.contains("CROSS_TENANT_ACCESS_DENIED"),
        "the problem body must carry CROSS_TENANT_ACCESS_DENIED; got {body}"
    );
}

/// (2) A cross-tenant erasure whose caller IS authorized for the target but
/// omits the investigation reason ⇒ 400 `MISSING_INVESTIGATION_REASON`, rejected
/// before any read/write. The caller must be authorized for the target so the
/// role check passes first (§5: role BEFORE reason — see the 403 test below).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_erasure_without_reason_returns_400() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let provider = provider_for(&url).await;

    let home = Uuid::now_v7();
    let target = Uuid::now_v7();
    let payer = Uuid::now_v7();
    // The enforcer authorizes BOTH the home tenant (the erase gate) and the
    // target (the cross-tenant role check), so the role gate passes and the
    // missing X-Investigation-Reason header is what rejects, pre-read.
    let router = audit_router(
        provider,
        enforcer_allowing_many(vec![home, target]),
        ctx_for(home),
    );

    let body = serde_json::json!({
        "payer_tenant_id": payer,
        "target_scope": target,
    })
    .to_string();
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bss-ledger/v1/ledger/audit/erasure")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build req"),
        )
        .await
        .expect("send");

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an authorized cross-tenant erasure without a reason must be rejected pre-read"
    );
    let body = body_string(response).await;
    assert!(
        body.contains("MISSING_INVESTIGATION_REASON"),
        "the problem body must carry MISSING_INVESTIGATION_REASON; got {body}"
    );
}

/// (3) A cross-tenant erasure whose caller is NOT entitled to the TARGET tenant
/// ⇒ 403 `CROSS_TENANT_ACCESS_DENIED`. The role is checked BEFORE the reason
/// (§5), so an unauthorized caller is denied even with no reason — it never
/// learns whether a reason would have sufficed. Guards the role-first ordering
/// that the shared `resolve_action_scope` enforces.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_erasure_unauthorized_returns_403() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let provider = provider_for(&url).await;

    let home = Uuid::now_v7();
    let target = Uuid::now_v7();
    let payer = Uuid::now_v7();
    // Only the home tenant is authorized; the target is outside the caller's
    // compiled scope, so the cross-tenant role check fails. No reason is sent,
    // proving role is evaluated first.
    let router = audit_router(provider, enforcer_allowing(home), ctx_for(home));

    let body = serde_json::json!({
        "payer_tenant_id": payer,
        "target_scope": target,
    })
    .to_string();
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bss-ledger/v1/ledger/audit/erasure")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("build req"),
        )
        .await
        .expect("send");

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "an unauthorized cross-tenant erasure must be denied on the role, not the reason"
    );
    let body = body_string(response).await;
    assert!(
        body.contains("CROSS_TENANT_ACCESS_DENIED"),
        "the problem body must carry CROSS_TENANT_ACCESS_DENIED; got {body}"
    );
}

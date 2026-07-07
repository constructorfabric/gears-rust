//! Postgres-only integration tests for the Slice 7 Phase 2 exception queue:
//! `ExceptionRouter` (additive, period-bound, deduped routing) + `ExceptionQueueRepo`
//! (list / read / resolve) + the close-gate consumption of OPEN rows (incl. the
//! `GL_WRITEOFF_VARIANCE` acknowledge-to-non-block path). Ignored by default; run
//! with `cargo test -p bss-ledger --test postgres_exception -- --ignored`.

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

use bss_ledger::domain::error::DomainError;
use bss_ledger::domain::exception::ExceptionType;
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::exception::ExceptionRouter;
use bss_ledger::infra::period_close::PeriodCloseService;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ExceptionQueueRepo;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

/// Boot + migrate; seed USD@2 + ONE OPEN fiscal period for `tenant` (LE = tenant).
/// Returns the raw conn, the provider, and the tenant id.
async fn setup(url: &str) -> (DatabaseConnection, DBProvider<DbError>, Uuid) {
    let raw = Database::connect(url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();

    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);

    let tenant = Uuid::now_v7();
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status) \
         VALUES ('{tenant}','{tenant}','202606','UTC','OPEN')"
    )))
    .await
    .unwrap();
    (raw, provider, tenant)
}

/// Count exception rows for `(tenant, type)` in a given status.
async fn count_rows(
    raw: &DatabaseConnection,
    tenant: Uuid,
    exception_type: &str,
    status: &str,
) -> i64 {
    let row = raw
        .query_one(pg(format!(
            "SELECT count(*) AS c FROM bss.ledger_exception_queue \
             WHERE tenant_id='{tenant}' AND exception_type='{exception_type}' AND status='{status}'"
        )))
        .await
        .unwrap()
        .expect("count row");
    row.try_get::<i64>("", "c").unwrap()
}

/// `ExceptionRouter::route` opens ONE OPEN row, period-bound to the current OPEN
/// period; a second route on the same `(type, business_ref)` dedups (still ONE row).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn route_opens_one_period_bound_row_and_dedups() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let (raw, provider, tenant) = setup(&url).await;
    let router = ExceptionRouter::new(provider);

    router
        .route(tenant, ExceptionType::ReconMismatch, "ref-1", None)
        .await;
    assert_eq!(
        count_rows(&raw, tenant, "RECON_MISMATCH", "OPEN").await,
        1,
        "the first route opens exactly one OPEN row"
    );

    // The row is bound to the current OPEN period (so the close gate sees it).
    let period = raw
        .query_one(pg(format!(
            "SELECT period_id FROM bss.ledger_exception_queue \
             WHERE tenant_id='{tenant}' AND exception_type='RECON_MISMATCH'"
        )))
        .await
        .unwrap()
        .expect("row")
        .try_get::<Option<String>>("", "period_id")
        .unwrap();
    assert_eq!(period.as_deref(), Some("202606"), "row is period-bound");

    // A second route on the same business key dedups — still exactly one OPEN row.
    router
        .route(tenant, ExceptionType::ReconMismatch, "ref-1", None)
        .await;
    assert_eq!(
        count_rows(&raw, tenant, "RECON_MISMATCH", "OPEN").await,
        1,
        "a re-route on the same (type, business_ref) does not duplicate the OPEN row"
    );
}

/// `resolve_one` clears an OPEN row (OPEN → RESOLVED); `list` filters by status.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn resolve_transitions_and_list_filters() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let (_raw, provider, tenant) = setup(&url).await;
    let scope = AccessScope::for_tenant(tenant);
    let router = ExceptionRouter::new(provider.clone());
    let repo = ExceptionQueueRepo::new(provider);

    router
        .route(tenant, ExceptionType::SplitAmbiguous, "cn-1", None)
        .await;
    let open = repo.list(&scope, tenant, Some("OPEN")).await.unwrap();
    assert_eq!(open.len(), 1, "one OPEN row listed");
    let id = open[0].exception_id;

    repo.resolve_one(&scope, tenant, id, "RESOLVED", "operator-1")
        .await
        .unwrap();

    assert!(
        repo.list(&scope, tenant, Some("OPEN"))
            .await
            .unwrap()
            .is_empty(),
        "no OPEN rows after resolve"
    );
    let resolved = repo.list(&scope, tenant, Some("RESOLVED")).await.unwrap();
    assert_eq!(resolved.len(), 1, "the row is now RESOLVED");
    assert_eq!(resolved[0].resolved_by.as_deref(), Some("operator-1"));
}

/// An OPEN exception blocks close; a `GL_WRITEOFF_VARIANCE` acknowledged to
/// `APPROVED_EXCEPTION` does NOT block (the one acknowledge-to-non-block kind).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn gl_writeoff_approved_exception_does_not_block_close() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let (raw, provider, tenant) = setup(&url).await;
    let scope = AccessScope::for_tenant(tenant);
    let router = ExceptionRouter::new(provider.clone());
    let repo = ExceptionQueueRepo::new(provider.clone());
    let close = PeriodCloseService::new(
        provider,
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(bss_ledger::infra::audit::secured_audit_sink::NoopSecuredAuditSink::new()),
    );

    // A GL-writeoff exception, OPEN → blocks close.
    router
        .route(tenant, ExceptionType::GlWriteoffVariance, "gl-1", None)
        .await;
    let id = repo.list(&scope, tenant, Some("OPEN")).await.unwrap()[0].exception_id;
    let err = close
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            "202606".to_owned(),
        )
        .await
        .expect_err("an OPEN GL-writeoff exception blocks close");
    assert!(
        matches!(err, DomainError::PeriodCloseBlocked(_)),
        "got {err:?}"
    );

    // Finance acknowledges it → APPROVED_EXCEPTION; it no longer blocks close
    // (there are no books, so the tie-out is clean and the close now succeeds).
    repo.resolve_one(&scope, tenant, id, "APPROVED_EXCEPTION", "finance-1")
        .await
        .unwrap();
    let outcome = close
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            "202606".to_owned(),
        )
        .await
        .expect("an APPROVED_EXCEPTION GL-writeoff does not block close");
    assert!(!outcome.already_closed, "the period closes cleanly");
    let status = raw
        .query_one(pg(format!(
            "SELECT status FROM bss.ledger_fiscal_period \
             WHERE tenant_id='{tenant}' AND period_id='202606'"
        )))
        .await
        .unwrap()
        .expect("period row")
        .try_get::<String>("", "status")
        .unwrap();
    assert_eq!(status, "CLOSED");
}

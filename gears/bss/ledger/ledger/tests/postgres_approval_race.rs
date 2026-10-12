//! Postgres-only race test for the dual-control active-uniqueness recovery
//! (DC13): concurrent `gate` calls for the same `(tenant, kind, business_key)`
//! and the same intent all resolve to ONE pending approval. Losers either see
//! the winner on their `read_active` probe or lose the partial-unique insert,
//! exhaust the conflict retries and recover by re-reading the winner (then
//! `ensure_same_intent`). Ignored by default; run with
//! `cargo test -p cf-gears-bss-ledger --test postgres_approval_race -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bss_ledger::domain::approval::ApprovalKind;
use bss_ledger::domain::approval::intent::{ApprovalIntent, CreditGrantIntent};
use bss_ledger::domain::approval::policy::OperationFacts;
use bss_ledger::domain::error::DomainError;
use bss_ledger::domain::ports::metrics::NoopLedgerMetrics;
use bss_ledger::infra::approval::service::{ApprovalExecutor, ApprovalService};
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use rust_decimal::Decimal;
use sea_orm::Database;
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

async fn boot() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    DBProvider<DbError>,
) {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    (container, DBProvider::<DbError>::new(tdb))
}

#[derive(Clone, Default)]
struct CountingExecutor {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ApprovalExecutor for CountingExecutor {
    async fn execute(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _intent: &ApprovalIntent,
    ) -> Result<(), DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn ctx_for(subject: Uuid, tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .subject_type(gts_id!("cf.core.security.subject_user.v1~"))
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("authed SecurityContext must build")
}

fn usd(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

/// Eight concurrent over-threshold gates for one intent: every caller gets the
/// same approval id, and exactly one active approval exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Docker (testcontainers)"]
async fn concurrent_gates_for_one_intent_resolve_to_one_pending_approval() {
    let (_c, provider) = boot().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let exec = CountingExecutor::default();
    let svc = Arc::new(ApprovalService::new(
        provider.clone(),
        Arc::new(exec.clone()),
        Arc::new(NoopLedgerMetrics),
        bss_ledger::config::FxConfig::default(),
    ));
    let intent = ApprovalIntent::CreditGrant(CreditGrantIntent {
        tenant_id: tenant,
        payer_tenant_id: Uuid::now_v7(),
        credit_application_id: "CA-RACE".to_owned(),
        amount: usd(500_000),
        credit_grant_event_type: Some("promo".to_owned()),
    });
    let facts = OperationFacts {
        kind: ApprovalKind::CreditGrant,
        amount: Some(usd(500_000)),
        effective_at: None,
        has_outstanding_balance: false,
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let svc = svc.clone();
        let scope = scope.clone();
        let intent = intent.clone();
        let facts = facts.clone();
        let barrier = barrier.clone();
        let ctx = ctx_for(Uuid::now_v7(), tenant);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            svc.gate(&ctx, &scope, intent, facts, "race".to_owned())
                .await
        }));
    }
    let mut ids = Vec::new();
    for handle in handles {
        let id = handle
            .await
            .unwrap()
            .expect("every racer resolves to the winner, never an error")
            .expect("over threshold: a pending approval");
        ids.push(id);
    }
    ids.dedup();
    assert_eq!(
        ids.len(),
        1,
        "all racers must share one approval id: {ids:?}"
    );
    let reader = ctx_for(Uuid::now_v7(), tenant);
    let rows = svc.list(&reader, &scope, None, None).await.unwrap();
    assert_eq!(rows.len(), 1, "exactly one approval row");
    assert_eq!(rows[0].approval_id, ids[0]);
    assert_eq!(rows[0].state, "PENDING");
    assert_eq!(exec.calls.load(Ordering::SeqCst), 0);
}

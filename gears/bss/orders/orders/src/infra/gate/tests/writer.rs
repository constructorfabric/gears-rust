//! S3-10: the gate diagnostics writer on real PostgreSQL (production migrations).
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use crate::config::MaintenanceTask;
use crate::domain::gate::predicates::PredicateId;
use crate::domain::gate::vector::{Applicability, Outcome, OutcomeKey, Vector, Verdict};
use crate::infra::gate::writer::{RunRecord, RunTarget, write_preview_run, write_run};
use crate::infra::maintenance::scope::{discover_clock, discover_retention};
use crate::infra::maintenance::{MaintenanceAuthority, RetentionTable, ServiceActor, TargetScope};
use crate::infra::storage::entity::{self, gate_outcome};
use crate::infra::storage::{migrations, repo};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ColumnTrait, ConnectionTrait, Database, DatabaseConnection, DbBackend, EntityTrait,
    QueryFilter, Statement,
};
use testcontainers_modules::{postgres::Postgres, testcontainers::ContainerAsync};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError, SecureEntityExt, secure_insert_many};
use toolkit_db::{ConnectOpts, Db, connect_db};
use uuid::Uuid;

/// The shared Orders container start (bounded port-publication retry and TCP readiness).
#[path = "../../storage/tests/pg_container.rs"]
#[allow(
    clippy::duplicate_mod,
    reason = "test-only harness file shared by private test modules that cannot reach each other"
)]
mod pg_container;

const SUBJECT: &str = "subject-40";

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
const TENANT: u128 = 10;
const OTHER_TENANT: u128 = 11;

struct Pg {
    db: Db,
    raw: DatabaseConnection,
    port: u16,
    _container: ContainerAsync<Postgres>,
}
impl Pg {
    async fn new() -> anyhow::Result<Self> {
        let (container, port) = pg_container::start_postgres().await?;
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let db = connect_db(&url, ConnectOpts::default()).await?;
        let raw = Database::connect(url).await?;
        toolkit_db::migration_runner::run_migrations_for_testing(&db, migrations::all()).await?;
        Ok(Self {
            db,
            raw,
            port,
            _container: container,
        })
    }
    /// Rows of one run, read with the privileged connection (no scope).
    async fn count(&self, run_id: Uuid) -> anyhow::Result<i64> {
        self.scalar(&format!(
            "SELECT count(*) AS n FROM bss_orders__gate_outcome WHERE run_id='{run_id}'"
        ))
        .await
    }
    async fn scalar(&self, sql: &str) -> anyhow::Result<i64> {
        Ok(self
            .raw
            .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await?
            .unwrap()
            .try_get("", "n")?)
    }
    /// A real restricted login of one Orders role class.
    async fn role(&self, class: &str) -> anyhow::Result<Db> {
        assert!(class.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        self.raw
            .execute_unprepared(&format!(
                "CREATE ROLE test_{class} LOGIN PASSWORD 'fixture' INHERIT; \
                 GRANT bss_orders_{class} TO test_{class}"
            ))
            .await?;
        let url = format!(
            "postgres://test_{class}:fixture@127.0.0.1:{}/postgres",
            self.port
        );
        Ok(connect_db(&url, ConnectOpts::default()).await?)
    }
}

/// The database refusal inside an error chain: SQLSTATE and violated constraint.
fn refusal(error: &anyhow::Error) -> (Option<String>, Option<String>) {
    use sea_orm::{DbErr, RuntimeErr};
    let db_err = error.chain().find_map(|cause| {
        cause
            .downcast_ref::<DbErr>()
            .or_else(|| match cause.downcast_ref::<ScopeError>() {
                Some(ScopeError::Db(e)) => Some(e),
                _ => None,
            })
            .or_else(|| match cause.downcast_ref::<toolkit_db::DbError>() {
                Some(toolkit_db::DbError::Sea(e)) => Some(e),
                _ => None,
            })
    });
    let Some(
        DbErr::Exec(RuntimeErr::SqlxError(e))
        | DbErr::Query(RuntimeErr::SqlxError(e))
        | DbErr::Conn(RuntimeErr::SqlxError(e)),
    ) = db_err
    else {
        return (None, None);
    };
    let Some(db) = e.as_database_error() else {
        return (None, None);
    };
    (
        db.code().map(std::borrow::Cow::into_owned),
        db.constraint().map(str::to_owned),
    )
}

fn key(
    predicate: PredicateId,
    line: Option<u8>,
    item: Option<u8>,
    has_selection: bool,
) -> OutcomeKey {
    OutcomeKey {
        predicate,
        line_id: line.map(|b| Uuid::from_bytes([b; 16])),
        item_id: item.map(|b| Uuid::from_bytes([b; 16])),
        selection: if has_selection {
            CatalogSelection::SelectedNull
        } else {
            CatalogSelection::NotSelected
        },
    }
}
fn outcome(key: OutcomeKey, verdict: Verdict, reason: Option<Reason>) -> Outcome {
    Outcome {
        key,
        verdict,
        reason,
        applicability: Applicability::Required,
        producer_results: vec![],
        upstream_detail: None,
    }
}
/// A mixed run: an order-level pass and a line-level failure.
fn mixed() -> Vector {
    Vector::build(vec![
        outcome(
            key(PredicateId::AxisValidity, None, None, false),
            Verdict::Passed,
            None,
        ),
        outcome(
            key(PredicateId::QuantityFloor, Some(1), None, false),
            Verdict::Failed,
            Some(Reason::QuantityBelowFloor),
        ),
    ])
    .unwrap()
}
fn record(
    run_id: Uuid,
    vector: &Vector,
    target: RunTarget,
    evaluated_at: OffsetDateTime,
) -> RunRecord<'_> {
    RunRecord {
        run_id,
        subject_tenant_id: u(TENANT),
        subject_id: SUBJECT,
        resource_tenant_id: u(TENANT),
        seller_tenant_id: u(20),
        payer_tenant_id: u(30),
        correlation_id: Some(u(50)),
        target,
        evaluated_at,
        vector,
    }
}
fn scope() -> AccessScope {
    AccessScope::for_tenant(u(TENANT))
}

fn order(id: u128) -> entity::order::Model {
    entity::order::Model {
        order_id: u(id),
        order_number: format!("O-{id}"),
        category: "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1".into(),
        resource_tenant_id: u(TENANT),
        audit_tenant_id: u(TENANT),
        payer_tenant_id: u(30),
        seller_tenant_id: u(20),
        initiating_actor: u(40).to_string(),
        sales_path: "self_service".into(),
        contract_id: None,
        state: "draft".into(),
        state_entered_at: OffsetDateTime::now_utc(),
        current_version: 1,
        version_allocation_high_water: 1,
        draft_revision: 0,
        pre_hold_state: None,
        resume_count: 0,
        amendment_count: 0,
        fulfillment_control_generation: 0,
        fulfillment_control_pending: None,
        spawn_signal_at: None,
        authorization_failure_tolerated_at: None,
        compensation_evidence: None,
        audit_sequence: 0,
        created_at: OffsetDateTime::now_utc(),
    }
}
fn version(id: u128) -> entity::order_version::Model {
    entity::order_version::Model {
        order_id: u(id),
        version: 1,
        supersedes_version: None,
        market_currency: None,
        market_region: None,
        payer_tenant_id: u(30),
        category: "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1".into(),
        contract_id: None,
        actor: u(40).to_string(),
        actor_tenant_id: u(TENANT),
        reason: "create".into(),
        amendment_reason: None,
        created_at: OffsetDateTime::now_utc(),
    }
}
/// An order with version 1, so a submit run can reference `(order_id, 1)`.
async fn create_order(db: &Db, id: u128) -> anyhow::Result<()> {
    let scope = AccessScope::for_resources(vec![u(id)]);
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let row = repo::insert_order(tx, &scope, order(id)).await?;
            let locked = repo::LockedOrder::acquire(tx, &scope, &row).await?;
            locked.insert_order_version(version(id)).await?;
            Ok::<(), anyhow::Error>(())
        })
    })
    .await
}
/// Run `write_run` on a fresh caller transaction that commits on success.
async fn write(db: &Db, vector: Vector, run_id: Uuid) -> anyhow::Result<u64> {
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let run = record(
                run_id,
                &vector,
                RunTarget::Preview,
                OffsetDateTime::now_utc(),
            );
            Ok::<u64, anyhow::Error>(write_run(tx, &scope(), &run).await?)
        })
    })
    .await
}

/// (1) Two component/slot results of one line are two identities, so the NULLS NOT DISTINCT
/// unique index accepts both: items 1 and 2, and on item 1 the not-selected and the selected
/// NULL keys.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn two_component_results_of_one_line_persist_as_distinct_rows() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let p = PredicateId::CatalogSlotCoverage;
    let vector = Vector::build(vec![
        outcome(key(p, Some(1), Some(1), false), Verdict::Passed, None),
        outcome(key(p, Some(1), Some(1), true), Verdict::Passed, None),
        outcome(key(p, Some(1), Some(2), false), Verdict::Passed, None),
    ])?;
    let run_id = Uuid::now_v7();
    assert_eq!(write(&pg.db, vector, run_id).await?, 3);
    assert_eq!(pg.count(run_id).await?, 3);
    assert_eq!(
        pg.scalar(&format!(
            "SELECT count(*) AS n FROM bss_orders__gate_outcome WHERE run_id='{run_id}' \
             AND has_catalog_selection AND catalog_scope_key IS NULL"
        ))
        .await?,
        1,
        "the selected NULL is stored as its own identity"
    );
    Ok(())
}

/// (2) The same identity twice in one run violates `uq_bss_orders__gate_outcome__0`, and the
/// caller's transaction persists nothing.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_same_identity_twice_in_one_run_is_a_unique_violation() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let run_id = Uuid::now_v7();
    let error = pg
        .db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let vector = mixed();
                let run = record(
                    run_id,
                    &vector,
                    RunTarget::Preview,
                    OffsetDateTime::now_utc(),
                );
                write_run(tx, &scope(), &run).await?;
                write_run(tx, &scope(), &run).await?;
                Ok::<(), anyhow::Error>(())
            })
        })
        .await
        .unwrap_err();
    assert_eq!(
        refusal(&error),
        (
            Some("23505".to_owned()),
            Some("uq_bss_orders__gate_outcome__0".to_owned())
        )
    );
    assert_eq!(pg.count(run_id).await?, 0);
    Ok(())
}

/// A row of `run_id` the vector can never produce: `failed` without a reason.
fn failed_without_reason(run_id: Uuid) -> gate_outcome::ActiveModel {
    let vector = Vector::build(vec![outcome(
        key(PredicateId::ContractActive, None, None, false),
        Verdict::Passed,
        None,
    )])
    .unwrap();
    let mut row = repo::gate_outcome::rows(&record(
        run_id,
        &vector,
        RunTarget::Preview,
        OffsetDateTime::now_utc(),
    ))
    .unwrap()
    .remove(0);
    row.verdict = Set("failed".to_owned());
    row.reason = Set(None);
    row
}

/// (3) `verdict='failed'` with `reason` NULL violates `ck_bss_orders__gate_outcome__2`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn failed_without_reason_violates_the_reason_check() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let run_id = Uuid::now_v7();
    let error = pg
        .db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                secure_insert_many::<gate_outcome::Entity>(
                    vec![failed_without_reason(run_id)],
                    &scope(),
                    tx,
                )
                .await?;
                Ok::<(), anyhow::Error>(())
            })
        })
        .await
        .unwrap_err();
    assert_eq!(
        refusal(&error),
        (
            Some("23514".to_owned()),
            Some("ck_bss_orders__gate_outcome__2".to_owned())
        )
    );
    assert_eq!(pg.count(run_id).await?, 0);
    Ok(())
}

/// (4) A Preview run writes `order_id`/`version` NULL in its own committed transaction and
/// returns the run id; a Preview record carrying an order is refused before any write.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_preview_run_commits_without_an_order_and_returns_its_id() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let vector = mixed();
    let run_id = Uuid::now_v7();
    let run = record(
        run_id,
        &vector,
        RunTarget::Preview,
        OffsetDateTime::now_utc(),
    );
    assert_eq!(write_preview_run(&pg.db, &scope(), &run).await?, run_id);
    assert_eq!(pg.count(run_id).await?, 2);
    assert_eq!(
        pg.scalar(&format!(
            "SELECT count(*) AS n FROM bss_orders__gate_outcome WHERE run_id='{run_id}' \
             AND order_id IS NULL AND version IS NULL AND mapping_version='{}'",
            crate::domain::gate::MAPPING_VERSION
        ))
        .await?,
        2
    );
    let with_order = record(
        Uuid::now_v7(),
        &vector,
        RunTarget::Order {
            order_id: u(1),
            version: Some(1),
        },
        OffsetDateTime::now_utc(),
    );
    assert!(matches!(
        write_preview_run(&pg.db, &scope(), &with_order).await,
        Err(ScopeError::Invalid(_))
    ));
    assert_eq!(pg.count(with_order.run_id).await?, 0);
    Ok(())
}

/// (5) A failure mid-batch leaves zero rows of the run: an invalid row last in one batch
/// statement, and an invalid row after a complete `write_run` on the same caller transaction.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_failure_mid_batch_leaves_no_row_of_the_run() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let batch_run = Uuid::now_v7();
    let error = pg
        .db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let vector = mixed();
                let mut batch = repo::gate_outcome::rows(&record(
                    batch_run,
                    &vector,
                    RunTarget::Preview,
                    OffsetDateTime::now_utc(),
                ))?;
                batch.push(failed_without_reason(batch_run));
                secure_insert_many::<gate_outcome::Entity>(batch, &scope(), tx).await?;
                Ok::<(), anyhow::Error>(())
            })
        })
        .await
        .unwrap_err();
    assert_eq!(
        refusal(&error).1.as_deref(),
        Some("ck_bss_orders__gate_outcome__2")
    );
    assert_eq!(pg.count(batch_run).await?, 0);

    let caller_run = Uuid::now_v7();
    let error = pg
        .db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let vector = mixed();
                let run = record(
                    caller_run,
                    &vector,
                    RunTarget::Preview,
                    OffsetDateTime::now_utc(),
                );
                assert_eq!(write_run(tx, &scope(), &run).await?, 2);
                repo::private::insert_gate_outcome(
                    tx,
                    &scope(),
                    gate_outcome::Model {
                        outcome_id: Uuid::now_v7(),
                        run_id: caller_run,
                        subject_tenant_id: u(TENANT),
                        subject_id: SUBJECT.to_owned(),
                        resource_tenant_id: u(TENANT),
                        seller_tenant_id: u(20),
                        payer_tenant_id: u(30),
                        correlation_id: None,
                        order_id: None,
                        version: None,
                        line_id: None,
                        predicate: PredicateId::ContractActive.token().to_owned(),
                        item_id: None,
                        has_catalog_selection: false,
                        catalog_scope_key: None,
                        verdict: "failed".to_owned(),
                        reason: None,
                        mapping_version: crate::domain::gate::MAPPING_VERSION.to_owned(),
                        applicability: "required".to_owned(),
                        producer_results: serde_json::json!([]),
                        upstream_detail: None,
                        evaluated_at: OffsetDateTime::now_utc(),
                    },
                )
                .await?;
                Ok::<(), anyhow::Error>(())
            })
        })
        .await
        .unwrap_err();
    assert_eq!(refusal(&error).0.as_deref(), Some("23514"));
    assert_eq!(pg.count(caller_run).await?, 0);
    Ok(())
}

/// (6) Rows are scoped by `subject_tenant_id`: another tenant's scope reads nothing and cannot
/// write the run either.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn another_tenants_scope_sees_nothing() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let run_id = Uuid::now_v7();
    assert_eq!(write(&pg.db, mixed(), run_id).await?, 2);
    let read = |scope: AccessScope| {
        pg.db.transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let found = gate_outcome::Entity::find()
                    .filter(gate_outcome::Column::RunId.eq(run_id))
                    .secure()
                    .scope_with(&scope)
                    .all(tx)
                    .await?;
                Ok::<usize, anyhow::Error>(found.len())
            })
        })
    };
    assert_eq!(read(scope()).await?, 2);
    assert_eq!(read(AccessScope::for_tenant(u(OTHER_TENANT))).await?, 0);
    let foreign = pg
        .db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let vector = mixed();
                let run = record(
                    Uuid::now_v7(),
                    &vector,
                    RunTarget::Preview,
                    OffsetDateTime::now_utc(),
                );
                write_run(tx, &AccessScope::for_tenant(u(OTHER_TENANT)), &run).await?;
                Ok::<(), anyhow::Error>(())
            })
        })
        .await;
    let denied = foreign.as_ref().err().is_some_and(|error| {
        error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<ScopeError>(),
                Some(ScopeError::TenantNotInScope { .. } | ScopeError::Denied(_))
            )
        })
    });
    assert!(
        denied,
        "a row outside the scope is never written: {foreign:?}"
    );
    Ok(())
}

/// (7) The 7-day Preview purge: rows 8 days old with `order_id` NULL are deleted by
/// `retention::preview_diagnostics` under the real retention login; order-linked rows and
/// 6-day-old Preview rows are kept.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn preview_rows_older_than_seven_days_are_purged_and_others_kept() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    create_order(&pg.db, 1).await?;
    let now = OffsetDateTime::now_utc();
    let expired = Uuid::now_v7();
    let live = Uuid::now_v7();
    let linked = Uuid::now_v7();
    pg.db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                let vector = mixed();
                for (run_id, order, age) in [
                    (expired, RunTarget::Preview, time::Duration::days(8)),
                    (live, RunTarget::Preview, time::Duration::days(6)),
                    (
                        linked,
                        RunTarget::Order {
                            order_id: u(1),
                            version: Some(1),
                        },
                        time::Duration::days(8),
                    ),
                ] {
                    write_run(tx, &scope(), &record(run_id, &vector, order, now - age)).await?;
                }
                Ok::<(), anyhow::Error>(())
            })
        })
        .await?;
    let retention_db = pg.role("retention").await?;
    let authority = MaintenanceAuthority::configured(
        ServiceActor::configured(u(900), u(901)).unwrap(),
        [MaintenanceTask::RetentionPurge],
    );
    let grant = authority.grant(MaintenanceTask::RetentionPurge).unwrap();
    let cutoff = discover_clock(&retention_db, &grant).await? - time::Duration::days(7);
    let batch = discover_retention(
        &retention_db,
        &grant,
        RetentionTable::PreviewDiagnostics,
        cutoff,
        100,
    )
    .await?;
    let target = TargetScope::from_discovered_retention(&batch);
    let purged = retention_db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                Ok::<u64, anyhow::Error>(repo::retention::preview_diagnostics(tx, &target).await?)
            })
        })
        .await?;
    assert_eq!(purged, 2);
    assert_eq!(pg.count(expired).await?, 0);
    assert_eq!(pg.count(live).await?, 2);
    assert_eq!(pg.count(linked).await?, 2);
    Ok(())
}

/// Identical vectors in concurrent Preview executions are separate committed diagnostic runs.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn concurrent_identical_preview_vectors_persist_two_runs() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let vector = mixed();
    let now = OffsetDateTime::now_utc();
    let a = record(Uuid::now_v7(), &vector, RunTarget::Preview, now);
    let b = record(Uuid::now_v7(), &vector, RunTarget::Preview, now);
    let scope = scope();
    let (first, second) = tokio::join!(
        write_preview_run(&pg.db, &scope, &a),
        write_preview_run(&pg.db, &scope, &b),
    );
    assert_eq!(first?, a.run_id);
    assert_eq!(second?, b.run_id);
    assert_ne!(a.run_id, b.run_id);
    assert_eq!(pg.count(a.run_id).await?, 2);
    assert_eq!(pg.count(b.run_id).await?, 2);
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order")
            .await?,
        0
    );
    Ok(())
}

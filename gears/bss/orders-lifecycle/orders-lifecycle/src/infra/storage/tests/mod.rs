//! Real PostgreSQL tests of the production migrations and repositories.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::{entity, migrations, repo};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use testcontainers_modules::{postgres::Postgres, testcontainers::ContainerAsync};
use toolkit_db::{ConnectOpts, Db, connect_db};
use uuid::Uuid;

/// The shared Orders container start (bounded port-publication retry and TCP readiness).
#[path = "../../../../tests/capability_support/pg_container.rs"]
#[allow(
    clippy::duplicate_mod,
    reason = "test-only harness file shared by private test modules that cannot reach each other"
)]
mod pg_container;

struct Pg {
    db: Db,
    raw: DatabaseConnection,
    port: u16,
    _container: ContainerAsync<Postgres>,
}
impl Pg {
    async fn new() -> anyhow::Result<Self> {
        let pg = Self::bare().await?;
        toolkit_db::migration_runner::run_migrations_for_testing(&pg.db, migrations::all()).await?;
        Ok(pg)
    }
    async fn bare() -> anyhow::Result<Self> {
        let (container, port) = pg_container::start_postgres().await?;
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let db = connect_db(&url, ConnectOpts::default()).await?;
        let raw = Database::connect(url).await?;
        Ok(Self {
            db,
            raw,
            port,
            _container: container,
        })
    }
    async fn sql(&self, sql: &str) -> anyhow::Result<()> {
        self.raw.execute_unprepared(sql).await?;
        Ok(())
    }
    async fn scalar(&self, sql: &str) -> anyhow::Result<i64> {
        Ok(self
            .raw
            .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await?
            .unwrap()
            .try_get("", "n")?)
    }
    async fn role(&self, class: &str) -> anyhow::Result<(Db, DatabaseConnection)> {
        assert!(class.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        self.sql(&format!("CREATE ROLE test_{class} LOGIN PASSWORD 'fixture' INHERIT; GRANT bss_orders_{class} TO test_{class}")).await?;
        let url = format!(
            "postgres://test_{class}:fixture@127.0.0.1:{}/postgres",
            self.port
        );
        Ok((
            connect_db(&url, ConnectOpts::default()).await?,
            Database::connect(url).await?,
        ))
    }
}
fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap()
}
fn order(id: u128) -> entity::order::Model {
    entity::order::Model {
        order_id: u(id),
        order_number: format!("O-{id}"),
        category: "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1".into(),
        resource_tenant_id: u(10),
        audit_tenant_id: u(10),
        payer_tenant_id: u(30),
        seller_tenant_id: u(20),
        initiating_actor: u(40).to_string(),
        sales_path: "self_service".into(),
        contract_id: None,
        state: "draft".into(),
        state_entered_at: now(),
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
        created_at: now(),
    }
}
fn version(id: u128, n: i32, previous: Option<i32>) -> entity::order_version::Model {
    entity::order_version::Model {
        order_id: u(id),
        version: n,
        supersedes_version: previous,
        market_currency: None,
        market_region: None,
        payer_tenant_id: u(30),
        category: "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1".into(),
        contract_id: None,
        actor: u(40).to_string(),
        actor_tenant_id: u(10),
        reason: if n == 1 { "create" } else { "submit" }.into(),
        amendment_reason: None,
        created_at: now(),
    }
}
async fn create(db: &Db, id: u128) -> anyhow::Result<()> {
    let scope = toolkit_security::AccessScope::for_resources(vec![u(id)]);

    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            let row = repo::insert_order(tx, &scope, order(id)).await?;
            let locked = repo::LockedOrder::acquire(tx, &scope, &row).await?;
            locked.insert_order_version(version(id, 1, None)).await?;
            Ok(())
        })
    })
    .await
}

/// Discover one audit namespace through the real read-only maintenance discovery (D-184).
async fn discovered_namespace(
    db: &Db,
    namespace: Uuid,
) -> anyhow::Result<crate::infra::maintenance::TargetScope> {
    use crate::infra::maintenance::{MaintenanceAuthority, MaintenanceTask, ServiceActor, scope};
    let authority = MaintenanceAuthority::configured(
        ServiceActor::configured(u(900), u(901)).unwrap(),
        [MaintenanceTask::AuditCheckpoint],
    );
    let grant = authority.grant(MaintenanceTask::AuditCheckpoint).unwrap();
    let found = scope::discover_audit_namespaces(db, &grant, None, 10).await?;
    let target = found
        .iter()
        .map(crate::infra::maintenance::TargetScope::from_discovered_audit_namespace)
        .find(|t| t.audit_namespace() == Some(namespace))
        .ok_or_else(|| anyhow::anyhow!("namespace not discovered"))?;
    Ok(target)
}

/// Private evidence writers accept only a transaction runner; append one audit row atomically.
async fn audit_tx(
    db: &Db,
    scope: &toolkit_security::AccessScope,
    row: entity::transition_audit::Model,
) -> anyhow::Result<entity::transition_audit::Model> {
    let scope = scope.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move { Ok(repo::private::insert_transition_audit(tx, &scope, row).await?) })
    })
    .await
}

#[tokio::test]
async fn empty_install_has_exact_inventory_and_no_evidence_cascades() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM pg_tables WHERE schemaname='public' AND tablename LIKE 'bss_orders__%'").await?,24);
    assert_eq!(pg.scalar("SELECT count(*) AS n FROM pg_constraint WHERE contype='f' AND conrelid IN (SELECT oid FROM pg_class WHERE relname LIKE 'bss_orders__%') AND confdeltype='c'").await?,0);
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__state_ttl_policy WHERE scope='platform'")
            .await?,
        5
    );
    assert_eq!(
        pg.scalar(
            "SELECT count(*) AS n FROM bss_orders__date_policy WHERE resource_tenant_id IS NULL"
        )
        .await?,
        1
    );
    // Re-running the exposed complete set is forward/idempotent, never a destructive down.
    toolkit_db::migration_runner::run_migrations_for_testing(&pg.db, migrations::all()).await?;
    let (runtime, _) = pg.role("runtime").await?;
    create(&runtime, 1).await?;
    assert_eq!(
        pg.scalar("SELECT count(*) AS n FROM bss_orders__order_version")
            .await?,
        1
    );
    Ok(())
}
mod audit;
mod authz;
mod claims;
mod constraints;
mod executions;
mod idempotency;
mod maintenance;
#[path = "migrations.rs"]
mod migration_tests;
mod policies_retention;
mod repositories;
mod roles;
mod workers;

mod capture;
mod dates;
mod engine;
mod events;
mod platform;
mod reads;
mod throttling;

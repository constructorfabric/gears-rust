//! Shared seed / read helpers for the AM real-DB integration suite.
//!
//! Every helper drives `SecureORM` `ActiveModel.insert(...)` /
//! `SecureDeleteExt` against an `Arc<AmDbProvider>`. No raw SQL, no
//! `Statement`, no `DbBackend` — the tests target the production
//! `secure-`shaped surface end-to-end.
//!
//! Two backends are wired:
//!
//! * `setup_sqlite` — in-memory `SQLite`, always available. The
//!   migration set is FK-free (`modkit-db` does not enable
//!   `PRAGMA foreign_keys`), so anomalous shapes (orphans, cycles,
//!   dangling closure rows) are reachable via plain `SecureORM`
//!   inserts without DDL acrobatics.
//! * [`pg::bring_up_postgres`] — real Postgres via `testcontainers`,
//!   gated behind `#[cfg(feature = "postgres")]`. The Postgres schema
//!   enforces FKs, the `ux_tenants_single_root` partial unique index,
//!   and `ck_tenants_root_depth`, so seeding deliberately-broken
//!   shapes requires the DDL-bypass helpers in [`pg`]. The auxiliary
//!   `sea_orm::DatabaseConnection` exposed by [`pg::PgHarness`] is
//!   the only place `execute_unprepared(...)` runs — purely for
//!   one-time DROP CONSTRAINT / DROP INDEX statements that the FKs
//!   would otherwise block. All data-side seeding still goes through
//!   the same `SecureORM` helpers above.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use anyhow::Result;
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::secure::{SecureEntityExt, secure_insert};
use modkit_db::{ConnectOpts, connect_db};
use modkit_security::AccessScope;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use uuid::Uuid;

use account_management::Migrator;
use account_management::infra::storage::entity::{integrity_check_runs, tenant_closure, tenants};
use account_management::infra::storage::repo_impl::{AmDbProvider, TenantRepoImpl};

/// Status code constants matching `domain::tenant::model::TenantStatus`'s
/// canonical SMALLINT mapping. Hard-coded here so tests assert against
/// the wire shape without taking a runtime dep on the enum.
pub const PROVISIONING: i16 = 0;
pub const ACTIVE: i16 = 1;
pub const SUSPENDED: i16 = 2;
pub const DELETED: i16 = 3;

/// PEP-bypass scope used by every seed and every operation —
/// matches the donor's `integrity_integration.rs` harness convention.
#[must_use]
pub fn allow_all() -> AccessScope {
    AccessScope::allow_all()
}

// ---------------------------------------------------------------------
// PEP test fixture
// ---------------------------------------------------------------------
//
// The metadata + tenant service surfaces now PEP-gate every call
// through `PolicyEnforcer::access_scope_with`. Service-level
// integration tests build an `ActorAuthZResolver` that always
// permits and emits an `InTenantSubtree` constraint rooted at the
// `subject_tenant_id` carried on the caller's `SecurityContext`.
// Mirrors the lib-internal `domain::tenant::test_support::auth::MockAuthZResolver`
// — duplicated here because `#[cfg(test)]` items in the lib crate
// are not reachable from the integration-test compilation unit.

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InTenantSubtreePredicate, Predicate};
use authz_resolver_sdk::models::{
    Capability, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError, PolicyEnforcer};
use modkit_security::{SecurityContext, pep_properties};

/// Permissive PDP fake that emits a single
/// [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
/// predicate rooted at the caller's `subject.properties["tenant_id"]`.
/// AM services call `access_scope_with(... require_constraints(true))`
/// so an empty-constraint response would fail closed; the subtree
/// clamp keeps every seeded tenant visible because tests act inside
/// the root they seeded.
struct PermitWithSubtreeResolver;

#[async_trait]
impl AuthZResolverClient for PermitWithSubtreeResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let root_str = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .expect(
                "PermitWithSubtreeResolver: ctx is missing subject_tenant_id; \
                 use `ctx_for(root)` to build the SecurityContext",
            );
        let root = Uuid::parse_str(root_str).expect("subject_tenant_id must be a Uuid");
        // Emit TWO alternative constraints, OR'd together at the
        // secure-orm boundary. Each constraint carries a single
        // `InTenantSubtree` predicate keyed on a different property
        // so the compiled scope clamps every AM entity:
        //
        // * `tenant_metadata` (`tenant_col = "tenant_id"`) resolves
        //   the `OWNER_TENANT_ID` constraint.
        // * `tenants` (`resource_col = "id"`, `no_owner`) and
        //   `conversion_requests` (via `conversion_repo_scope`)
        //   resolve the `RESOURCE_ID` constraint.
        //
        // Constraint resolution is fail-closed at the FILTER level:
        // if a constraint's filter references a column the entity
        // does not declare, that constraint resolves to `None` and is
        // dropped. The OR-of-constraints semantics means the other
        // surviving constraint still clamps the read — exactly what
        // the production PDP returns when policies emit alternative
        // access paths.
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![
                    Constraint {
                        predicates: vec![Predicate::InTenantSubtree(
                            InTenantSubtreePredicate::new(pep_properties::OWNER_TENANT_ID, root),
                        )],
                    },
                    Constraint {
                        predicates: vec![Predicate::InTenantSubtree(
                            InTenantSubtreePredicate::new(pep_properties::RESOURCE_ID, root),
                        )],
                    },
                ],
                deny_reason: None,
            },
        })
    }
}

/// Build a production-shaped [`PolicyEnforcer`] with the
/// [`Capability::TenantHierarchy`] advertised so the PDP returns the
/// native `InTenantSubtree` predicate. Used by metadata + tenant
/// service integration tests.
#[must_use]
pub fn mock_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(PermitWithSubtreeResolver);
    PolicyEnforcer::new(authz).with_capabilities(vec![Capability::TenantHierarchy])
}

/// Build a [`SecurityContext`] whose `subject_tenant_id` matches the
/// supplied `root`. The `mock_enforcer` returns an
/// `InTenantSubtree(root = subject_tenant_id)` predicate, so every
/// tenant in `root`'s closure subtree is visible under the compiled
/// scope.
#[must_use]
pub fn ctx_for(root: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xCAFE))
        .subject_tenant_id(root)
        .build()
        .expect("ctx")
}

/// Bring-up output: an isolated in-memory `SQLite` DB with the AM
/// migration set applied, plus the production-shaped
/// `(provider, repo)` pair wired on top.
pub struct Harness {
    pub repo: Arc<TenantRepoImpl>,
    pub provider: Arc<AmDbProvider>,
}

/// Spin up a fresh in-memory `SQLite`, run migrations, and return
/// the production-shaped `(provider, repo)` pair.
pub async fn setup_sqlite() -> Result<Harness> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default()).await?;
    let provider: Arc<AmDbProvider> = Arc::new(AmDbProvider::new(db.clone()));
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(Harness {
        repo: Arc::new(TenantRepoImpl::new(Arc::clone(&provider))),
        provider,
    })
}

/// Insert a single tenant row, bypassing the create-tenant saga.
/// Used to seed deliberately-broken states no production happy-path
/// could produce on its own. Mirrors the donor's `common::insert_tenant`.
pub async fn insert_tenant(
    provider: &Arc<AmDbProvider>,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: &str,
    status: i16,
    self_managed: bool,
    depth: i32,
) -> Result<()> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let now = OffsetDateTime::now_utc();
    let am = tenants::ActiveModel {
        id: ActiveValue::Set(id),
        parent_id: ActiveValue::Set(parent_id),
        name: ActiveValue::Set(name.to_owned()),
        status: ActiveValue::Set(status),
        self_managed: ActiveValue::Set(self_managed),
        tenant_type_uuid: ActiveValue::Set(Uuid::nil()),
        depth: ActiveValue::Set(depth),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        deleted_at: ActiveValue::Set(None),
        retention_window_secs: ActiveValue::Set(None),
        claimed_by: ActiveValue::Set(None),
        claimed_at: ActiveValue::Set(None),
        terminal_failure_at: ActiveValue::Set(None),
    };
    secure_insert::<tenants::Entity>(am, &allow_all(), &conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(())
}

/// Insert a single `tenant_closure` row.
pub async fn insert_closure(
    provider: &Arc<AmDbProvider>,
    ancestor_id: Uuid,
    descendant_id: Uuid,
    barrier: i16,
    descendant_status: i16,
) -> Result<()> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let am = tenant_closure::ActiveModel {
        ancestor_id: ActiveValue::Set(ancestor_id),
        descendant_id: ActiveValue::Set(descendant_id),
        barrier: ActiveValue::Set(barrier),
        descendant_status: ActiveValue::Set(descendant_status),
    };
    secure_insert::<tenant_closure::Entity>(am, &allow_all(), &conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(())
}

/// Read one closure row by `(ancestor_id, descendant_id)`.
pub async fn fetch_closure_row(
    provider: &Arc<AmDbProvider>,
    ancestor: Uuid,
    descendant: Uuid,
) -> Result<Option<tenant_closure::Model>> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    let row = tenant_closure::Entity::find()
        .filter(
            Condition::all()
                .add(tenant_closure::Column::AncestorId.eq(ancestor))
                .add(tenant_closure::Column::DescendantId.eq(descendant)),
        )
        .secure()
        .scope_with(&allow)
        .one(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(row)
}

/// Read every closure row whose `descendant_id` matches the argument
/// — used by lifecycle tests to assert status-flip rewrites every
/// row pointing at a tenant.
pub async fn fetch_closure_rows_for_descendant(
    provider: &Arc<AmDbProvider>,
    descendant: Uuid,
) -> Result<Vec<tenant_closure::Model>> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    let rows = tenant_closure::Entity::find()
        .filter(tenant_closure::Column::DescendantId.eq(descendant))
        .secure()
        .scope_with(&allow)
        .all(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(rows)
}

/// Read every closure row referencing `tenant_id` as ancestor or
/// descendant — used by the hard-delete test.
pub async fn fetch_closure_rows_referencing(
    provider: &Arc<AmDbProvider>,
    tenant_id: Uuid,
) -> Result<Vec<tenant_closure::Model>> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    let rows = tenant_closure::Entity::find()
        .filter(
            Condition::any()
                .add(tenant_closure::Column::AncestorId.eq(tenant_id))
                .add(tenant_closure::Column::DescendantId.eq(tenant_id)),
        )
        .secure()
        .scope_with(&allow)
        .all(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(rows)
}

/// Snapshot every `tenants.id` for the closure-only invariant test.
pub async fn fetch_all_tenant_ids(provider: &Arc<AmDbProvider>) -> Result<Vec<Uuid>> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    let rows = tenants::Entity::find()
        .secure()
        .scope_with(&allow)
        .all(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let mut ids: Vec<Uuid> = rows.into_iter().map(|m| m.id).collect();
    ids.sort();
    Ok(ids)
}

/// Snapshot every `tenants` row, sorted by `id`, for tests that need
/// to assert closure-only repair did not mutate any tenant column.
/// Comparing IDs alone would miss a stray UPDATE on `parent_id`,
/// `status`, `depth`, or `self_managed`.
pub async fn fetch_all_tenant_rows(provider: &Arc<AmDbProvider>) -> Result<Vec<tenants::Model>> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    let mut rows = tenants::Entity::find()
        .secure()
        .scope_with(&allow)
        .all(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    rows.sort_by_key(|r| r.id);
    Ok(rows)
}

/// Read one tenant row for direct status assertions.
pub async fn fetch_tenant(
    provider: &Arc<AmDbProvider>,
    id: Uuid,
) -> Result<Option<tenants::Model>> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    let row = tenants::Entity::find()
        .filter(tenants::Column::Id.eq(id))
        .secure()
        .scope_with(&allow)
        .one(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(row)
}

/// Stamp `claimed_by` / `claimed_at` directly on a tenant row to
/// simulate the retention-scan claim UPDATE without going through
/// `scan_retention_due`. Used by the `hard_delete_one` integration
/// tests that exercise the in-tx eligibility / claim-fence
/// contracts independently of the scanner SQL.
///
/// # Errors
///
/// Returns an error if the `SecureORM` update fails.
pub async fn stamp_retention_claim(
    provider: &Arc<AmDbProvider>,
    tenant_id: Uuid,
    worker_id: Uuid,
    claimed_at: OffsetDateTime,
) -> Result<()> {
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::sea_query::Expr;
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    tenants::Entity::update_many()
        .col_expr(tenants::Column::ClaimedBy, Expr::value(Some(worker_id)))
        .col_expr(tenants::Column::ClaimedAt, Expr::value(Some(claimed_at)))
        .filter(tenants::Column::Id.eq(tenant_id))
        .secure()
        .scope_with(&allow_all())
        .exec(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(())
}

/// Insert a synthetic `integrity_check_runs` row so the next gate
/// `acquire` observes the gate as already held. Returns the
/// synthetic `worker_id` so the caller can DELETE the row after the
/// assertion.
pub async fn pre_populate_gate(provider: &Arc<AmDbProvider>) -> Result<Uuid> {
    let worker_id = Uuid::new_v4();
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let am = integrity_check_runs::ActiveModel {
        id: ActiveValue::Set(1),
        worker_id: ActiveValue::Set(worker_id),
        started_at: ActiveValue::Set(OffsetDateTime::now_utc()),
    };
    secure_insert::<integrity_check_runs::Entity>(am, &allow_all(), &conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(worker_id)
}

/// Release the synthetic gate row — the next operation MUST succeed
/// (gate is non-sticky).
///
/// Filters on `worker_id`, mirroring the production `lock::release`
/// contract.
pub async fn release_gate(provider: &Arc<AmDbProvider>, worker_id: Uuid) -> Result<()> {
    use modkit_db::secure::SecureDeleteExt;
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    integrity_check_runs::Entity::delete_many()
        .filter(integrity_check_runs::Column::WorkerId.eq(worker_id))
        .secure()
        .scope_with(&allow)
        .exec(&conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(())
}

/// Negative-control fixture: root + active child, both with their
/// `(id, id)` self-rows + the strict `(root, child)` closure row.
/// Returns `(root_id, child_id)`.
pub async fn seed_clean_two_node_tree(provider: &Arc<AmDbProvider>) -> Result<(Uuid, Uuid)> {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    insert_tenant(provider, root, None, "root", ACTIVE, false, 0).await?;
    insert_tenant(provider, child, Some(root), "child", ACTIVE, false, 1).await?;
    insert_closure(provider, root, root, 0, ACTIVE).await?;
    insert_closure(provider, child, child, 0, ACTIVE).await?;
    insert_closure(provider, root, child, 0, ACTIVE).await?;
    Ok((root, child))
}

/// Pull the per-category count from a flat
/// `Vec<(IntegrityCategory, Violation)>` returned by
/// `run_integrity_check_for_scope`.
#[must_use]
pub fn count_for(
    violations: &[(
        account_management::domain::tenant::integrity::IntegrityCategory,
        account_management::domain::tenant::integrity::Violation,
    )],
    category: account_management::domain::tenant::integrity::IntegrityCategory,
) -> usize {
    violations.iter().filter(|(c, _)| *c == category).count()
}

/// Pull the per-category `repaired` count out of a `RepairReport`.
#[must_use]
pub fn repaired_count(
    report: &account_management::domain::tenant::integrity::RepairReport,
    cat: account_management::domain::tenant::integrity::IntegrityCategory,
) -> usize {
    report
        .repaired_per_category
        .iter()
        .find(|(c, _)| *c == cat)
        .map_or(0, |(_, n)| *n)
}

/// Pull the per-category `deferred` count out of a `RepairReport`.
#[must_use]
pub fn deferred_count(
    report: &account_management::domain::tenant::integrity::RepairReport,
    cat: account_management::domain::tenant::integrity::IntegrityCategory,
) -> usize {
    report
        .deferred_per_category
        .iter()
        .find(|(c, _)| *c == cat)
        .map_or(0, |(_, n)| *n)
}

// ---------------------------------------------------------------------
// Postgres bring-up (testcontainers).
// ---------------------------------------------------------------------
//
// Gated behind `#[cfg(feature = "postgres")]` because pulling up a
// container per test requires Docker on the host and is therefore not
// part of the default test run. Enable explicitly with
// `cargo test -p cyberware-account-management --features postgres ...`.

#[cfg(feature = "postgres")]
pub mod pg {
    //! Postgres `testcontainers` harness. Mirrors the in-memory
    //! `SQLite` shape (`provider`, `repo`) and adds an auxiliary
    //! `sea_orm::DatabaseConnection` (`ddl_conn`) for one-time DDL
    //! bypass. The container handle is held via `_container` so the
    //! Postgres instance lives until `PgHarness` is dropped.

    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use modkit_db::migration_runner::run_migrations_for_testing;
    use modkit_db::{ConnectOpts, connect_db};
    use sea_orm::ConnectionTrait;
    use testcontainers::{ContainerRequest, ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;

    use account_management::Migrator;
    use account_management::infra::storage::repo_impl::{AmDbProvider, TenantRepoImpl};
    use sea_orm_migration::MigratorTrait;

    /// Bring-up output for the Postgres path. `ddl_conn` is the only
    /// place `execute_unprepared` runs (DROP CONSTRAINT / DROP INDEX
    /// for anomaly seeding); every data write goes through `provider`
    /// via `SecureORM`. `_container` keeps the testcontainers handle
    /// alive — dropping the harness tears the container down.
    pub struct PgHarness {
        pub repo: Arc<TenantRepoImpl>,
        pub provider: Arc<AmDbProvider>,
        pub ddl_conn: sea_orm::DatabaseConnection,
        _container: testcontainers::ContainerAsync<Postgres>,
    }

    /// Spin up a fresh Postgres container, run the AM migrations
    /// against it, and return the production-shaped
    /// `(provider, repo)` pair plus the auxiliary DDL connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the Docker daemon is unreachable, the
    /// container fails to become ready inside the wait window, or
    /// migrations fail. Tests that call this helper `expect(...)` it
    /// — a missing Docker daemon shows up as a clear container-start
    /// failure rather than a silent skip.
    pub async fn bring_up_postgres() -> Result<PgHarness> {
        let postgres_image = Postgres::default();
        let request = ContainerRequest::from(postgres_image)
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app");
        let container = request.start().await?;
        let port = container.get_host_port_ipv4(5432).await?;
        wait_for_tcp("127.0.0.1", port, Duration::from_secs(30)).await?;

        let dsn = format!("postgres://user:pass@127.0.0.1:{port}/app");
        let db = connect_db(&dsn, ConnectOpts::default()).await?;
        let provider: Arc<AmDbProvider> = Arc::new(AmDbProvider::new(db.clone()));

        // Migrations through the modkit-db migration runner so the
        // `_test`-prefixed schema_migrations table matches the
        // SQLite path's bookkeeping byte-for-byte.
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        // Auxiliary raw `sea_orm::DatabaseConnection` solely for DDL
        // bypass (DROP CONSTRAINT / DROP INDEX). Every data-side
        // write still goes through `provider` / `SecureORM`.
        let ddl_conn = sea_orm::Database::connect(&dsn).await?;

        Ok(PgHarness {
            repo: Arc::new(TenantRepoImpl::new(Arc::clone(&provider))),
            provider,
            ddl_conn,
            _container: container,
        })
    }

    /// Drop a named constraint via the auxiliary DDL connection.
    /// Call sites are bounded to "make it possible to seed an
    /// anomalous shape that the FKs would otherwise reject"; never
    /// used to alter behaviour the integrity check then observes.
    ///
    /// # Errors
    ///
    /// Returns an error if the DDL statement fails (constraint name
    /// typo, connection lost).
    pub async fn drop_constraint(
        ddl_conn: &sea_orm::DatabaseConnection,
        table: &str,
        constraint: &str,
    ) -> Result<()> {
        let sql = format!("ALTER TABLE {table} DROP CONSTRAINT {constraint};");
        ddl_conn.execute_unprepared(&sql).await?;
        Ok(())
    }

    /// Drop the `ux_tenants_single_root` partial unique index so a
    /// test can seed two roots and exercise the
    /// `RootCountAnomaly` classifier.
    ///
    /// # Errors
    ///
    /// Returns an error if the DDL statement fails.
    pub async fn drop_unique_root_index(ddl_conn: &sea_orm::DatabaseConnection) -> Result<()> {
        ddl_conn
            .execute_unprepared("DROP INDEX IF EXISTS ux_tenants_single_root;")
            .await?;
        Ok(())
    }

    async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) -> Result<()> {
        use tokio::{
            net::TcpStream,
            time::{Instant, sleep},
        };
        let deadline = Instant::now() + timeout;
        loop {
            if TcpStream::connect((host, port)).await.is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!("timeout waiting for {host}:{port}");
            }
            sleep(Duration::from_millis(200)).await;
        }
    }
}

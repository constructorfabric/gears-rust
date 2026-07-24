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
//!   migration set is FK-free (`toolkit-db` does not enable
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

#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    // The E2E harness section below docks doc-comments that mention
    // identifiers like `OData`, `Router::oneshot`, `MetadataService`,
    // `IdpUser`, etc. — the strict markdown linter wants every such
    // mention back-ticked. The doc text is human-targeted, not a
    // public-API reference; allow the broader doc style here.
    clippy::doc_markdown,
    clippy::too_many_arguments,
    clippy::items_after_statements,
    clippy::struct_field_names,
    clippy::needless_pass_by_value,
    clippy::duration_suboptimal_units,
    clippy::double_must_use,
    clippy::module_name_repetitions,
)]

use std::sync::Arc;
use toolkit_gts::gts_id;

use anyhow::Result;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{SecureEntityExt, secure_insert};
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

use account_management::Migrator;
use account_management::infra::storage::entity::{am_leases, tenant_closure, tenants};
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
use toolkit_security::{SecurityContext, pep_properties};

/// Permissive PDP fake that emits a single
/// [`InTenantSubtree`](toolkit_security::ScopeFilter::in_tenant_subtree)
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
///
/// `subject_id` is fixed at `0xCAFE` — sufficient for tests that
/// exercise behavior independent of audit-actor identity. Tests that
/// assert audit-trail invariants (`requested_by != approved_by`,
/// distinct cancel/reject initiator) MUST use
/// [`ctx_for_with_subject`] with two different subject ids.
#[must_use]
pub fn ctx_for(root: Uuid) -> SecurityContext {
    ctx_for_with_subject(root, Uuid::from_u128(0xCAFE))
}

/// Variant of [`ctx_for`] with a caller-supplied `subject_id`. Use
/// when a test needs distinct actors on opposite sides of a
/// dual-consent flow so the resulting `requested_by` / `approved_by`
/// audit columns can be asserted as different uuids.
#[must_use]
pub fn ctx_for_with_subject(root: Uuid, subject_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject_id)
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
///
/// `ConnectOpts::default()` would use the production `max_conns: 10`
/// pool. With a `sqlite::memory:` DSN every connection in the pool
/// is a **separate private in-memory database**, so migrations
/// applied through one connection are invisible to handlers reading
/// from another — REST integration tests then pass or fail based on
/// which pool slot the request lands in. Pin `max_conns: 1` so the
/// whole test shares a single in-memory database. Mirrors the
/// `tr_plugin::tests::setup` pattern.
pub async fn setup_sqlite() -> Result<Harness> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        ..ConnectOpts::default()
    };
    let db = connect_db("sqlite::memory:", opts).await?;
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
///
/// The `tenant_type_uuid` is left as [`Uuid::nil`] — fixtures that
/// drive code paths through the types-registry should use
/// [`insert_tenant_typed`] instead.
pub async fn insert_tenant(
    provider: &Arc<AmDbProvider>,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: &str,
    status: i16,
    self_managed: bool,
    depth: i32,
) -> Result<()> {
    insert_tenant_with_type(
        provider,
        id,
        parent_id,
        name,
        status,
        self_managed,
        depth,
        Uuid::nil(),
    )
    .await
}

/// Like [`insert_tenant`] but stamps the supplied
/// `tenant_type_uuid`. Used by HTTP-level fixtures that drive
/// `resolve_active_tenant` through a pre-seeded types-registry.
pub async fn insert_tenant_typed(
    provider: &Arc<AmDbProvider>,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: &str,
    status: i16,
    self_managed: bool,
    depth: i32,
) -> Result<()> {
    insert_tenant_with_type(
        provider,
        id,
        parent_id,
        name,
        status,
        self_managed,
        depth,
        harness_tenant_type_uuid(),
    )
    .await
}

async fn insert_tenant_with_type(
    provider: &Arc<AmDbProvider>,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: &str,
    status: i16,
    self_managed: bool,
    depth: i32,
    tenant_type_uuid: Uuid,
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
        tenant_type_uuid: ActiveValue::Set(tenant_type_uuid),
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

/// Seed a `Deleted`-status tenant with explicit retention columns so
/// the retention scanner can be driven deterministically without
/// replaying the soft-delete saga. `retention_window_secs = None`
/// exercises the scanner's gear-default fallback; `claimed_by` /
/// `claimed_at` let a test pre-stamp a (possibly stale) worker claim;
/// `terminal_failure_at = Some(_)` parks the row out of the scan.
#[allow(clippy::too_many_arguments)]
pub async fn insert_deleted_tenant(
    provider: &Arc<AmDbProvider>,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: &str,
    depth: i32,
    deleted_at: OffsetDateTime,
    retention_window_secs: Option<i64>,
    claimed_by: Option<Uuid>,
    claimed_at: Option<OffsetDateTime>,
    terminal_failure_at: Option<OffsetDateTime>,
) -> Result<()> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let now = OffsetDateTime::now_utc();
    let am = tenants::ActiveModel {
        id: ActiveValue::Set(id),
        parent_id: ActiveValue::Set(parent_id),
        name: ActiveValue::Set(name.to_owned()),
        status: ActiveValue::Set(DELETED),
        self_managed: ActiveValue::Set(false),
        tenant_type_uuid: ActiveValue::Set(Uuid::nil()),
        depth: ActiveValue::Set(depth),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        deleted_at: ActiveValue::Set(Some(deleted_at)),
        retention_window_secs: ActiveValue::Set(retention_window_secs),
        claimed_by: ActiveValue::Set(claimed_by),
        claimed_at: ActiveValue::Set(claimed_at),
        terminal_failure_at: ActiveValue::Set(terminal_failure_at),
    };
    secure_insert::<tenants::Entity>(am, &allow_all(), &conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(())
}

/// Seed a `Provisioning` tenant with explicit `created_at` and claim /
/// terminal columns so the provisioning-reaper scan
/// (`scan_stuck_provisioning`) can be driven deterministically.
#[allow(clippy::too_many_arguments)]
pub async fn insert_provisioning_tenant(
    provider: &Arc<AmDbProvider>,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: &str,
    depth: i32,
    created_at: OffsetDateTime,
    claimed_by: Option<Uuid>,
    claimed_at: Option<OffsetDateTime>,
    terminal_failure_at: Option<OffsetDateTime>,
) -> Result<()> {
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let am = tenants::ActiveModel {
        id: ActiveValue::Set(id),
        parent_id: ActiveValue::Set(parent_id),
        name: ActiveValue::Set(name.to_owned()),
        status: ActiveValue::Set(PROVISIONING),
        self_managed: ActiveValue::Set(false),
        tenant_type_uuid: ActiveValue::Set(Uuid::nil()),
        depth: ActiveValue::Set(depth),
        created_at: ActiveValue::Set(created_at),
        updated_at: ActiveValue::Set(created_at),
        deleted_at: ActiveValue::Set(None),
        retention_window_secs: ActiveValue::Set(None),
        claimed_by: ActiveValue::Set(claimed_by),
        claimed_at: ActiveValue::Set(claimed_at),
        terminal_failure_at: ActiveValue::Set(terminal_failure_at),
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
    use sea_orm::sea_query::Expr;
    use toolkit_db::secure::SecureUpdateExt;
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

/// Insert a synthetic `am_leases` row so the next `LeaseManager::acquire`
/// observes the lease as already held. Returns the synthetic holder
/// UUID so the caller can DELETE the row after the assertion.
///
/// The lease key is fixed to `"hierarchy_integrity"` (the only key
/// in use today — see
/// [`account_management::domain::integrity_check::HIERARCHY_INTEGRITY_KEY`]).
/// `locked_until` is set 1h into the future so the steal path's
/// `WHERE locked_until < NOW()` filter does not pick the row up.
pub async fn pre_populate_gate(provider: &Arc<AmDbProvider>) -> Result<Uuid> {
    let holder = Uuid::new_v4();
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let am = am_leases::ActiveModel {
        key: ActiveValue::Set("hierarchy_integrity".to_owned()),
        locked_by: ActiveValue::Set(Some(holder)),
        locked_until: ActiveValue::Set(
            OffsetDateTime::now_utc() + std::time::Duration::from_secs(3600),
        ),
        attempts: ActiveValue::Set(1),
    };
    secure_insert::<am_leases::Entity>(am, &allow_all(), &conn)
        .await
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    Ok(holder)
}

/// Release the synthetic lease row — the next operation MUST succeed
/// (the row's `locked_by` is reset to NULL).
///
/// Filters on the holder UUID so we don't accidentally clear a row
/// the test itself didn't seed.
pub async fn release_gate(provider: &Arc<AmDbProvider>, holder: Uuid) -> Result<()> {
    use toolkit_db::secure::SecureDeleteExt;
    let conn = provider
        .conn()
        .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
    let allow = allow_all();
    am_leases::Entity::delete_many()
        .filter(am_leases::Column::LockedBy.eq(holder))
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

// =====================================================================
// E2E HTTP harness — `Router::oneshot` based in-process driver for
// `tests/api_*_test.rs`.
//
// Mirrors `gears/system/resource-group/resource-group/tests/api_rest_test.rs`.
// AM-specific divergences from the RG template:
//
// * Four services to wire (`TenantService`, `MetadataService`,
//   `UserService`, `ConversionService`) vs RG's three.
// * AM uses [`PermitWithSubtreeResolver`] (already defined above) — it
//   emits `InTenantSubtree` predicates, matching the production
//   PDP shape declared in `gear.rs` (`Capability::TenantHierarchy`).
// * Path prefix on every endpoint is `/account-management/v1/...` —
//   the production `/api/` prefix is added by the gateway layer, not
//   by `register_routes`, so the test router does NOT nest under
//   `/api/`.
// =====================================================================

use std::any::Any;

use account_management::api::rest::routes::register_routes;
use account_management::domain::conversion::service::ConversionService;
use account_management::domain::metadata::registry::{
    InheritancePolicy, MetadataSchemaRegistry, StubMetadataSchemaRegistry,
};
use account_management::domain::metadata::repo::MetadataRepo;
use account_management::domain::metadata::service::MetadataService;
use account_management::domain::tenant::TenantRepo;
use account_management::domain::tenant::resource_checker::InertResourceOwnershipChecker;
use account_management::domain::tenant::service::TenantService;
use account_management::domain::tenant_type::inert_tenant_type_checker;
use account_management::domain::user::service::UserService;
use account_management::infra::storage::repo_impl::{ConversionRepoImpl, MetadataRepoImpl};
use account_management_sdk::{
    IdpDeprovisionTenantRequest, IdpDeprovisionUserRequest, IdpListUsersRequest, IdpPluginClient,
    IdpProvisionFailure, IdpProvisionResult, IdpProvisionTenantRequest, IdpProvisionUserRequest,
    IdpUpdateUserRequest, IdpUser, IdpUserDuplicateField, IdpUserFilterField,
    IdpUserOperationFailure,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use gts::GtsTypeId;
use http_body_util::BodyExt;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationSpec;
use toolkit_odata::Page;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;

// ── No-op OpenAPI registry ───────────────────────────────────────────

/// `OpenApiRegistry` implementation that discards every operation and
/// schema registration. The HTTP-level tests only exercise the runtime
/// router; the `utoipa`-side OpenAPI emission is pinned by the route
/// builder's own unit tests.
pub struct NoopOpenApiRegistry;

impl OpenApiRegistry for NoopOpenApiRegistry {
    fn register_operation(&self, _spec: &OperationSpec) {}

    fn ensure_schema_raw(&self, name: &str, _schemas: Vec<(String, RefOr<Schema>)>) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── In-memory `IdpPluginClient` fake ─────────────────────────────────

/// Stateful in-memory `IdpPluginClient` for integration tests.
///
/// Mirrors the shape of the production
/// `static-idp-plugin::domain::Service`: per-tenant `HashMap<user_id,
/// IdpUser>`, deterministic UUIDv5 ids derived from the username, and
/// a cursor walk over a sorted snapshot. Pre-fix the harness's IdP
/// fake returned `Page::empty(50)` unconditionally and treated every
/// deprovision as success without recording state — so regressions
/// in `create → list → list-with-user_id-filter → delete → list`
/// could ship green. This stateful fake exercises every transition
/// end-to-end.
/// Detects the canonical `$filter = id eq <uuid>` point-lookup shape
/// produced by `ListUsersQuery::with_id`. Local mirror of the
/// equivalent helper in `domain::user::service` — kept in this
/// integration harness so we don't need to leak the production helper.
fn extract_top_level_id_eq(
    filter: Option<&toolkit_odata::filter::FilterNode<IdpUserFilterField>>,
) -> Option<Uuid> {
    use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
    match filter? {
        FilterNode::Binary {
            field: IdpUserFilterField::Id,
            op: FilterOp::Eq,
            value: ODataValue::Uuid(u),
        } => Some(*u),
        _ => None,
    }
}

pub struct FakeIdpPlugin {
    users: parking_lot::Mutex<
        std::collections::HashMap<Uuid, std::collections::HashMap<Uuid, IdpUser>>,
    >,
}

impl FakeIdpPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            users: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn build_user(tenant_id: Uuid, req: &IdpProvisionUserRequest) -> IdpUser {
        // Username is the natural key per tenant, so derive the IdP-
        // assigned uuid from `(tenant_id, username)` — repeated calls
        // against the same pair land on the same id (matches a real
        // provider's stable user-uuid-per-tenant guarantee and keeps
        // tests reproducible across runs).
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_DNS, tenant_id.as_bytes());
        let id = Uuid::new_v5(&namespace, req.payload.username.as_bytes());
        let mut user = IdpUser::new(id, req.payload.username.clone());
        if let Some(email) = req.payload.email.clone() {
            user = user.with_email(email);
        }
        if let Some(display_name) = req.payload.display_name.clone() {
            user = user.with_display_name(display_name);
        }
        user
    }
}

impl Default for FakeIdpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IdpPluginClient for FakeIdpPlugin {
    async fn provision_tenant(
        &self,
        _ctx: &SecurityContext,
        _req: &IdpProvisionTenantRequest,
    ) -> Result<IdpProvisionResult, IdpProvisionFailure> {
        Ok(IdpProvisionResult::new(None))
    }

    async fn deprovision_tenant(
        &self,
        _ctx: &SecurityContext,
        _req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), account_management_sdk::IdpDeprovisionFailure> {
        Ok(())
    }

    async fn provision_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpProvisionUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        let tenant_id = req.tenant_context.tenant_id;
        let user = Self::build_user(tenant_id, req);
        self.users
            .lock()
            .entry(tenant_id)
            .or_default()
            .insert(user.id, user.clone());
        Ok(user)
    }

    async fn deprovision_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpDeprovisionUserRequest,
    ) -> Result<(), IdpUserOperationFailure> {
        // Removed vs already-absent collapse to `Ok(())` per the SDK
        // trait contract; AM does not distinguish them on the wire.
        let mut guard = self.users.lock();
        if let Some(scope) = guard.get_mut(&req.tenant_context.tenant_id) {
            scope.remove(&req.user_id);
            if scope.is_empty() {
                guard.remove(&req.tenant_context.tenant_id);
            }
        }
        Ok(())
    }

    async fn update_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpUpdateUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        let tenant_id = req.tenant_context.tenant_id;
        let mut guard = self.users.lock();
        let Some(scope) = guard.get_mut(&tenant_id) else {
            return Err(IdpUserOperationFailure::NotFound {
                detail: format!("user {} not found in tenant {tenant_id}", req.user_id),
            });
        };
        if !scope.contains_key(&req.user_id) {
            return Err(IdpUserOperationFailure::NotFound {
                detail: format!("user {} not found in tenant {tenant_id}", req.user_id),
            });
        }
        // Username uniqueness on rename, checked against OTHER users.
        if let Some(new_username) = &req.patch.username
            && scope
                .iter()
                .any(|(id, u)| *id != req.user_id && &u.username == new_username)
        {
            return Err(IdpUserOperationFailure::DuplicateUser {
                field: IdpUserDuplicateField::Username,
                detail: "username already in use in this tenant scope".to_owned(),
            });
        }
        let Some(user) = scope.get_mut(&req.user_id) else {
            return Err(IdpUserOperationFailure::NotFound {
                detail: format!("user {} not found in tenant {tenant_id}", req.user_id),
            });
        };
        if let Some(username) = &req.patch.username {
            user.username.clone_from(username);
        }
        if let Some(email) = &req.patch.email {
            user.email.clone_from(email);
        }
        if let Some(display_name) = &req.patch.display_name {
            user.display_name.clone_from(display_name);
        }
        if let Some(first_name) = &req.patch.first_name {
            user.first_name.clone_from(first_name);
        }
        if let Some(last_name) = &req.patch.last_name {
            user.last_name.clone_from(last_name);
        }
        // `password` is write-only and never projected; accepted and
        // ignored by the in-memory store.
        Ok(user.clone())
    }

    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        req: &IdpListUsersRequest,
    ) -> Result<Page<IdpUser>, IdpUserOperationFailure> {
        let mut snapshot: Vec<IdpUser> = {
            let guard = self.users.lock();
            let Some(scope) = guard.get(&req.tenant_context.tenant_id) else {
                return Ok(Page::empty(u64::from(req.pagination.top())));
            };
            // The integration harness only exercises the canonical
            // `$filter = id eq <uuid>` point-lookup shape (via
            // `ListUsersQuery::with_id`); anything else is treated as
            // "no filter, return all".
            match extract_top_level_id_eq(req.filter.as_ref()) {
                Some(uid) => scope.get(&uid).cloned().into_iter().collect(),
                None => scope.values().cloned().collect(),
            }
        };
        // Deterministic order so the offset cursor below is stable.
        snapshot.sort_by_key(|u| u.id);

        let offset: usize = match req.pagination.cursor() {
            None => 0,
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|err| IdpUserOperationFailure::Rejected {
                    detail: format!(
                        "FakeIdpPlugin: cursor must be a non-negative decimal offset \
                         (got {raw:?}): {err}"
                    ),
                })?,
        };
        let total = snapshot.len();
        let top = req.pagination.top() as usize;
        let start = offset.min(total);
        let end = start.saturating_add(top).min(total);
        let items: Vec<IdpUser> = snapshot.drain(start..end).collect();
        let next_cursor = (end < total).then(|| end.to_string());
        let prev_cursor = (start > 0).then(|| start.saturating_sub(top).to_string());
        Ok(Page::new(
            items,
            toolkit_odata::PageInfo {
                next_cursor,
                prev_cursor,
                limit: u64::from(req.pagination.top()),
            },
        ))
    }
}

/// Shared `Arc<dyn IdpPluginClient>` handle for the test router.
#[must_use]
pub fn fake_idp() -> Arc<dyn IdpPluginClient> {
    Arc::new(FakeIdpPlugin::new())
}

// ── Inert collaborators (resource checker, types registry) ───────────

/// Resource-ownership checker that always reports zero owned
/// resources, so soft-delete preconditions never trip on
/// `tenant_has_resources`.
#[must_use]
pub fn inert_resource_checker()
-> Arc<dyn account_management::domain::tenant::resource_checker::ResourceOwnershipChecker> {
    Arc::new(InertResourceOwnershipChecker)
}

/// Canonical chained `tenant_type` id used by every E2E tenant fixture.
/// Mirrors the in-source `domain::user::service_tests::TEST_TENANT_TYPE_ID`
/// so the harness lines up byte-for-byte with the canonical AM unit-test
/// shape.
pub const HARNESS_TENANT_TYPE_ID: &str =
    gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~");

/// The deterministic UUIDv5 derived from [`HARNESS_TENANT_TYPE_ID`].
/// Used as the `tenant_type_uuid` on every tenant row seeded for HTTP
/// tests so [`types_registry_for_users`] can resolve the chained id
/// without a real catalog.
#[must_use]
pub fn harness_tenant_type_uuid() -> Uuid {
    gts::GtsId::try_new(HARNESS_TENANT_TYPE_ID)
        .expect("HARNESS_TENANT_TYPE_ID is a valid chain")
        .to_uuid()
}

/// `MockTypesRegistryClient` pre-seeded with the schemas the AM REST
/// surface needs to reach its happy-path branches: the
/// AM user-projection schema (required by
/// `create_user` per the fail-closed GTS validator) and a minimal
/// tenant-type schema chain so `resolve_active_tenant` (which uses
/// `tenant_type_uuid` to compute the chained id) does not fail closed.
#[must_use]
pub fn types_registry_for_users() -> Arc<dyn types_registry_sdk::TypesRegistryClient> {
    use serde_json::json;
    use types_registry_sdk::{GtsTypeId, GtsTypeSchema, testing::MockTypesRegistryClient};

    let user_body = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "username"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "username": { "type": "string", "minLength": 1, "maxLength": 255 },
            "email": { "type": "string", "format": "email" },
            "display_name": { "type": "string", "minLength": 1, "maxLength": 255 },
        },
    });
    let user_schema = GtsTypeSchema::try_new(
        GtsTypeId::new(gts_id!("cf.core.am.user.v1~")),
        user_body,
        None,
        None,
    )
    .expect("synthetic user schema is valid");

    // Derived chains require the parent to be passed in. We build the
    // full chain root→child recursively.
    fn build_tenant_type_chain(type_id: &str) -> GtsTypeSchema {
        let parent = GtsTypeSchema::derive_parent_type_id(type_id)
            .map(|p| std::sync::Arc::new(build_tenant_type_chain(p.as_ref())));
        GtsTypeSchema::try_new(GtsTypeId::new(type_id), serde_json::json!({}), None, parent)
            .expect("synthetic tenant_type chain is valid")
    }
    let tenant_type_schema = build_tenant_type_chain(HARNESS_TENANT_TYPE_ID);

    Arc::new(MockTypesRegistryClient::new().with_type_schemas([user_schema, tenant_type_schema]))
}

/// Empty metadata schema registry — every per-schema lookup
/// surfaces [`DomainError::MetadataEntryNotFound`]. Used by tests
/// that pin the unregistered-schema 404 envelope.
#[must_use]
pub fn empty_metadata_registry() -> Arc<dyn MetadataSchemaRegistry> {
    Arc::new(StubMetadataSchemaRegistry::new())
}

/// Metadata schema registry seeded with one
/// `(type_id, InheritancePolicy::OverrideOnly)` pair so writes /
/// reads against a registered schema land cleanly while still
/// distinguishing the "schema unknown" 404 path.
#[must_use]
pub fn metadata_registry_with(
    schemas: Vec<(GtsTypeId, InheritancePolicy)>,
) -> Arc<dyn MetadataSchemaRegistry> {
    Arc::new(StubMetadataSchemaRegistry::with_seed(schemas))
}

/// Canonical "registered" metadata schema id used by the metadata
/// REST tests. The chained shape mirrors the
/// `metadata_integration::SCHEMA_A` fixture in the closure-walk
/// integration suite and conforms to the GTS chain grammar
/// (`vendor.package.namespace.type.vMAJOR[.MINOR]` on each segment).
pub const REGISTERED_METADATA_SCHEMA: &str =
    gts_id!("cf.core.am.tenant_metadata.v1~vendor.app.metadata.feature_flag.v1~");

/// Canonical "unregistered" metadata schema id — same chained shape,
/// but the harness does NOT pre-seed it in [`metadata_registry_with`].
pub const UNREGISTERED_METADATA_SCHEMA: &str =
    gts_id!("cf.core.am.tenant_metadata.v1~vendor.app.metadata.unregistered.v1~");

// ── Router builder ───────────────────────────────────────────────────

/// Tuple of every service handle the test router wires up. Tests that
/// need to drive a service directly outside HTTP (e.g. seed a
/// conversion request and then PATCH it via the router) can grab the
/// matching handle from this struct.
pub struct TestServices {
    pub tenant_service: Arc<TenantService<TenantRepoImpl>>,
    pub metadata_service: Arc<MetadataService>,
    pub user_service: Arc<UserService>,
    pub conversion_service: Arc<ConversionService>,
}

/// Build the four AM domain services using the default fakes
/// ([`fake_idp`], [`inert_resource_checker`], [`empty_metadata_registry`],
/// inert types-registry, [`mock_enforcer`]). Mirrors the production
/// `register_rest` wiring in `gear.rs`.
#[must_use]
pub fn build_services(harness: &Harness) -> TestServices {
    build_services_with(harness, fake_idp(), empty_metadata_registry())
}

/// Same as [`build_services`] but parameterised on the `IdP` plugin
/// and the metadata-schema registry — used by tests that need a
/// pre-seeded schema or a non-default IdP behaviour. Uses
/// [`types_registry_for_users`] so the types-registry resolves the
/// harness tenant-type chain: `create_tenant` now loads the parent's
/// context (`load_tenant_context`), which reverse-resolves the
/// parent's `tenant_type_uuid` — an inert registry would 503 it.
#[must_use]
pub fn build_services_with(
    harness: &Harness,
    idp: Arc<dyn IdpPluginClient>,
    metadata_registry: Arc<dyn MetadataSchemaRegistry>,
) -> TestServices {
    build_services_full(harness, idp, metadata_registry, types_registry_for_users())
}

/// Full variant of [`build_services_with`] that also takes the
/// types-registry. Used by tests that exercise `create_user`
/// (requires the `gts.cf.core.am.user.v1~` schema to be present).
#[must_use]
pub fn build_services_full(
    harness: &Harness,
    idp: Arc<dyn IdpPluginClient>,
    metadata_registry: Arc<dyn MetadataSchemaRegistry>,
    types_registry: Arc<dyn types_registry_sdk::TypesRegistryClient>,
) -> TestServices {
    use account_management::config::AccountManagementConfig;

    let cfg = AccountManagementConfig::default();

    let tenant_service = Arc::new(
        TenantService::new(
            Arc::clone(&harness.repo),
            Arc::clone(&idp),
            inert_resource_checker(),
            inert_tenant_type_checker(),
            mock_enforcer(),
            cfg,
        )
        .with_types_registry(Arc::clone(&types_registry)),
    );

    let user_service = Arc::new(UserService::new(
        Arc::clone(&harness.repo) as Arc<dyn TenantRepo>,
        Arc::clone(&idp),
        Arc::clone(&types_registry),
        mock_enforcer(),
    ));

    let metadata_repo: Arc<dyn MetadataRepo> =
        Arc::new(MetadataRepoImpl::new(Arc::clone(&harness.provider)));
    let metadata_service = Arc::new(MetadataService::new(
        metadata_repo,
        Arc::clone(&harness.repo) as Arc<dyn TenantRepo>,
        metadata_registry,
        mock_enforcer(),
    ));

    let conversion_repo: Arc<dyn account_management::domain::conversion::repo::ConversionRepo> =
        Arc::new(ConversionRepoImpl::new(Arc::clone(&harness.provider)));
    let conversion_service = Arc::new(ConversionService::new(
        conversion_repo,
        Arc::clone(&harness.repo) as Arc<dyn TenantRepo>,
        inert_tenant_type_checker(),
        mock_enforcer(),
        std::time::Duration::from_secs(7 * 24 * 60 * 60),
        std::time::Duration::from_secs(7 * 24 * 60 * 60),
    ));

    TestServices {
        tenant_service,
        metadata_service,
        user_service,
        conversion_service,
    }
}

/// Build a full router with default fakes. Equivalent to
/// `build_test_router_with(harness, fake_idp(), empty_metadata_registry())`.
#[must_use]
pub fn build_test_router(services: &TestServices) -> Router {
    let openapi = NoopOpenApiRegistry;
    register_routes(
        Router::new(),
        &openapi,
        Arc::clone(&services.tenant_service),
        Arc::clone(&services.metadata_service),
        Arc::clone(&services.user_service),
        Arc::clone(&services.conversion_service),
    )
}

// ── HTTP request / response helpers ──────────────────────────────────

/// Build a JSON request with an attached [`SecurityContext`] extension.
/// `body` is optional — `None` produces an empty body and no
/// `Content-Type` header (matches RG harness behavior).
#[must_use]
pub fn json_request(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    ctx: SecurityContext,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }

    let body = match body {
        Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
        None => Body::empty(),
    };

    let mut req = builder.body(body).unwrap();
    req.extensions_mut().insert(ctx);
    req
}

/// Build a JSON request WITHOUT a [`SecurityContext`] extension. Used
/// by negative tests that probe what happens when the gateway-injected
/// `Extension<SecurityContext>` is missing.
#[must_use]
pub fn json_request_no_ctx(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);

    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }

    let body = match body {
        Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
        None => Body::empty(),
    };

    builder.body(body).unwrap()
}

/// Read the response body as a JSON [`serde_json::Value`]. Returns
/// `serde_json::Value::Null` for empty / non-JSON bodies (per the RG
/// harness convention).
pub async fn response_body(resp: Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Read the response status + parsed JSON body in one pass — the
/// standard tuple used by error-path assertions on the RFC 9457
/// `Problem` envelope.
pub async fn response_problem(resp: Response<Body>) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let body = response_body(resp).await;
    (status, body)
}

// ── Seeding helpers used by the HTTP suite ───────────────────────────

/// Seed the platform root tenant with its closure self-row. The
/// resulting `root_id` is the tenant the test caller's
/// [`SecurityContext`] should scope to. The `tenant_type_uuid` is
/// stamped to [`harness_tenant_type_uuid`] so the user-ops surface
/// (which mandates a registry-resolvable tenant type) reaches its
/// happy path.
pub async fn seed_root(h: &Harness, root_id: Uuid) {
    insert_tenant_typed(&h.provider, root_id, None, "root", ACTIVE, false, 0)
        .await
        .expect("seed root tenant");
    insert_closure(&h.provider, root_id, root_id, 0, ACTIVE)
        .await
        .expect("seed root self-row");
}

/// Seed an active child tenant under `parent_id`. Inserts the child
/// row plus all closure rows along the ancestor chain — keeps the
/// test fixtures FK-clean even on the FK-enforcing Postgres backend.
/// Stamps [`harness_tenant_type_uuid`] on the row so user-ops
/// fixtures resolve cleanly.
pub async fn seed_active_child(
    h: &Harness,
    child_id: Uuid,
    parent_id: Uuid,
    name: &str,
    depth: i32,
) {
    insert_tenant_typed(
        &h.provider,
        child_id,
        Some(parent_id),
        name,
        ACTIVE,
        false,
        depth,
    )
    .await
    .expect("seed child tenant");
    insert_closure(&h.provider, child_id, child_id, 0, ACTIVE)
        .await
        .expect("seed child self-row");
    // Strict ancestor closure rows along the chain from `parent_id`
    // up to the root. `load_ancestor_chain_through_parent` would
    // discover them at runtime; here we only need `(parent_id,
    // child_id)` plus the root self-row already seeded.
    insert_closure(&h.provider, parent_id, child_id, 0, ACTIVE)
        .await
        .expect("seed (parent, child) closure row");
}

// =====================================================================
// END E2E HTTP harness
// =====================================================================

// ---------------------------------------------------------------------
// Postgres bring-up (testcontainers).
// ---------------------------------------------------------------------
//
// Gated behind `#[cfg(feature = "postgres")]` because pulling up a
// container per test requires Docker on the host and is therefore not
// part of the default test run. Enable explicitly with
// `cargo test -p cf-gears-account-management --features postgres ...`.

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
    use sea_orm::ConnectionTrait;
    use testcontainers::{ContainerRequest, ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    use toolkit_db::{ConnectOpts, connect_db};

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

        // Migrations through the toolkit-db migration runner so the
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

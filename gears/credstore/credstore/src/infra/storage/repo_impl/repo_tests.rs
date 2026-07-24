//! SQLite-backed integration tests for [`SecretRepoImpl`].
//!
//! These exercise the real `SeaORM`/`SecureORM` read + write paths against an
//! in-memory SQLite database built from the module's own migration plus a
//! test-only `tenant_closure` table. No raw SQL: schema comes from migration
//! definitions, fixtures are seeded through the repository's own write methods.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::sync::Arc;

use credstore_sdk::{OwnerId, SecretRef, SecretType, SharingMode, TenantId};
use sea_orm::{EntityTrait, Set};
use sea_orm_migration::{MigratorTrait, prelude as mig};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::SecureInsertExt;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{NewSecret, SecretStatus};
use crate::domain::secret::repo::SecretRepo;
use crate::infra::storage::entity;
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo_impl::SecretRepoImpl;

/// Test-only migration creating the platform-owned `tenant_closure` table.
/// In production this table is owned by another platform module; for repo tests
/// we recreate its minimal shape so the `InTenantSubtree` path is exercisable.
struct CreateTenantClosure;

impl mig::MigrationName for CreateTenantClosure {
    fn name(&self) -> &'static str {
        "m_test_create_tenant_closure"
    }
}

#[async_trait::async_trait]
impl mig::MigrationTrait for CreateTenantClosure {
    async fn up(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        manager
            .create_table(
                mig::Table::create()
                    .table(mig::Alias::new("tenant_closure"))
                    .if_not_exists()
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("ancestor_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("descendant_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        mig::ColumnDef::new(mig::Alias::new("barrier"))
                            .small_integer()
                            .not_null()
                            .default(0),
                    )
                    .primary_key(
                        mig::Index::create()
                            .col(mig::Alias::new("ancestor_id"))
                            .col(mig::Alias::new("descendant_id")),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        manager
            .drop_table(
                mig::Table::drop()
                    .table(mig::Alias::new("tenant_closure"))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

/// Build a repo backed by a fresh, isolated in-memory SQLite database.
async fn setup() -> SecretRepoImpl {
    let id = Uuid::new_v4();
    let dsn = format!("sqlite:file:credstore_repo_{id}?mode=memory&cache=shared");
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect sqlite");

    let mut migrations = Migrator::migrations();
    migrations.push(Box::new(CreateTenantClosure));
    run_migrations_for_testing(&db, migrations)
        .await
        .expect("run migrations");

    SecretRepoImpl::new(Arc::new(DBProvider::<DomainError>::new(db)))
}

fn sref(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid secret ref")
}

/// Insert a Provisioning row, leaving it un-promoted.
async fn seed_provisioning(
    repo: &SecretRepoImpl,
    tenant: Uuid,
    owner: Uuid,
    key: &str,
    sharing: SharingMode,
) -> Uuid {
    let id = Uuid::new_v4();
    repo.insert_provisioning(
        &AccessScope::for_tenant(tenant),
        &NewSecret {
            id,
            tenant_id: TenantId(tenant),
            reference: sref(key),
            sharing,
            owner_id: OwnerId(owner),
            secret_type_uuid: SecretType::generic().uuid(),
            expires_at: None,
            value_fp: vec![7u8; 32],
            fp_key_id: 1,
        },
    )
    .await
    .expect("insert_provisioning");
    id
}

/// Insert a row and promote it to Active.
async fn seed_active(
    repo: &SecretRepoImpl,
    tenant: Uuid,
    owner: Uuid,
    key: &str,
    sharing: SharingMode,
) -> Uuid {
    let id = seed_provisioning(repo, tenant, owner, key, sharing).await;
    repo.mark_active(&AccessScope::for_tenant(tenant), id)
        .await
        .expect("mark_active");
    id
}

/// Seed a `tenant_closure` edge (`ancestor` -> `descendant`).
async fn seed_closure(repo: &SecretRepoImpl, ancestor: Uuid, descendant: Uuid, barrier: i16) {
    let conn = repo.db.conn().expect("conn");
    entity::tenant_closure::Entity::insert(entity::tenant_closure::ActiveModel {
        ancestor_id: Set(ancestor),
        descendant_id: Set(descendant),
        barrier: Set(barrier),
    })
    .secure()
    .scope_unchecked(&AccessScope::allow_all())
    .expect("scope")
    .exec(&conn)
    .await
    .expect("seed closure");
}

fn subtree_scope(root: Uuid) -> AccessScope {
    AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_tenant_subtree(pep_properties::OWNER_TENANT_ID, root, true, Vec::new()),
    ])])
}

// ── write path ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn insert_then_mark_active_promotes_row() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = seed_provisioning(&repo, tenant, owner, "db-pw", SharingMode::Private).await;

    // Not yet active: a get over the tenant chain finds nothing.
    let found = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("db-pw"), &[tenant])
        .await
        .expect("resolve");
    assert!(found.is_none(), "provisioning rows are invisible to get");

    repo.mark_active(&AccessScope::for_tenant(tenant), id)
        .await
        .expect("mark_active");

    let found = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("db-pw"), &[tenant])
        .await
        .expect("resolve")
        .expect("active row visible");
    assert_eq!(found.id, id);
}

#[tokio::test]
async fn mark_active_twice_conflicts() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let id = seed_active(&repo, tenant, Uuid::new_v4(), "k", SharingMode::Tenant).await;
    // Second promotion finds no Provisioning row -> Conflict.
    let err = repo
        .mark_active(&AccessScope::for_tenant(tenant), id)
        .await
        .expect_err("second mark_active conflicts");
    assert!(matches!(err, DomainError::Conflict));
}

#[tokio::test]
async fn touch_bumps_version_and_changes_sharing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = seed_active(&repo, tenant, owner, "share-me", SharingMode::Tenant).await;

    let row = repo
        .touch(
            &AccessScope::for_tenant(tenant),
            id,
            SharingMode::Shared,
            None,
            None,
            vec![9u8; 32],
        )
        .await
        .expect("touch")
        .expect("row updated");
    assert_eq!(row.sharing, SharingMode::Shared);
    assert_eq!(row.version, 2, "insert seeds 1; touch bumps to 2");

    // A second touch bumps again (sharing unchanged this time).
    let row = repo
        .touch(
            &AccessScope::for_tenant(tenant),
            id,
            SharingMode::Shared,
            None,
            None,
            vec![9u8; 32],
        )
        .await
        .expect("touch")
        .expect("row updated");
    assert_eq!(row.version, 3);
}

#[tokio::test]
async fn touch_missing_row_returns_none() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let res = repo
        .touch(
            &AccessScope::for_tenant(tenant),
            Uuid::new_v4(),
            SharingMode::Shared,
            None,
            None,
            vec![9u8; 32],
        )
        .await
        .expect("touch");
    assert!(res.is_none());
}

#[tokio::test]
async fn touch_provisioning_row_returns_none() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    // A provisioning (un-promoted) row must not be touchable — the Status==Active
    // filter guards against bumping a stuck or never-promoted row.
    let id = seed_provisioning(&repo, tenant, owner, "prov", SharingMode::Tenant).await;
    let res = repo
        .touch(
            &AccessScope::for_tenant(tenant),
            id,
            SharingMode::Shared,
            None,
            None,
            vec![9u8; 32],
        )
        .await
        .expect("touch");
    assert!(res.is_none(), "touch must not bump a provisioning row");
}

#[tokio::test]
async fn touch_with_matching_expected_version_bumps() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let id = seed_active(&repo, tenant, Uuid::new_v4(), "ver", SharingMode::Tenant).await;
    // seed_active inserts version 1; the matching precondition bumps to 2.
    let row = repo
        .touch(
            &AccessScope::for_tenant(tenant),
            id,
            SharingMode::Tenant,
            Some(1),
            None,
            vec![9u8; 32],
        )
        .await
        .expect("touch")
        .expect("row updated");
    assert_eq!(row.version, 2);
}

#[tokio::test]
async fn touch_with_stale_expected_version_returns_none() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let id = seed_active(&repo, tenant, Uuid::new_v4(), "ver", SharingMode::Tenant).await;
    // Version is 1; a stale precondition (99) gates the UPDATE to 0 rows.
    let res = repo
        .touch(
            &AccessScope::for_tenant(tenant),
            id,
            SharingMode::Tenant,
            Some(99),
            None,
            vec![9u8; 32],
        )
        .await
        .expect("touch");
    assert!(res.is_none(), "stale expected_version must match 0 rows");
}

#[tokio::test]
async fn delete_by_id_removes_then_not_found() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let id = seed_active(&repo, tenant, Uuid::new_v4(), "gone", SharingMode::Tenant).await;
    let scope = AccessScope::for_tenant(tenant);

    repo.delete_by_id(&scope, id, None).await.expect("delete");
    let err = repo
        .delete_by_id(&scope, id, None)
        .await
        .expect_err("second delete is NotFound");
    assert!(matches!(err, DomainError::NotFound));
}

#[tokio::test]
async fn delete_by_id_with_stale_expected_version_is_not_found() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let id = seed_active(
        &repo,
        tenant,
        Uuid::new_v4(),
        "ver-del",
        SharingMode::Tenant,
    )
    .await;
    let scope = AccessScope::for_tenant(tenant);

    // Version is 1; a stale precondition (99) gates the DELETE to 0 rows.
    let err = repo
        .delete_by_id(&scope, id, Some(99))
        .await
        .expect_err("stale expected_version must match 0 rows");
    assert!(matches!(err, DomainError::NotFound));

    // The matching version deletes the row.
    repo.delete_by_id(&scope, id, Some(1))
        .await
        .expect("matching expected_version deletes");
}

#[tokio::test]
async fn list_stale_pending_matches_by_status_and_age() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let prov_id =
        seed_provisioning(&repo, tenant, Uuid::new_v4(), "stale", SharingMode::Tenant).await;
    let deprov_id = seed_active(&repo, tenant, Uuid::new_v4(), "gone", SharingMode::Tenant).await;
    assert!(
        repo.mark_deprovisioning(&AccessScope::for_tenant(tenant), deprov_id, None)
            .await
            .expect("mark deprovisioning")
    );
    seed_active(&repo, tenant, Uuid::new_v4(), "live", SharingMode::Tenant).await;

    // Fresh rows survive a stale-only sweep (cutoff far in the past).
    let none = repo
        .list_stale_pending(100_000, 100_000, 100)
        .await
        .expect("list none");
    assert!(none.is_empty(), "fresh rows are not stale");

    // older_than 0 -> cutoff = now: both non-active rows match, active never.
    let stale = repo
        .list_stale_pending(0, 0, 100)
        .await
        .expect("list stale");
    let ids: Vec<Uuid> = stale.iter().map(|r| r.id).collect();
    assert_eq!(stale.len(), 2);
    assert!(ids.contains(&prov_id) && ids.contains(&deprov_id));

    // Per-status cutoffs are independent.
    let only_deprov = repo
        .list_stale_pending(100_000, 0, 100)
        .await
        .expect("list deprov only");
    assert_eq!(only_deprov.len(), 1);
    assert_eq!(only_deprov[0].id, deprov_id);

    // The limit bounds the batch.
    let limited = repo.list_stale_pending(0, 0, 1).await.expect("limit");
    assert_eq!(limited.len(), 1);
}

#[tokio::test]
async fn reap_by_id_is_status_gated_and_idempotent() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let id = seed_provisioning(&repo, tenant, Uuid::new_v4(), "stale", SharingMode::Tenant).await;

    // A status mismatch never removes the row (fences a concurrent transition).
    assert!(
        !repo
            .reap_by_id(id, SecretStatus::Deprovisioning)
            .await
            .expect("reap wrong-status"),
        "must not reap a row whose status differs from `expected`"
    );

    // Matching status removes it and reports the removal.
    assert!(
        repo.reap_by_id(id, SecretStatus::Provisioning)
            .await
            .expect("reap")
    );
    // Idempotent: a second delete of a gone row is success, reporting no removal.
    assert!(
        !repo
            .reap_by_id(id, SecretStatus::Provisioning)
            .await
            .expect("reap again")
    );

    let stale = repo.list_stale_pending(0, 0, 100).await.expect("list");
    assert!(stale.is_empty());
}

#[tokio::test]
async fn mark_deprovisioning_gates_on_version_and_hides_from_resolution() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let id = seed_active(&repo, tenant, owner, "revoke-me", SharingMode::Tenant).await;

    // Stale expected_version flips nothing.
    assert!(
        !repo
            .mark_deprovisioning(&scope, id, Some(99))
            .await
            .expect("gated mark")
    );

    // Matching version flips the row; resolution no longer sees it.
    assert!(
        repo.mark_deprovisioning(&scope, id, Some(1))
            .await
            .expect("mark")
    );
    let resolved = repo
        .resolve_for_get(
            TenantId(tenant),
            OwnerId(owner),
            &sref("revoke-me"),
            &[tenant],
        )
        .await
        .expect("resolve");
    assert!(
        resolved.is_none(),
        "deprovisioning row must be invisible to resolution"
    );

    // find_own still returns it (saga resume), version untouched.
    let own = repo
        .find_own(&scope, TenantId(tenant), OwnerId(owner), &sref("revoke-me"))
        .await
        .expect("find_own")
        .expect("row visible for delete resume");
    assert_eq!(own.id, id);
    assert_eq!(own.version, 1, "mark_deprovisioning must not bump version");
    assert_eq!(
        own.status,
        crate::domain::secret::model::SecretStatus::Deprovisioning
    );

    // A second mark is a no-op (row no longer active).
    assert!(
        !repo
            .mark_deprovisioning(&scope, id, None)
            .await
            .expect("re-mark")
    );
}

#[tokio::test]
async fn deprovisioning_row_still_holds_unique_index() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let id = seed_active(&repo, tenant, owner, "held", SharingMode::Tenant).await;
    assert!(
        repo.mark_deprovisioning(&scope, id, None)
            .await
            .expect("mark")
    );

    // Re-creating the same non-private reference conflicts until cleanup.
    let err = repo
        .insert_provisioning(
            &scope,
            &NewSecret {
                id: Uuid::new_v4(),
                tenant_id: TenantId(tenant),
                reference: sref("held"),
                sharing: SharingMode::Tenant,
                owner_id: OwnerId(owner),
                secret_type_uuid: SecretType::generic().uuid(),
                expires_at: None,
                value_fp: vec![7u8; 32],
                fp_key_id: 1,
            },
        )
        .await
        .expect_err("unique index still held");
    assert!(matches!(err, DomainError::Conflict));

    // After the reaper removes the row, the reference is free again.
    assert!(
        repo.reap_by_id(id, SecretStatus::Deprovisioning)
            .await
            .expect("reap")
    );
    seed_provisioning(&repo, tenant, owner, "held", SharingMode::Tenant).await;
}

#[tokio::test]
async fn duplicate_nonprivate_insert_conflicts() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed_provisioning(&repo, tenant, owner, "dup", SharingMode::Tenant).await;

    // uq_credstore_nonprivate: one (tenant, reference) row for sharing <> 1.
    // The DB unique violation is classified back to a domain Conflict.
    let err = repo
        .insert_provisioning(
            &AccessScope::for_tenant(tenant),
            &NewSecret {
                id: Uuid::new_v4(),
                tenant_id: TenantId(tenant),
                reference: sref("dup"),
                sharing: SharingMode::Tenant,
                owner_id: OwnerId(owner),
                secret_type_uuid: SecretType::generic().uuid(),
                expires_at: None,
                value_fp: vec![7u8; 32],
                fp_key_id: 1,
            },
        )
        .await
        .expect_err("duplicate non-private insert violates unique index");
    assert!(matches!(err, DomainError::Conflict));
}

// ── read path ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn find_own_matches_private_owner_and_tenant_shared() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    // Private row owned by `owner`.
    seed_active(&repo, tenant, owner, "priv", SharingMode::Private).await;
    let own = repo
        .find_own(&scope, TenantId(tenant), OwnerId(owner), &sref("priv"))
        .await
        .expect("find_own")
        .expect("private row found by its owner");
    assert_eq!(own.sharing, SharingMode::Private);

    // A different subject does not see the private row.
    let other = repo
        .find_own(
            &scope,
            TenantId(tenant),
            OwnerId(Uuid::new_v4()),
            &sref("priv"),
        )
        .await
        .expect("find_own");
    assert!(other.is_none());

    // Tenant-shared row is visible to any subject in the tenant.
    seed_active(&repo, tenant, owner, "team", SharingMode::Tenant).await;
    let team = repo
        .find_own(
            &scope,
            TenantId(tenant),
            OwnerId(Uuid::new_v4()),
            &sref("team"),
        )
        .await
        .expect("find_own")
        .expect("tenant-shared row visible");
    assert_eq!(team.sharing, SharingMode::Tenant);
}

#[tokio::test]
async fn find_for_write_addresses_sharing_class_for_coexistence() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    // A tenant/shared and a private secret coexist under the SAME reference — the
    // partial unique indexes (`uq_credstore_nonprivate` / `uq_credstore_private`)
    // permit both; these two seeds succeeding is itself the coexistence proof.
    seed_active(&repo, tenant, owner, "dup", SharingMode::Tenant).await;
    seed_active(&repo, tenant, owner, "dup", SharingMode::Private).await;

    // A private write addresses the private row, never the tenant one.
    let private = repo
        .find_for_write(
            &scope,
            TenantId(tenant),
            OwnerId(owner),
            &sref("dup"),
            SharingMode::Private,
        )
        .await
        .expect("find_for_write")
        .expect("private row addressed");
    assert_eq!(private.sharing, SharingMode::Private);

    // A non-private write addresses the tenant/shared row, never the private one.
    for write_sharing in [SharingMode::Tenant, SharingMode::Shared] {
        let nonprivate = repo
            .find_for_write(
                &scope,
                TenantId(tenant),
                OwnerId(owner),
                &sref("dup"),
                write_sharing,
            )
            .await
            .expect("find_for_write")
            .expect("non-private row addressed");
        assert_eq!(nonprivate.sharing, SharingMode::Tenant);
    }

    // A private write by a different owner finds nothing (would create its own).
    let other = repo
        .find_for_write(
            &scope,
            TenantId(tenant),
            OwnerId(Uuid::new_v4()),
            &sref("dup"),
            SharingMode::Private,
        )
        .await
        .expect("find_for_write");
    assert!(other.is_none());
}

#[tokio::test]
async fn private_and_nonprivate_versions_are_independent() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    // Coexisting tenant + private rows under the same reference.
    seed_active(&repo, tenant, owner, "dup", SharingMode::Tenant).await;
    seed_active(&repo, tenant, owner, "dup", SharingMode::Private).await;

    // Bump only the non-private row twice.
    let nonprivate = repo
        .find_for_write(
            &scope,
            TenantId(tenant),
            OwnerId(owner),
            &sref("dup"),
            SharingMode::Tenant,
        )
        .await
        .expect("find")
        .expect("non-private row");
    repo.touch(
        &scope,
        nonprivate.id,
        SharingMode::Tenant,
        None,
        None,
        vec![9u8; 32],
    )
    .await
    .expect("touch")
    .expect("row");
    repo.touch(
        &scope,
        nonprivate.id,
        SharingMode::Tenant,
        None,
        None,
        vec![9u8; 32],
    )
    .await
    .expect("touch")
    .expect("row");

    // Non-private is now at 3; private is untouched at 1.
    let nonprivate = repo
        .find_for_write(
            &scope,
            TenantId(tenant),
            OwnerId(owner),
            &sref("dup"),
            SharingMode::Tenant,
        )
        .await
        .expect("find")
        .expect("non-private row");
    assert_eq!(nonprivate.version, 3);

    let private = repo
        .find_for_write(
            &scope,
            TenantId(tenant),
            OwnerId(owner),
            &sref("dup"),
            SharingMode::Private,
        )
        .await
        .expect("find")
        .expect("private row");
    assert_eq!(
        private.version, 1,
        "private version is independent of the non-private bumps"
    );
}

#[tokio::test]
async fn resolve_for_get_prefers_closest_tenant_then_private() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    // Same reference shared at the parent and held privately at the child.
    seed_active(&repo, parent, owner, "cfg", SharingMode::Shared).await;
    seed_active(&repo, child, owner, "cfg", SharingMode::Private).await;

    // Walk-up chain: child first, then parent. Closest (child) wins.
    let row = repo
        .resolve_for_get(
            TenantId(child),
            OwnerId(owner),
            &sref("cfg"),
            &[child, parent],
        )
        .await
        .expect("resolve")
        .expect("row resolved");
    assert_eq!(row.tenant_id, TenantId(child));
    assert_eq!(row.sharing, SharingMode::Private);

    // Missing key resolves to None.
    let none = repo
        .resolve_for_get(
            TenantId(child),
            OwnerId(owner),
            &sref("absent"),
            &[child, parent],
        )
        .await
        .expect("resolve");
    assert!(none.is_none());
}

// Real-SQL coverage for the core inheritance contract (previously exercised only
// by the FakeSecretRepo): a parent Tenant-mode row is EXCLUDED from a child's
// walk-up, while a parent Shared-mode row IS inherited. Guards the
// `Condition::any()` branch against both a tenant-leak and a false-miss.
#[tokio::test]
async fn resolve_for_get_excludes_parent_tenant_mode_but_inherits_shared() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    // (Tenant and Shared are both non-private and cannot coexist under one ref,
    // so they are seeded under distinct references.)
    seed_active(&repo, parent, owner, "tenant-only", SharingMode::Tenant).await;
    seed_active(&repo, parent, owner, "shared-cfg", SharingMode::Shared).await;

    // Tenant-mode parent secret must NOT leak into the child's walk-up.
    let tenant_only = repo
        .resolve_for_get(
            TenantId(child),
            OwnerId(owner),
            &sref("tenant-only"),
            &[child, parent],
        )
        .await
        .expect("resolve");
    assert!(
        tenant_only.is_none(),
        "parent Tenant-mode secret must not be inherited by a child"
    );

    // Shared parent secret IS inherited; the resolved row belongs to the parent.
    let shared = repo
        .resolve_for_get(
            TenantId(child),
            OwnerId(owner),
            &sref("shared-cfg"),
            &[child, parent],
        )
        .await
        .expect("resolve")
        .expect("shared row must be inherited");
    assert_eq!(shared.sharing, SharingMode::Shared);
    // `is_inherited` is derived at the service layer as `row.tenant_id != req`;
    // here the resolved tenant is the parent, not the requesting child.
    assert_eq!(shared.tenant_id, TenantId(parent));
    assert_ne!(shared.tenant_id, TenantId(child));
}

/// Regression: a Private secret is owner-scoped *and* pinned to its own tenant.
/// Even if the same `subject` (owner) id appears in an ancestor tenant, that
/// ancestor's private secret must never resolve for a descendant walk-up — the
/// query pins `tenant_id == req` rather than trusting the authn invariant that
/// a subject id belongs to a single tenant.
#[tokio::test]
async fn resolve_for_get_never_inherits_ancestor_private_even_for_same_owner() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    // Same owner id present in the ancestor tenant (the invariant we no longer
    // depend on: a reused/cross-tenant subject id).
    let owner = Uuid::new_v4();

    seed_active(&repo, parent, owner, "priv-key", SharingMode::Private).await;

    let resolved = repo
        .resolve_for_get(
            TenantId(child),
            OwnerId(owner),
            &sref("priv-key"),
            &[child, parent],
        )
        .await
        .expect("resolve");
    assert!(
        resolved.is_none(),
        "an ancestor's private secret must not resolve for a descendant, even with a matching owner id"
    );
}

#[tokio::test]
async fn inventory_aggregates_by_sharing_status_and_tenants() {
    let repo = setup().await;
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let owner = Uuid::new_v4();

    seed_active(&repo, t1, owner, "a", SharingMode::Private).await;
    seed_active(&repo, t1, owner, "b", SharingMode::Tenant).await;
    seed_active(&repo, t2, owner, "c", SharingMode::Shared).await;
    seed_provisioning(&repo, t2, owner, "d", SharingMode::Tenant).await;

    let counts = repo.inventory().await.expect("inventory");
    assert_eq!(counts.private, 1);
    assert_eq!(counts.tenant, 1);
    assert_eq!(counts.shared, 1);
    assert_eq!(counts.provisioning, 1);
    assert_eq!(counts.tenants, 2, "distinct tenants among active rows");
}

#[tokio::test]
async fn new_secret_defaults_to_version_one() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed_active(&repo, tenant, owner, "v1", SharingMode::Tenant).await;

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("v1"), &[tenant])
        .await
        .expect("resolve")
        .expect("active row");
    assert_eq!(
        row.version, 1,
        "a freshly inserted secret starts at version 1"
    );
}

// ── scope_includes_tenant ────────────────────────────────────────────────────

#[tokio::test]
async fn scope_includes_tenant_unconstrained_and_deny() {
    let repo = setup().await;
    let t = Uuid::new_v4();
    assert!(
        repo.scope_includes_tenant(&AccessScope::allow_all(), t)
            .await
            .expect("allow_all")
    );
    assert!(
        !repo
            .scope_includes_tenant(&AccessScope::deny_all(), t)
            .await
            .expect("deny_all")
    );
}

#[tokio::test]
async fn scope_includes_tenant_direct_uuid_match() {
    let repo = setup().await;
    let t = Uuid::new_v4();
    assert!(
        repo.scope_includes_tenant(&AccessScope::for_tenant(t), t)
            .await
            .expect("direct match")
    );
    assert!(
        !repo
            .scope_includes_tenant(&AccessScope::for_tenant(t), Uuid::new_v4())
            .await
            .expect("direct miss")
    );
}

#[tokio::test]
async fn scope_includes_tenant_subtree_via_closure() {
    let repo = setup().await;
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let stranger = Uuid::new_v4();
    seed_closure(&repo, root, child, 0).await;

    let scope = subtree_scope(root);
    assert!(
        repo.scope_includes_tenant(&scope, child)
            .await
            .expect("descendant in subtree")
    );
    assert!(
        !repo
            .scope_includes_tenant(&scope, stranger)
            .await
            .expect("non-descendant excluded")
    );
}

#[tokio::test]
async fn scope_includes_tenant_sibling_owner_filter_fails_closed() {
    // A constraint narrower than tenant granularity
    // (`OWNER_TENANT_ID = T AND owner_id = X`) grants access only to X's
    // secrets, not the whole tenant. The gate must NOT admit the tenant on
    // the lone `OWNER_TENANT_ID` match — it fails closed on the sibling.
    let repo = setup().await;
    let t = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, t),
        ScopeFilter::eq(pep_properties::OWNER_ID, owner),
    ])]);
    assert!(
        !repo
            .scope_includes_tenant(&scope, t)
            .await
            .expect("sibling owner filter must fail closed"),
        "a sub-tenant scope must not be widened to the whole tenant"
    );
}

#[tokio::test]
async fn scope_includes_tenant_unknown_property_fails_closed() {
    // A constraint whose only filter is on a property this gate does not
    // treat as tenant-level (`resource_id`) must not admit the tenant.
    let repo = setup().await;
    let t = Uuid::new_v4();
    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![ScopeFilter::eq(
        pep_properties::RESOURCE_ID,
        Uuid::new_v4(),
    )])]);
    assert!(
        !repo
            .scope_includes_tenant(&scope, t)
            .await
            .expect("non-tenant property must fail closed")
    );
}

#[tokio::test]
async fn scope_includes_tenant_or_of_constraints_admits_on_broad_alternative() {
    // Constraints are OR-ed: a narrow (non-admitting) alternative must not
    // veto a sibling constraint that does grant whole-tenant access.
    let repo = setup().await;
    let t = Uuid::new_v4();
    let scope = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, t),
            ScopeFilter::eq(pep_properties::OWNER_ID, Uuid::new_v4()),
        ]),
        ScopeConstraint::new(vec![ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, t)]),
    ]);
    assert!(
        repo.scope_includes_tenant(&scope, t)
            .await
            .expect("broad OR alternative admits"),
        "a whole-tenant alternative must still grant despite a narrower sibling"
    );
}

#[tokio::test]
async fn expired_rows_hidden_from_resolution_and_flipped_by_expiry_sweep() {
    use time::{Duration as TimeDuration, OffsetDateTime};
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    // Seed an active expirable row whose expiry is already in the past
    // (repo layer does not enforce traits; that's the domain's job).
    let id = Uuid::new_v4();
    repo.insert_provisioning(
        &scope,
        &NewSecret {
            id,
            tenant_id: TenantId(tenant),
            reference: sref("expired"),
            sharing: SharingMode::Tenant,
            owner_id: OwnerId(owner),
            secret_type_uuid: SecretType::from_name("bearer-token").expect("known").uuid(),
            expires_at: Some(OffsetDateTime::now_utc() - TimeDuration::seconds(5)),
            value_fp: vec![7u8; 32],
            fp_key_id: 1,
        },
    )
    .await
    .expect("insert");
    repo.mark_active(&scope, id).await.expect("mark active");

    // Expired -> not resolvable...
    let resolved = repo
        .resolve_for_get(
            TenantId(tenant),
            OwnerId(owner),
            &sref("expired"),
            &[tenant],
        )
        .await
        .expect("resolve");
    assert!(resolved.is_none(), "expired row must not resolve");

    // ...but still addressable for writes/deletes.
    assert!(
        repo.find_own(&scope, TenantId(tenant), OwnerId(owner), &sref("expired"))
            .await
            .expect("find_own")
            .is_some()
    );

    // The expiry sweep flips it into the deprovisioning saga.
    let flipped = repo
        .mark_expired_deprovisioning()
        .await
        .expect("expiry sweep");
    assert_eq!(flipped, 1);
    let stale = repo.list_stale_pending(0, 0, 100).await.expect("stale");
    assert_eq!(stale.len(), 1);
    assert_eq!(
        stale[0].status,
        crate::domain::secret::model::SecretStatus::Deprovisioning
    );

    // A live (unexpired) row is untouched by the sweep.
    let live = Uuid::new_v4();
    repo.insert_provisioning(
        &scope,
        &NewSecret {
            id: live,
            tenant_id: TenantId(tenant),
            reference: sref("living"),
            sharing: SharingMode::Tenant,
            owner_id: OwnerId(owner),
            secret_type_uuid: SecretType::from_name("bearer-token").expect("known").uuid(),
            expires_at: Some(OffsetDateTime::now_utc() + TimeDuration::hours(1)),
            value_fp: vec![7u8; 32],
            fp_key_id: 1,
        },
    )
    .await
    .expect("insert live");
    repo.mark_active(&scope, live).await.expect("mark active");
    assert_eq!(
        repo.mark_expired_deprovisioning().await.expect("sweep"),
        0,
        "future expiry must not be swept"
    );
}

#[tokio::test]
async fn secret_type_round_trips_through_storage() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let id = Uuid::new_v4();
    repo.insert_provisioning(
        &scope,
        &NewSecret {
            id,
            tenant_id: TenantId(tenant),
            reference: sref("typed"),
            sharing: SharingMode::Private,
            owner_id: OwnerId(owner),
            secret_type_uuid: SecretType::from_name("personal-token")
                .expect("known")
                .uuid(),
            expires_at: None,
            value_fp: vec![7u8; 32],
            fp_key_id: 1,
        },
    )
    .await
    .expect("insert");
    repo.mark_active(&scope, id).await.expect("mark active");

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("typed"), &[tenant])
        .await
        .expect("resolve")
        .expect("row");
    assert_eq!(
        row.secret_type_uuid,
        SecretType::from_name("personal-token")
            .expect("known")
            .uuid()
    );
}

// ── value-fingerprint fence columns ──────────────────────────────────────────

/// Simulate an out-of-band seeded row: direct INSERT with NULL fence columns
/// (the seeder contract from docs/features/001-value-fingerprint-fence.md).
async fn seed_unfenced(repo: &SecretRepoImpl, tenant: Uuid, owner: Uuid, key: &str) -> Uuid {
    use sea_orm::ActiveValue;
    let id = Uuid::new_v4();
    let conn = repo.db.conn().expect("conn");
    entity::secrets::Entity::insert(entity::secrets::ActiveModel {
        id: ActiveValue::Set(id),
        tenant_id: ActiveValue::Set(tenant),
        reference: ActiveValue::Set(key.to_owned()),
        sharing: ActiveValue::Set(2), // Tenant
        owner_id: ActiveValue::Set(owner),
        status: ActiveValue::Set(2), // Active
        created_at: ActiveValue::NotSet,
        updated_at: ActiveValue::NotSet,
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(SecretType::generic().uuid()),
        expires_at: ActiveValue::Set(None),
        value_fp: ActiveValue::Set(None),
        fp_key_id: ActiveValue::Set(None),
    })
    .secure()
    .scope_unchecked(&AccessScope::allow_all())
    .expect("scope")
    .exec(&conn)
    .await
    .expect("seed unfenced row");
    id
}

#[tokio::test]
async fn backfill_fp_stamps_null_rows_once_and_preserves_version() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let id = seed_unfenced(&repo, tenant, owner, "seeded").await;

    // Listed as unfenced before the stamp.
    let unfenced = repo.list_unfenced(16).await.expect("list");
    assert!(unfenced.iter().any(|r| r.id == id));

    // First backfill stamps (CAS on NULL matches).
    let stamped = repo
        .backfill_fp(id, vec![1u8; 32], 1)
        .await
        .expect("backfill");
    assert!(stamped, "NULL fp row must be stamped");

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("seeded"), &[tenant])
        .await
        .expect("resolve")
        .expect("row");
    assert_eq!(row.value_fp.as_deref(), Some([1u8; 32].as_slice()));
    assert_eq!(row.fp_key_id, Some(1));
    assert_eq!(
        row.version, 1,
        "backfill must not bump the version (the caller's ETag stays stable)"
    );

    // Second backfill is a CAS no-op: a concurrent stamp must never clobber.
    let again = repo
        .backfill_fp(id, vec![2u8; 32], 1)
        .await
        .expect("backfill");
    assert!(!again, "an already-stamped row must not be re-stamped");
    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("seeded"), &[tenant])
        .await
        .expect("resolve")
        .expect("row");
    assert_eq!(
        row.value_fp.as_deref(),
        Some([1u8; 32].as_slice()),
        "the first stamp wins"
    );

    // No longer listed as unfenced.
    let unfenced = repo.list_unfenced(16).await.expect("list");
    assert!(unfenced.iter().all(|r| r.id != id));
}

#[tokio::test]
async fn list_unfenced_skips_stamped_and_non_active_rows() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    // Stamped active row (API-created) — excluded.
    seed_active(&repo, tenant, owner, "stamped", SharingMode::Tenant).await;
    // Unfenced but provisioning-status rows do not exist in practice (API
    // creates always stamp); the sweep must still only pick ACTIVE rows.
    let unfenced_active = seed_unfenced(&repo, tenant, owner, "plain").await;

    let listed = repo.list_unfenced(16).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, unfenced_active);
}

#[tokio::test]
async fn touch_restamps_the_fence_fingerprint() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    // seed_active stamps vec![7u8; 32] via NewSecret.
    let id = seed_active(&repo, tenant, owner, "restamp", SharingMode::Tenant).await;

    let row = repo
        .touch(
            &AccessScope::for_tenant(tenant),
            id,
            SharingMode::Tenant,
            None,
            None,
            vec![8u8; 32],
        )
        .await
        .expect("touch")
        .expect("row updated");
    assert_eq!(
        row.value_fp.as_deref(),
        Some([8u8; 32].as_slice()),
        "touch must persist the new fingerprint atomically with the metadata"
    );
    assert_eq!(row.fp_key_id, Some(1));
    assert_eq!(row.version, 2);
}

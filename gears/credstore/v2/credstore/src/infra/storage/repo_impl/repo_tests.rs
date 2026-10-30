// Updated: 2026-10-06 by Constructor Tech
//! SQLite-backed integration tests for [`SecretRepoImpl`] (ADR-0006:
//! immutable value versions).
//!
//! These exercise the real `SeaORM`/`SecureORM` read + write paths against an
//! in-memory SQLite database built from the module's own migrations
//! (`m0001_initial_schema` + `m0002_value_versions`). No raw SQL for the
//! schema; fixtures are seeded through the repository's own write methods
//! (a handful of tests probe the raw schema directly to pin the migration's
//! `CHECK` contracts).
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::sync::Arc;
use std::time::Duration;

use credstore_sdk::{
    DestroySelector, OwnerId, SecretRef, SecretType, SharingMode, StoreKey, TenantId, ValueVersion,
};
use sea_orm::{ActiveValue, EntityTrait};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{DbTx, ScopeError, SecureEntityExt, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    CleanupDebt, CleanupTask, DeleteVerification, Fallback, HealFlags, IntentCommit,
    NewDeclaredSecret, NewSecret, SecretRow, SecretStatus, WriteAttempt, WriteVerification,
};
use crate::domain::secret::repo::SecretRepo;
use crate::infra::storage::entity;
use crate::infra::storage::entity::enums::{DbFallback, DbSharing, DbStatus};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo_impl::SecretRepoImpl;
use crate::infra::storage::repo_impl::helpers::{TxFuture, entity_to_debt};
use crate::infra::storage::repo_impl::intents::{delete_intent_tx, record_debts};

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

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");

    SecretRepoImpl::new(Arc::new(DBProvider::<DomainError>::new(db)))
}

/// The tasks of every debt row currently held (`credstore_store_cleanup`),
/// in a deterministic order (the rows of one transaction share a timestamp).
async fn debt_tasks(repo: &SecretRepoImpl) -> Vec<CleanupTask> {
    let conn = repo.db.conn().expect("conn");
    let mut tasks: Vec<CleanupTask> = entity::store_cleanup::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("read debts")
        .into_iter()
        .map(|m| entity_to_debt(m).expect("in-domain debt").task)
        .collect();
    tasks.sort_by_key(|t| format!("{t:?}"));
    tasks
}

/// Every key purge among the held debt rows.
async fn purged(repo: &SecretRepoImpl) -> Vec<StoreKey> {
    debt_tasks(repo)
        .await
        .into_iter()
        .filter_map(|t| match t {
            CleanupTask::Purge(key) => Some(key),
            CleanupTask::Destroy { .. } => None,
        })
        .collect()
}

/// The tasks of `debts`, in a deterministic order.
fn tasks_of(debts: Vec<CleanupDebt>) -> Vec<CleanupTask> {
    let mut tasks: Vec<CleanupTask> = debts.into_iter().map(|d| d.task).collect();
    tasks.sort_by_key(|t| format!("{t:?}"));
    tasks
}

const LEASE: Duration = Duration::from_mins(5);

/// The attempt ids of every write intent currently held.
async fn intent_ids(repo: &SecretRepoImpl) -> Vec<Uuid> {
    let conn = repo.db.conn().expect("conn");
    entity::write_intents::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("read intents")
        .into_iter()
        .map(|i| i.attempt_id)
        .collect()
}

/// Deletes the attempt's intent (what a later heal does to an expired one).
async fn drop_intent(repo: &SecretRepoImpl, attempt_id: Uuid) {
    repo.run_tx(move |tx: &DbTx<'_>| {
        Box::pin(async move { delete_intent_tx(tx, attempt_id).await.map(|_| ()) })
            as TxFuture<'_, ()>
    })
    .await
    .expect("drop intent");
}

/// Records `tasks` as debt rows in one transaction.
async fn record_tasks(repo: &SecretRepoImpl, tasks: Vec<CleanupTask>) -> Vec<CleanupDebt> {
    repo.run_tx(move |tx: &DbTx<'_>| {
        let tasks = tasks.clone();
        Box::pin(async move { record_debts(tx, tasks).await }) as TxFuture<'_, Vec<CleanupDebt>>
    })
    .await
    .expect("record debts")
}

/// An attempt that `put`s under the store key of record `id` in `tenant`,
/// for a plugin that supports `destroy`.
fn attempt_for(tenant: Uuid, id: Uuid) -> WriteAttempt {
    WriteAttempt {
        attempt_id: Uuid::new_v4(),
        key: StoreKey::new(TenantId(tenant), id),
        reference: "k".to_owned(),
        destroy_supported: true,
        heal_expired_intents: false,
    }
}

/// tx0: announce a write attempt on record `id`.
async fn begin(repo: &SecretRepoImpl, tenant: Uuid, id: Uuid) -> WriteAttempt {
    let attempt = attempt_for(tenant, id);
    repo.begin_write_intent(&attempt, LEASE)
        .await
        .expect("begin_write_intent");
    attempt
}

/// tx0 + tx1 of a create: announces the attempt, then commits the insert.
async fn insert_with_intent(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    new: &NewSecret,
) -> IntentCommit<()> {
    let attempt = begin(repo, new.tenant_id.0, new.id).await;
    repo.insert_active(scope, new, &attempt)
        .await
        .expect("insert_active")
}

/// [`insert_with_intent`], asserting the insert committed.
async fn insert_ok(repo: &SecretRepoImpl, scope: &AccessScope, new: &NewSecret) {
    assert!(matches!(
        insert_with_intent(repo, scope, new).await,
        IntentCommit::Committed { .. }
    ));
}

/// tx0 + tx1 of an overwrite of record `id` (version `expected_version`) by
/// `value`, on a `Tenant`-shared, `Inherit` record without expiry.
async fn switch_with_intent(
    repo: &SecretRepoImpl,
    tenant: Uuid,
    id: Uuid,
    expected_version: i64,
    value: &str,
) -> IntentCommit<SecretRow> {
    let attempt = begin(repo, tenant, id).await;
    repo.switch_value(
        &AccessScope::for_tenant(tenant),
        id,
        expected_version,
        SharingMode::Tenant,
        Fallback::Inherit,
        None,
        vv(value),
        &attempt,
    )
    .await
    .expect("switch_value")
}

fn vv(s: &str) -> ValueVersion {
    ValueVersion::new(s)
}

fn sref(s: &str) -> SecretRef {
    SecretRef::new(s).expect("valid secret ref")
}

fn new_secret(
    tenant: Uuid,
    owner: Uuid,
    key: &str,
    sharing: SharingMode,
    value_version: ValueVersion,
) -> NewSecret {
    new_secret_typed(
        tenant,
        owner,
        key,
        sharing,
        value_version,
        SecretType::generic().uuid(),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "test fixture builder mirroring NewSecret's own field list plus an explicit type"
)]
fn new_secret_typed(
    tenant: Uuid,
    owner: Uuid,
    key: &str,
    sharing: SharingMode,
    value_version: ValueVersion,
    secret_type_uuid: Uuid,
) -> NewSecret {
    NewSecret {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: sref(key),
        sharing,
        owner_id: OwnerId(owner),
        secret_type_uuid,
        expires_at: None,
        value_version,
        fallback: Fallback::Inherit,
    }
}

/// Insert an active row via the real write-protocol entrypoint, returning
/// `(row_id, value_version)`.
async fn seed_active(
    repo: &SecretRepoImpl,
    tenant: Uuid,
    owner: Uuid,
    key: &str,
    sharing: SharingMode,
) -> (Uuid, ValueVersion) {
    let value_version = vv("1");
    let new = new_secret(tenant, owner, key, sharing, value_version.clone());
    let id = new.id;
    insert_ok(repo, &AccessScope::for_tenant(tenant), &new).await;
    (id, value_version)
}

/// Like [`seed_active`], with an explicit `secret_type_uuid` — for tests
/// that need more than one distinct type present.
async fn seed_active_typed(
    repo: &SecretRepoImpl,
    tenant: Uuid,
    owner: Uuid,
    key: &str,
    sharing: SharingMode,
    secret_type_uuid: Uuid,
) -> (Uuid, ValueVersion) {
    let value_version = vv("1");
    let new = new_secret_typed(
        tenant,
        owner,
        key,
        sharing,
        value_version.clone(),
        secret_type_uuid,
    );
    let id = new.id;
    insert_ok(repo, &AccessScope::for_tenant(tenant), &new).await;
    (id, value_version)
}

fn subtree_scope(root: Uuid) -> AccessScope {
    AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_tenant_subtree(pep_properties::OWNER_TENANT_ID, root, true, Vec::new()),
    ])])
}

// ── migration schema contracts ───────────────────────────────────────────────

/// Insert a `credstore_secrets` row bypassing the domain layer entirely, so
/// tests can probe the raw migration `CHECK` contracts directly (a
/// pointer/status pairing the domain would never construct; the retired
/// status codes cannot even be expressed by the typed entity and are
/// covered by the migration tests).
async fn insert_raw_secret(
    repo: &SecretRepoImpl,
    status: DbStatus,
    value_version: Option<String>,
) -> Result<(), ScopeError> {
    let conn = repo.db.conn().expect("conn");
    entity::secrets::Entity::insert(entity::secrets::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(Uuid::new_v4()),
        reference: ActiveValue::Set("x".to_owned()),
        sharing: ActiveValue::Set(DbSharing::Tenant),
        owner_id: ActiveValue::Set(Uuid::new_v4()),
        status: ActiveValue::Set(status),
        created_at: ActiveValue::NotSet,
        updated_at: ActiveValue::NotSet,
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(SecretType::generic().uuid()),
        expires_at: ActiveValue::Set(None),
        value_version: ActiveValue::Set(value_version),
        fallback: ActiveValue::Set(DbFallback::Inherit),
    })
    .secure()
    .scope_unchecked(&AccessScope::allow_all())?
    .exec(&conn)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn migration_final_schema_enforces_value_version_with_status_pairing() {
    let repo = setup().await;
    // An active row with no value version violates
    // credstore_secrets_value_version_check.
    let err = insert_raw_secret(&repo, DbStatus::Active, None)
        .await
        .expect_err("active row without a value version must violate the pairing CHECK");
    assert!(err.to_string().to_lowercase().contains("check"));

    // A declared row carrying a value version equally violates it.
    let err = insert_raw_secret(&repo, DbStatus::Declared, Some("1".to_owned()))
        .await
        .expect_err("declared row with a value version must violate the pairing CHECK");
    assert!(err.to_string().to_lowercase().contains("check"));

    // The two consistent shapes are accepted.
    insert_raw_secret(&repo, DbStatus::Active, Some("1".to_owned()))
        .await
        .expect("active with a value version");
    insert_raw_secret(&repo, DbStatus::Declared, None)
        .await
        .expect("declared without one");
}

// ── write protocol: insert_active ────────────────────────────────────────────

#[tokio::test]
async fn insert_active_stores_the_value_version_pointer() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();

    let new = new_secret(tenant, owner, "k", SharingMode::Tenant, vv("5"));
    insert_ok(&repo, &AccessScope::for_tenant(tenant), &new).await;

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("k"), &[tenant])
        .await
        .expect("resolve")
        .expect("row visible");
    assert_eq!(row.value_version, Some(vv("5")));
    assert_eq!(row.status, SecretStatus::Active);
    assert_eq!(row.version, 1);
    assert_eq!(row.store_key(), StoreKey::new(TenantId(tenant), new.id));
}

#[tokio::test]
async fn duplicate_nonprivate_insert_is_a_definite_loss_that_purges_the_fresh_key() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed_active(&repo, tenant, owner, "dup", SharingMode::Tenant).await;

    let new = new_secret(tenant, owner, "dup", SharingMode::Tenant, vv("2"));
    let attempt = begin(&repo, tenant, new.id).await;
    let outcome = repo
        .insert_active(&AccessScope::for_tenant(tenant), &new, &attempt)
        .await
        .expect("a unique violation is a definite outcome, not an error");
    let key = StoreKey::new(TenantId(tenant), new.id);
    let IntentCommit::Lost { debts } = outcome else {
        panic!("duplicate non-private insert violates the unique index: {outcome:?}");
    };
    assert_eq!(tasks_of(debts), vec![CleanupTask::Purge(key.clone())]);
    // The intent deletion committed in the same transaction as the purge.
    assert_eq!(intent_ids(&repo).await, Vec::<Uuid>::new());
    assert_eq!(debt_tasks(&repo).await, vec![CleanupTask::Purge(key)]);
}

#[tokio::test]
async fn insert_over_an_expired_row_conflicts_and_changes_nothing() {
    use time::Duration as TimeDuration;
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    let mut old = new_secret(tenant, owner, "exp", SharingMode::Tenant, vv("1"));
    old.expires_at = Some(time::OffsetDateTime::now_utc() - TimeDuration::seconds(5));
    insert_ok(&repo, &scope, &old).await;

    // An expired record still holds the reference: expiry applies to the
    // secret, not to the record, so a plain create over it is a conflict.
    let new = new_secret(tenant, owner, "exp", SharingMode::Tenant, vv("1"));
    let outcome = insert_with_intent(&repo, &scope, &new).await;
    assert!(
        matches!(outcome, IntentCommit::Lost { .. }),
        "the expired row still holds the reference: {outcome:?}"
    );

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("exp"), &[tenant])
        .await
        .expect("resolve")
        .expect("the expired row is still the decisive record");
    assert_eq!(row.id, old.id);
    assert!(row.is_expired(time::OffsetDateTime::now_utc()));
    assert_eq!(
        purged(&repo).await,
        vec![StoreKey::new(TenantId(tenant), new.id)],
        "only the loser's own fresh key is purged"
    );
}

// ── write-intent journal: begin / drop / settle / heal ──────────────────────

/// Inserts an intent row directly, with an explicit `lease_until`.
async fn insert_raw_intent(
    repo: &SecretRepoImpl,
    attempt_id: Uuid,
    key: &StoreKey,
    reference: &str,
    lease_until: time::OffsetDateTime,
) -> Result<(), ScopeError> {
    let conn = repo.db.conn().expect("conn");
    entity::write_intents::Entity::insert(entity::write_intents::ActiveModel {
        attempt_id: ActiveValue::Set(attempt_id),
        tenant_id: ActiveValue::Set(key.tenant_id.0),
        record_id: ActiveValue::Set(key.record_id),
        reference: ActiveValue::Set(reference.to_owned()),
        lease_until: ActiveValue::Set(lease_until),
    })
    .secure()
    .scope_unchecked(&AccessScope::allow_all())?
    .exec(&conn)
    .await
    .map(|_| ())
}

fn hours_ago(h: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() - time::Duration::hours(h)
}

fn some_key(tenant: Uuid) -> StoreKey {
    StoreKey::new(TenantId(tenant), Uuid::new_v4())
}

#[tokio::test]
async fn migration_creates_the_write_intents_table_keyed_by_attempt_id() {
    let repo = setup().await;
    let key = some_key(Uuid::new_v4());
    let attempt = Uuid::new_v4();
    insert_raw_intent(&repo, attempt, &key, "k", hours_ago(-1))
        .await
        .expect("the table exists");

    let err = insert_raw_intent(
        &repo,
        attempt,
        &some_key(Uuid::new_v4()),
        "k",
        hours_ago(-1),
    )
    .await
    .expect_err("attempt_id is the primary key");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected a uniqueness violation, got: {err}"
    );
    assert_eq!(intent_ids(&repo).await, vec![attempt]);
}

#[tokio::test]
async fn begin_write_intent_stores_the_key_the_reference_and_a_database_clock_lease() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let attempt = attempt_for(tenant, Uuid::new_v4());
    let before = time::OffsetDateTime::now_utc();

    repo.begin_write_intent(&attempt, Duration::from_mins(5))
        .await
        .expect("begin");

    let conn = repo.db.conn().expect("conn");
    let rows = entity::write_intents::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("read");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.attempt_id, attempt.attempt_id);
    assert_eq!(row.tenant_id, attempt.key.tenant_id.0);
    assert_eq!(row.record_id, attempt.key.record_id);
    assert_eq!(row.reference, attempt.reference);
    let lease = row.lease_until - before;
    assert!(
        lease > time::Duration::seconds(290) && lease < time::Duration::seconds(310),
        "lease_until is now + 300s on the database clock, got +{lease}"
    );

    // A second `begin` with the same attempt id is refused: ids are never
    // reused.
    repo.begin_write_intent(&attempt, Duration::from_mins(5))
        .await
        .expect_err("attempt_id is unique");
}

// ── cleanup debts: pending / delete ─────────────────────────────────────────

#[tokio::test]
async fn pending_debts_are_the_records_own_rows_and_delete_debt_is_idempotent() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let mine = some_key(tenant);
    let other = some_key(tenant);
    let mine_debts = record_tasks(&repo, vec![CleanupTask::Purge(mine.clone())]).await;
    record_tasks(&repo, vec![CleanupTask::Purge(other.clone())]).await;

    let pending = repo.pending_debts(&mine).await.expect("pending");
    assert_eq!(
        pending, mine_debts,
        "only the record's own debt, with its id"
    );
    assert_eq!(pending[0].task, CleanupTask::Purge(mine.clone()));

    repo.delete_debt(pending[0].id).await.expect("delete");
    assert!(repo.pending_debts(&mine).await.expect("pending").is_empty());
    assert_eq!(
        repo.pending_debts(&other).await.expect("pending").len(),
        1,
        "the other record's debt is untouched"
    );
    repo.delete_debt(pending[0].id)
        .await
        .expect("deleting an already-deleted debt row is not an error");
}

// ── heal on access ──────────────────────────────────────────────────────────

#[tokio::test]
async fn row_reads_return_the_heal_flags_from_the_same_query() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;
    let key = StoreKey::new(TenantId(tenant), id);

    let flags = |row: Option<SecretRow>| row.expect("row").heal;
    let read_all = || async {
        let get = repo
            .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("k"), &[tenant])
            .await
            .expect("resolve_for_get");
        let own = repo
            .find_own(&scope, TenantId(tenant), OwnerId(owner), &sref("k"))
            .await
            .expect("find_own");
        let write = repo
            .find_for_write(
                &scope,
                TenantId(tenant),
                OwnerId(owner),
                &sref("k"),
                SharingMode::Tenant,
            )
            .await
            .expect("find_for_write");
        [flags(get), flags(own), flags(write)]
    };

    // Nothing pending.
    for heal in read_all().await {
        assert_eq!(heal, HealFlags::default());
    }

    // A live (unexpired) intent is not an expired one.
    let live = begin(&repo, tenant, id).await;
    for heal in read_all().await {
        assert!(!heal.expired_intents && !heal.debts, "{heal:?}");
    }

    // An intent whose lease is over, on the database clock.
    insert_raw_intent(&repo, Uuid::new_v4(), &key, "k", hours_ago(1))
        .await
        .expect("seed");
    for heal in read_all().await {
        assert!(heal.expired_intents && !heal.debts, "{heal:?}");
    }

    // A pending debt of the record (and none for another record's).
    let debts = record_tasks(
        &repo,
        vec![CleanupTask::Destroy {
            key: key.clone(),
            selector: DestroySelector::Exactly(vv("2")),
        }],
    )
    .await;
    record_tasks(&repo, vec![CleanupTask::Purge(some_key(tenant))]).await;
    for heal in read_all().await {
        assert!(heal.expired_intents && heal.debts, "{heal:?}");
    }

    // Executed debts leave the flags.
    for debt in &debts {
        repo.delete_debt(debt.id).await.expect("delete");
    }
    for heal in read_all().await {
        assert!(heal.expired_intents && !heal.debts, "{heal:?}");
    }
    drop(live);
}

#[tokio::test]
async fn the_next_secret_write_deletes_the_records_expired_intents_and_only_those() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;
    let key = StoreKey::new(TenantId(tenant), id);
    let (expired_a, expired_b) = (Uuid::new_v4(), Uuid::new_v4());
    let other_expired = Uuid::new_v4();
    insert_raw_intent(&repo, expired_a, &key, "k", hours_ago(2))
        .await
        .expect("seed");
    insert_raw_intent(&repo, expired_b, &key, "k", hours_ago(1))
        .await
        .expect("seed");
    insert_raw_intent(&repo, other_expired, &some_key(tenant), "k", hours_ago(1))
        .await
        .expect("seed");
    // Another writer of the same record, still within its lease.
    let concurrent = begin(&repo, tenant, id).await;

    let attempt = WriteAttempt {
        heal_expired_intents: true,
        ..begin(&repo, tenant, id).await
    };
    let outcome = repo
        .switch_value(
            &AccessScope::for_tenant(tenant),
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("2"),
            &attempt,
        )
        .await
        .expect("switch_value");
    let IntentCommit::Committed { healed, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(healed, 2, "both expired intents of the record");
    let mut left = intent_ids(&repo).await;
    left.sort();
    let mut expected = vec![other_expired, concurrent.attempt_id];
    expected.sort();
    assert_eq!(
        left, expected,
        "the writer's own intent, the other record's expired one and the live one"
    );
}

#[tokio::test]
async fn a_write_that_was_not_told_of_expired_intents_leaves_them() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;
    let key = StoreKey::new(TenantId(tenant), id);
    let expired = Uuid::new_v4();
    insert_raw_intent(&repo, expired, &key, "k", hours_ago(1))
        .await
        .expect("seed");

    let outcome = switch_with_intent(&repo, tenant, id, 1, "2").await;
    let IntentCommit::Committed { healed, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(healed, 0);
    assert_eq!(intent_ids(&repo).await, vec![expired]);
}

#[tokio::test]
async fn heal_by_the_next_write_makes_a_stalled_writers_commit_intent_lost() {
    // End to end on SQL: a writer announces and stalls,
    // another writer of the record commits and heals the expired intent, the
    // stalled writer's own commit finds its intent gone.
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let stalled = attempt_for(tenant, id);
    insert_raw_intent(&repo, stalled.attempt_id, &stalled.key, "k", hours_ago(1))
        .await
        .expect("seed");
    let healer = WriteAttempt {
        heal_expired_intents: true,
        ..begin(&repo, tenant, id).await
    };
    let outcome = repo
        .switch_value(
            &AccessScope::for_tenant(tenant),
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("2"),
            &healer,
        )
        .await
        .expect("switch_value");
    assert!(
        matches!(outcome, IntentCommit::Committed { healed: 1, .. }),
        "{outcome:?}"
    );

    let outcome = repo
        .switch_value(
            &AccessScope::for_tenant(tenant),
            id,
            2,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("1"),
            &stalled,
        )
        .await
        .expect("switch_value");
    assert!(matches!(outcome, IntentCommit::IntentLost), "{outcome:?}");
    let row = repo
        .find_own(
            &AccessScope::for_tenant(tenant),
            TenantId(tenant),
            OwnerId(owner),
            &sref("k"),
        )
        .await
        .expect("find_own")
        .expect("row");
    assert_eq!(row.value_version, Some(vv("2")), "the stalled writer lost");
}

#[tokio::test]
async fn heal_failed_creates_removes_expired_intents_without_a_row_and_purges_their_keys() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let reference = sref("wanted");

    // Expired intent of the reference whose create never inserted a row.
    let dead_key = some_key(tenant);
    insert_raw_intent(&repo, Uuid::new_v4(), &dead_key, "wanted", hours_ago(1))
        .await
        .expect("seed");
    // Two attempts of one dead record: one purge, two intents.
    let dead_twice = some_key(tenant);
    for hours in [3, 2] {
        insert_raw_intent(
            &repo,
            Uuid::new_v4(),
            &dead_twice,
            "wanted",
            hours_ago(hours),
        )
        .await
        .expect("seed");
    }
    // Not to be touched: a live lease, another reference, another tenant, and
    // an expired intent whose record has a row.
    let live = begin(&repo, tenant, Uuid::new_v4()).await;
    let live = WriteAttempt {
        reference: "wanted".to_owned(),
        ..live
    };
    let _ = live;
    let other_ref = Uuid::new_v4();
    insert_raw_intent(&repo, other_ref, &some_key(tenant), "other", hours_ago(1))
        .await
        .expect("seed");
    let other_tenant = Uuid::new_v4();
    insert_raw_intent(
        &repo,
        other_tenant,
        &some_key(Uuid::new_v4()),
        "wanted",
        hours_ago(1),
    )
    .await
    .expect("seed");
    let (live_id, _) = seed_active(&repo, tenant, owner, "wanted", SharingMode::Tenant).await;
    let with_row = Uuid::new_v4();
    insert_raw_intent(
        &repo,
        with_row,
        &StoreKey::new(TenantId(tenant), live_id),
        "wanted",
        hours_ago(1),
    )
    .await
    .expect("seed");
    let live_attempt = intent_ids(&repo).await;
    assert_eq!(live_attempt.len(), 7);

    let healed = repo
        .heal_failed_creates(TenantId(tenant), &reference)
        .await
        .expect("heal");

    assert_eq!(healed.intents, 3, "the three expired intents without a row");
    let mut keys = vec![dead_key, dead_twice];
    keys.sort_by_key(|k| k.record_id);
    let mut purged_keys: Vec<StoreKey> = purged(&repo).await;
    purged_keys.sort_by_key(|k| k.record_id);
    assert_eq!(purged_keys, keys, "one purge per distinct dead key");
    assert_eq!(
        tasks_of(healed.debts).len(),
        2,
        "the returned debts are the recorded ones"
    );
    let left = intent_ids(&repo).await;
    assert_eq!(left.len(), 4);
    for kept in [other_ref, other_tenant, with_row] {
        assert!(left.contains(&kept), "{kept} must stay");
    }
}

#[tokio::test]
async fn heal_failed_creates_finds_nothing_and_writes_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    // Only a live lease of the reference.
    let live = begin(&repo, tenant, Uuid::new_v4()).await;

    let healed = repo
        .heal_failed_creates(TenantId(tenant), &sref("k"))
        .await
        .expect("heal");

    assert_eq!(healed.intents, 0);
    assert!(healed.debts.is_empty());
    assert_eq!(intent_ids(&repo).await, vec![live.attempt_id]);
    assert!(debt_tasks(&repo).await.is_empty());
}

// ── generated SQL of the PostgreSQL-specific statements ──────────────────────
//
// The SQLite tests above run the same builders, but the PostgreSQL text
// (database-clock lease, a target-less `ON CONFLICT DO NOTHING`) is pinned here without a server.

#[test]
fn postgres_begin_intent_uses_the_database_clock() {
    use sea_orm::sea_query::{PostgresQueryBuilder, Query};

    let attempt = attempt_for(Uuid::nil(), Uuid::nil());
    let mut insert = Query::insert();
    insert
        .into_table(entity::write_intents::Entity)
        .columns([
            entity::write_intents::Column::AttemptId,
            entity::write_intents::Column::TenantId,
            entity::write_intents::Column::RecordId,
            entity::write_intents::Column::Reference,
            entity::write_intents::Column::LeaseUntil,
        ])
        .select_from(super::intents::begin_intent_source(
            sea_orm::DbBackend::Postgres,
            &attempt,
            Duration::from_mins(5),
        ))
        .expect("column count matches");
    let sql = insert.to_string(PostgresQueryBuilder);
    assert!(
        sql.starts_with(
            r#"INSERT INTO "credstore_write_intents" ("attempt_id", "tenant_id", "record_id", "reference", "lease_until") SELECT "#
        ),
        "{sql}"
    );
    assert!(
        sql.contains("now() + make_interval(0, 0, 0, 0, 0, 0, 300"),
        "lease_until must be the database clock plus the lease: {sql}"
    );

    // The prepared form binds the lease as the fifth parameter: the
    // placeholder syntax is the backend's own (`$n`).
    let (prepared, values) = insert.build(PostgresQueryBuilder);
    assert!(
        prepared.ends_with("now() + make_interval(0, 0, 0, 0, 0, 0, $5)"),
        "{prepared}"
    );
    assert_eq!(values.0.len(), 5);
}

#[test]
fn the_create_insert_does_nothing_on_any_unique_conflict() {
    use sea_orm::QueryTrait;

    let tenant = Uuid::new_v4();
    let new = new_secret(tenant, Uuid::new_v4(), "k", SharingMode::Tenant, vv("1"));
    let am = entity::secrets::ActiveModel {
        id: ActiveValue::Set(new.id),
        ..Default::default()
    };
    for backend in [sea_orm::DbBackend::Postgres, sea_orm::DbBackend::Sqlite] {
        let sql = super::writes::insert_unless_taken(am.clone())
            .build(backend)
            .to_string();
        // No conflict target (the builder leaves a double space): whichever
        // unique index the reference hits is skipped.
        let normalized = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("ON CONFLICT DO NOTHING"),
            "no conflict target, so any unique index conflict is skipped: {sql}"
        );
    }
}

// ── write protocol: insert_declared (ADR-0007, "PUT: whole replace, secret tri-state") ──────────────────

fn new_declared_secret(
    tenant: Uuid,
    owner: Uuid,
    key: &str,
    sharing: SharingMode,
    fallback: Fallback,
) -> NewDeclaredSecret {
    NewDeclaredSecret {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: sref(key),
        sharing,
        owner_id: OwnerId(owner),
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        fallback,
    }
}

#[tokio::test]
async fn insert_declared_creates_a_value_less_row() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let new = new_declared_secret(
        tenant,
        owner,
        "declared",
        SharingMode::Tenant,
        Fallback::None,
    );
    let id = new.id;

    repo.insert_declared(&scope, &new)
        .await
        .expect("insert_declared");

    let row = repo
        .resolve_for_get(
            TenantId(tenant),
            OwnerId(owner),
            &sref("declared"),
            &[tenant],
        )
        .await
        .expect("resolve_for_get")
        .expect("declared/none row resolves and competes as a winner");
    assert_eq!(row.id, id);
    assert_eq!(row.status, SecretStatus::Declared);
    assert_eq!(row.fallback, Fallback::None);
    assert!(row.value_version.is_none());
    assert_eq!(row.version, 1);
}

#[tokio::test]
async fn insert_declared_enqueues_no_purge() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let new = new_declared_secret(
        tenant,
        owner,
        "declared-nopurge",
        SharingMode::Tenant,
        Fallback::Inherit,
    );

    repo.insert_declared(&scope, &new)
        .await
        .expect("insert_declared");

    assert!(
        purged(&repo).await.is_empty(),
        "a value-less create touches no store key"
    );
}

#[tokio::test]
async fn insert_declared_duplicate_nonprivate_conflicts() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed_active(&repo, tenant, owner, "dup-declared", SharingMode::Tenant).await;

    let new = new_declared_secret(
        tenant,
        owner,
        "dup-declared",
        SharingMode::Tenant,
        Fallback::Inherit,
    );
    let err = repo
        .insert_declared(&AccessScope::for_tenant(tenant), &new)
        .await
        .expect_err("duplicate non-private insert violates unique index");
    assert!(matches!(err, DomainError::Conflict));
}

// ── write protocol: switch_value ─────────────────────────────────────────────

#[tokio::test]
async fn switch_value_is_a_cas_that_bumps_the_row_version_and_moves_the_pointer() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _old) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let IntentCommit::Committed { value: row, .. } =
        switch_with_intent(&repo, tenant, id, 1, "2").await
    else {
        panic!("the CAS on the version read first must commit");
    };
    assert_eq!(row.version, 2);
    assert_eq!(row.value_version, Some(vv("2")));
    assert_eq!(row.status, SecretStatus::Active);
    let _ = scope;
}

#[tokio::test]
async fn switch_value_version_mismatch_is_a_definite_loss_that_destroys_its_own_version() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, old_value) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let outcome = switch_with_intent(&repo, tenant, id, 99 /* stale */, "2").await;
    let IntentCommit::Lost { debts } = outcome else {
        panic!("a stale CAS is a definite loss: {outcome:?}");
    };
    // The row still exists, so only the loser's own version is dead.
    let key = StoreKey::new(TenantId(tenant), id);
    let expected = vec![CleanupTask::Destroy {
        key,
        selector: DestroySelector::Exactly(vv("2")),
    }];
    assert_eq!(tasks_of(debts), expected);
    assert_eq!(debt_tasks(&repo).await, expected);
    assert!(
        intent_ids(&repo).await.is_empty(),
        "the intent deletion commits with the loss"
    );

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("k"), &[tenant])
        .await
        .expect("resolve")
        .expect("row");
    assert_eq!(row.value_version, Some(old_value));
    assert_eq!(row.version, 1);
}

#[tokio::test]
async fn switch_value_loss_without_destroy_support_records_nothing_for_a_live_row() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let attempt = WriteAttempt {
        destroy_supported: false,
        ..begin(&repo, tenant, id).await
    };
    let outcome = repo
        .switch_value(
            &AccessScope::for_tenant(tenant),
            id,
            99,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("2"),
            &attempt,
        )
        .await
        .expect("switch_value");
    let IntentCommit::Lost { debts } = outcome else {
        panic!("a stale CAS is a definite loss: {outcome:?}");
    };
    assert!(debts.is_empty());
    assert!(debt_tasks(&repo).await.is_empty());
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn switch_value_missing_row_is_a_definite_loss_that_purges_the_key() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();

    let outcome = switch_with_intent(&repo, tenant, id, 1, "2").await;
    let IntentCommit::Lost { debts } = outcome else {
        panic!("a missing row loses the CAS: {outcome:?}");
    };
    let expected = vec![CleanupTask::Purge(StoreKey::new(TenantId(tenant), id))];
    assert_eq!(tasks_of(debts), expected);
    assert_eq!(debt_tasks(&repo).await, expected);
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn switch_value_commit_enqueues_destroy_below_the_new_version_with_the_intent_deletion() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let outcome = switch_with_intent(&repo, tenant, id, 1, "2").await;
    let IntentCommit::Committed { debts, .. } = outcome else {
        panic!("{outcome:?}");
    };
    let expected = vec![CleanupTask::Destroy {
        key: StoreKey::new(TenantId(tenant), id),
        selector: DestroySelector::Below(vv("2")),
    }];
    assert_eq!(tasks_of(debts), expected);
    assert_eq!(debt_tasks(&repo).await, expected);
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn switch_value_with_a_healed_intent_changes_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, old_value) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    // The attempt's intent is gone before its commit (healed).
    let attempt = begin(&repo, tenant, id).await;
    drop_intent(&repo, attempt.attempt_id).await;
    let outcome = repo
        .switch_value(
            &AccessScope::for_tenant(tenant),
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("2"),
            &attempt,
        )
        .await
        .expect("switch_value");
    assert!(matches!(outcome, IntentCommit::IntentLost), "{outcome:?}");

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("k"), &[tenant])
        .await
        .expect("resolve")
        .expect("row");
    assert_eq!(row.value_version, Some(old_value), "the row is unchanged");
    assert_eq!(row.version, 1);
    assert!(debt_tasks(&repo).await.is_empty(), "no debt recorded");
}

#[tokio::test]
async fn insert_active_with_a_healed_intent_changes_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let new = new_secret(tenant, owner, "k", SharingMode::Tenant, vv("1"));

    let attempt = begin(&repo, tenant, new.id).await;
    drop_intent(&repo, attempt.attempt_id).await;
    let outcome = repo
        .insert_active(&scope, &new, &attempt)
        .await
        .expect("insert_active");
    assert!(matches!(outcome, IntentCommit::IntentLost), "{outcome:?}");
    assert!(
        repo.find_own(&scope, TenantId(tenant), OwnerId(owner), &sref("k"))
            .await
            .expect("find_own")
            .is_none(),
        "no row was inserted"
    );
    assert!(debt_tasks(&repo).await.is_empty());
}

// ── transaction retry ────────────────────────────────────────────────────────

#[tokio::test]
async fn run_tx_runs_the_body_again_after_a_definite_rollback_and_commits_once() {
    use std::sync::atomic::{AtomicU32, Ordering};

    use sea_orm::{DbErr, RuntimeErr};

    use crate::infra::canonical_mapping::classify_db_err_to_domain;
    use crate::infra::storage::repo_impl::helpers::TxFuture;

    let repo = setup().await;
    let attempts = Arc::new(AtomicU32::new(0));
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let attempt = attempt_for(tenant, id);

    let counter = Arc::clone(&attempts);
    let intent_attempt = attempt.clone();
    let result: Result<(), DomainError> = repo
        .run_tx(move |tx| {
            let counter = Arc::clone(&counter);
            let attempt = intent_attempt.clone();
            Box::pin(async move {
                // The body writes, then the first run is rolled back as a
                // busy database; only the second run commits.
                crate::infra::storage::repo_impl::intents::begin_intent_tx(
                    tx,
                    sea_orm::DbBackend::Sqlite,
                    &attempt,
                    LEASE,
                )
                .await?;
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(classify_db_err_to_domain(DbErr::Exec(
                        RuntimeErr::Internal(
                            "Execution Error: error returned from database: (code: 5) database \
                             is locked"
                                .to_owned(),
                        ),
                    )));
                }
                Ok(())
            }) as TxFuture<'_, ()>
        })
        .await;
    result.expect("the second run commits");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        intent_ids(&repo).await,
        [attempt.attempt_id],
        "the rolled-back run left nothing: one intent, written by the committed run"
    );

    // A failure that is not a definite rollback is never run again.
    let counter = Arc::clone(&attempts);
    counter.store(0, Ordering::SeqCst);
    let result: Result<(), DomainError> = repo
        .run_tx(move |_tx| {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(classify_db_err_to_domain(DbErr::Custom(
                    "connection reset by peer".to_owned(),
                )))
            }) as TxFuture<'_, ()>
        })
        .await;
    assert!(result.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "not retried");
}

// ── verification after an ambiguous commit ───────────────────────────────────

/// The row of record `id`, whatever its status.
async fn row_of(repo: &SecretRepoImpl, tenant: Uuid, id: Uuid) -> Option<SecretRow> {
    repo.resolve_candidates(
        TenantId(tenant),
        OwnerId(Uuid::nil()),
        &sref("k"),
        &[tenant],
    )
    .await
    .expect("resolve_candidates")
    .into_iter()
    .find(|r| r.id == id)
}

#[tokio::test]
async fn verify_insert_active_with_the_intent_present_runs_tx1_again() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let new = new_secret(tenant, Uuid::new_v4(), "k", SharingMode::Tenant, vv("1"));
    let attempt = begin(&repo, tenant, new.id).await;

    let out = repo
        .verify_insert_active(&scope, &new, &attempt)
        .await
        .expect("verify");
    assert!(
        matches!(
            out,
            WriteVerification::Retried(IntentCommit::Committed { .. })
        ),
        "{out:?}"
    );
    assert!(intent_ids(&repo).await.is_empty());
    let row = row_of(&repo, tenant, new.id).await.expect("row");
    assert_eq!(row.value_version, Some(vv("1")));
}

#[tokio::test]
async fn verify_insert_active_sees_a_committed_row_and_changes_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let new = new_secret(tenant, Uuid::new_v4(), "k", SharingMode::Tenant, vv("1"));
    let attempt = begin(&repo, tenant, new.id).await;
    repo.insert_active(&scope, &new, &attempt)
        .await
        .expect("insert_active");

    let out = repo
        .verify_insert_active(&scope, &new, &attempt)
        .await
        .expect("verify");
    let WriteVerification::Committed { row } = out else {
        panic!("{out:?}");
    };
    assert_eq!((row.id, row.version), (new.id, 1));
    assert!(debt_tasks(&repo).await.is_empty(), "no debt recorded");
}

#[tokio::test]
async fn verify_insert_active_without_intent_or_row_records_a_purge() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let new = new_secret(tenant, Uuid::new_v4(), "k", SharingMode::Tenant, vv("1"));
    let attempt = begin(&repo, tenant, new.id).await;
    drop_intent(&repo, attempt.attempt_id).await;

    let out = repo
        .verify_insert_active(&scope, &new, &attempt)
        .await
        .expect("verify");
    let WriteVerification::NotApplied { debts } = out else {
        panic!("{out:?}");
    };
    let expected = vec![CleanupTask::Purge(attempt.key.clone())];
    assert_eq!(tasks_of(debts), expected);
    assert_eq!(debt_tasks(&repo).await, expected);
    assert!(row_of(&repo, tenant, new.id).await.is_none());
}

#[tokio::test]
async fn verify_switch_value_with_the_intent_present_runs_tx1_again() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, Uuid::new_v4(), "k", SharingMode::Tenant).await;
    let attempt = begin(&repo, tenant, id).await;

    let out = repo
        .verify_switch_value(
            &AccessScope::for_tenant(tenant),
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("2"),
            &attempt,
        )
        .await
        .expect("verify");
    let WriteVerification::Retried(IntentCommit::Committed {
        value: row, debts, ..
    }) = out
    else {
        panic!("{out:?}");
    };
    assert_eq!((row.version, row.value_version), (2, Some(vv("2"))));
    assert_eq!(
        tasks_of(debts),
        vec![CleanupTask::Destroy {
            key: attempt.key.clone(),
            selector: DestroySelector::Below(vv("2")),
        }]
    );
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn verify_switch_value_with_the_intent_present_and_a_changed_row_records_the_loss() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, Uuid::new_v4(), "k", SharingMode::Tenant).await;
    let attempt = begin(&repo, tenant, id).await;

    // The caller's base version (1) is stale: the row is at 2.
    let committed = switch_with_intent(&repo, tenant, id, 1, "other").await;
    assert!(matches!(committed, IntentCommit::Committed { .. }));
    let out = repo
        .verify_switch_value(
            &AccessScope::for_tenant(tenant),
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("mine"),
            &attempt,
        )
        .await
        .expect("verify");
    let WriteVerification::Retried(IntentCommit::Lost { debts }) = out else {
        panic!("{out:?}");
    };
    assert_eq!(
        tasks_of(debts),
        vec![CleanupTask::Destroy {
            key: attempt.key.clone(),
            selector: DestroySelector::Exactly(vv("mine")),
        }]
    );
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn verify_switch_value_sees_the_committed_pointer_and_records_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, Uuid::new_v4(), "k", SharingMode::Tenant).await;
    let attempt = begin(&repo, tenant, id).await;
    let scope = AccessScope::for_tenant(tenant);
    repo.switch_value(
        &scope,
        id,
        1,
        SharingMode::Tenant,
        Fallback::Inherit,
        None,
        vv("2"),
        &attempt,
    )
    .await
    .expect("switch_value");
    let debts_before = debt_tasks(&repo).await;

    let out = repo
        .verify_switch_value(
            &scope,
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("2"),
            &attempt,
        )
        .await
        .expect("verify");
    let WriteVerification::Committed { row } = out else {
        panic!("{out:?}");
    };
    assert_eq!((row.version, row.value_version), (2, Some(vv("2"))));
    assert_eq!(
        debt_tasks(&repo).await,
        debts_before,
        "nothing new recorded"
    );
}

#[tokio::test]
async fn verify_switch_value_destroys_exactly_only_when_no_row_points_at_the_version() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, Uuid::new_v4(), "k", SharingMode::Tenant).await;
    let scope = AccessScope::for_tenant(tenant);

    // The intent is gone (healed) and the row is at version "1", not "2":
    // the attempt did not take effect.
    let attempt = begin(&repo, tenant, id).await;
    drop_intent(&repo, attempt.attempt_id).await;
    let out = repo
        .verify_switch_value(
            &scope,
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("2"),
            &attempt,
        )
        .await
        .expect("verify");
    let WriteVerification::NotApplied { debts } = out else {
        panic!("{out:?}");
    };
    assert_eq!(
        tasks_of(debts),
        vec![CleanupTask::Destroy {
            key: attempt.key.clone(),
            selector: DestroySelector::Exactly(vv("2")),
        }]
    );

    // A plugin without `destroy`: nothing to record for a live row.
    let attempt = WriteAttempt {
        destroy_supported: false,
        ..begin(&repo, tenant, id).await
    };
    drop_intent(&repo, attempt.attempt_id).await;
    let out = repo
        .verify_switch_value(
            &scope,
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("3"),
            &attempt,
        )
        .await
        .expect("verify");
    let WriteVerification::NotApplied { debts } = out else {
        panic!("{out:?}");
    };
    assert!(debts.is_empty());

    // The row now points at "4": the verification of that attempt (intent
    // gone) is a commit, never a destroy of "4".
    let attempt = begin(&repo, tenant, id).await;
    repo.switch_value(
        &scope,
        id,
        1,
        SharingMode::Tenant,
        Fallback::Inherit,
        None,
        vv("4"),
        &attempt,
    )
    .await
    .expect("switch_value");
    let recorded = debt_tasks(&repo).await;
    let out = repo
        .verify_switch_value(
            &scope,
            id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            vv("4"),
            &attempt,
        )
        .await
        .expect("verify");
    assert!(
        matches!(out, WriteVerification::Committed { .. }),
        "{out:?}"
    );
    assert_eq!(debt_tasks(&repo).await, recorded);
    assert!(
        !debt_tasks(&repo).await.contains(&CleanupTask::Destroy {
            key: attempt.key.clone(),
            selector: DestroySelector::Exactly(vv("4")),
        }),
        "no destroy of the live pointer"
    );
}

#[tokio::test]
async fn verify_delete_branches() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, Uuid::new_v4(), "k", SharingMode::Tenant).await;
    let key = StoreKey::new(TenantId(tenant), id);

    // A changed row answers as a precondition failure and nothing is deleted.
    let out = repo
        .verify_delete(&scope, &key, Some(7))
        .await
        .expect("verify");
    assert!(
        matches!(out, DeleteVerification::PreconditionFailed),
        "{out:?}"
    );
    assert!(row_of(&repo, tenant, id).await.is_some());
    assert!(debt_tasks(&repo).await.is_empty());

    // The row is present: the delete did not commit, so it runs here.
    let out = repo
        .verify_delete(&scope, &key, Some(1))
        .await
        .expect("verify");
    let DeleteVerification::Retried { debts } = out else {
        panic!("{out:?}");
    };
    assert_eq!(tasks_of(debts), vec![CleanupTask::Purge(key.clone())]);
    assert!(row_of(&repo, tenant, id).await.is_none());
    assert_eq!(purged(&repo).await, std::slice::from_ref(&key));

    // The row is gone: the delete committed; nothing more is recorded.
    let out = repo
        .verify_delete(&scope, &key, Some(1))
        .await
        .expect("verify");
    assert!(matches!(out, DeleteVerification::Committed), "{out:?}");
    assert_eq!(purged(&repo).await, [key]);
}

// ── write protocol: delete_by_id ─────────────────────────────────────────────

#[tokio::test]
async fn delete_by_id_enqueues_the_key_purge_and_then_not_found() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "gone", SharingMode::Tenant).await;
    let key = StoreKey::new(TenantId(tenant), id);

    repo.delete_by_id(&scope, &key, None).await.expect("delete");
    assert_eq!(purged(&repo).await, vec![key.clone()]);

    let err = repo
        .delete_by_id(&scope, &key, None)
        .await
        .expect_err("second delete is NotFound");
    assert!(matches!(err, DomainError::NotFound));
    assert_eq!(
        purged(&repo).await.len(),
        1,
        "no second purge for a missing row"
    );
}

#[tokio::test]
async fn delete_by_id_with_stale_expected_version_is_not_found_and_enqueues_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _value) = seed_active(&repo, tenant, owner, "ver-del", SharingMode::Tenant).await;
    let scope = AccessScope::for_tenant(tenant);
    let key = StoreKey::new(TenantId(tenant), id);

    let err = repo
        .delete_by_id(&scope, &key, Some(99))
        .await
        .expect_err("stale expected_version must match 0 rows");
    assert!(matches!(err, DomainError::NotFound));
    assert!(purged(&repo).await.is_empty());

    repo.delete_by_id(&scope, &key, Some(1))
        .await
        .expect("matching expected_version deletes");
    assert_eq!(purged(&repo).await, vec![key]);
}

#[tokio::test]
async fn delete_then_create_only_put_under_the_same_reference_succeeds() {
    // No name retention (ADR-0006): a successor mints its own record id and
    // store key, so it can never collide with the predecessor's lagging purge.
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _old) = seed_active(&repo, tenant, owner, "reused", SharingMode::Tenant).await;

    repo.delete_by_id(&scope, &StoreKey::new(TenantId(tenant), id), None)
        .await
        .expect("delete");

    let new = new_secret(tenant, owner, "reused", SharingMode::Tenant, vv("1"));
    insert_ok(&repo, &scope, &new).await;

    let row = repo
        .resolve_for_get(TenantId(tenant), OwnerId(owner), &sref("reused"), &[tenant])
        .await
        .expect("resolve")
        .expect("row");
    assert_eq!(row.id, new.id);
    assert_ne!(row.id, id);
    assert_eq!(
        purged(&repo).await,
        vec![StoreKey::new(TenantId(tenant), id)],
        "only the old key is purged"
    );
}

// ── read path (unchanged logic, adapted fixtures) ────────────────────────────

#[tokio::test]
async fn find_own_matches_private_owner_and_tenant_shared() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    seed_active(&repo, tenant, owner, "priv", SharingMode::Private).await;
    let own = repo
        .find_own(&scope, TenantId(tenant), OwnerId(owner), &sref("priv"))
        .await
        .expect("find_own")
        .expect("private row found by its owner");
    assert_eq!(own.sharing, SharingMode::Private);

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

    seed_active(&repo, tenant, owner, "dup", SharingMode::Tenant).await;
    seed_active(&repo, tenant, owner, "dup", SharingMode::Private).await;

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
async fn resolve_for_get_prefers_closest_tenant_then_private() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    seed_active(&repo, parent, owner, "cfg", SharingMode::Shared).await;
    seed_active(&repo, child, owner, "cfg", SharingMode::Private).await;

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

#[tokio::test]
async fn resolve_for_get_excludes_parent_tenant_mode_but_inherits_shared() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    seed_active(&repo, parent, owner, "tenant-only", SharingMode::Tenant).await;
    seed_active(&repo, parent, owner, "shared-cfg", SharingMode::Shared).await;

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
    assert_eq!(shared.tenant_id, TenantId(parent));
    assert_ne!(shared.tenant_id, TenantId(child));
}

#[tokio::test]
async fn resolve_for_get_never_inherits_ancestor_private_even_for_same_owner() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
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

#[tokio::test]
async fn secret_type_round_trips_through_storage() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let mut new = new_secret(tenant, owner, "typed", SharingMode::Private, vv("1"));
    new.secret_type_uuid = SecretType::from_name("personal-token")
        .expect("known")
        .uuid();
    insert_ok(&repo, &scope, &new).await;

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
async fn scope_includes_tenant_structured_subtree_fails_closed() {
    let repo = setup().await;
    let root = Uuid::new_v4();
    let scope = subtree_scope(root);
    assert!(
        !repo
            .scope_includes_tenant(&scope, root)
            .await
            .expect("structured subtree predicate must fail closed")
    );
    assert!(
        !repo
            .scope_includes_tenant(&scope, Uuid::new_v4())
            .await
            .expect("structured subtree predicate must fail closed for any tenant")
    );
}

#[tokio::test]
async fn scope_includes_tenant_sibling_owner_filter_fails_closed() {
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

// ── ADR-0007: update_metadata, remove_value, widened find_own/find_for_write,
//    widened resolve_for_get (suppression), resolve_candidates ──────────────

#[tokio::test]
async fn update_metadata_bumps_version_and_leaves_value_untouched() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, value_version) = seed_active(&repo, tenant, owner, "meta", SharingMode::Tenant).await;

    let row = repo
        .update_metadata(
            &scope,
            id,
            Some(1),
            SharingMode::Shared,
            Fallback::None,
            None,
        )
        .await
        .expect("update_metadata")
        .expect("row updated");
    assert_eq!(row.version, 2);
    assert_eq!(row.sharing, SharingMode::Shared);
    assert_eq!(row.fallback, Fallback::None);
    assert_eq!(row.value_version, Some(value_version), "value untouched");
    assert_eq!(row.status, SecretStatus::Active, "status untouched");
}

#[tokio::test]
async fn update_metadata_version_mismatch_returns_none() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "meta-stale", SharingMode::Tenant).await;

    let result = repo
        .update_metadata(
            &scope,
            id,
            Some(99),
            SharingMode::Shared,
            Fallback::None,
            None,
        )
        .await
        .expect("update_metadata");
    assert!(result.is_none());
}

#[tokio::test]
async fn remove_value_declares_row_and_records_destroy_below_and_exactly_the_old_version() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, value_version) = seed_active(&repo, tenant, owner, "rm", SharingMode::Tenant).await;

    let (row, tasks) = repo
        .remove_value(
            &scope,
            id,
            Some(1),
            SharingMode::Tenant,
            Fallback::None,
            None,
            true,
        )
        .await
        .expect("remove_value")
        .expect("row updated");
    assert_eq!(row.status, SecretStatus::Declared);
    assert_eq!(row.value_version, None);
    assert_eq!(row.fallback, Fallback::None);
    assert_eq!(row.version, 2);
    let key = StoreKey::new(TenantId(tenant), id);
    let expected = vec![
        CleanupTask::Destroy {
            key: key.clone(),
            selector: DestroySelector::Below(value_version.clone()),
        },
        CleanupTask::Destroy {
            key,
            selector: DestroySelector::Exactly(value_version),
        },
    ];
    assert_eq!(tasks_of(tasks), expected);
    assert_eq!(
        debt_tasks(&repo).await,
        expected,
        "recorded by the same transaction as the CAS"
    );
}

#[tokio::test]
async fn remove_value_without_destroy_support_records_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "rm-nd", SharingMode::Tenant).await;

    let (row, tasks) = repo
        .remove_value(
            &scope,
            id,
            Some(1),
            SharingMode::Tenant,
            Fallback::None,
            None,
            false,
        )
        .await
        .expect("remove_value")
        .expect("row updated");
    assert_eq!(row.status, SecretStatus::Declared);
    assert!(tasks.is_empty());
    assert!(debt_tasks(&repo).await.is_empty());
}

#[tokio::test]
async fn remove_value_on_already_declared_row_enqueues_nothing() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "rm-twice", SharingMode::Tenant).await;

    let (declared, first) = repo
        .remove_value(
            &scope,
            id,
            Some(1),
            SharingMode::Tenant,
            Fallback::None,
            None,
            true,
        )
        .await
        .expect("remove_value")
        .expect("row updated");
    assert_eq!(declared.status, SecretStatus::Declared);
    assert_eq!(first.len(), 2);

    // Idempotent re-send: metadata equal, no value version to destroy this time.
    let (row, tasks) = repo
        .remove_value(
            &scope,
            id,
            Some(2),
            SharingMode::Tenant,
            Fallback::None,
            None,
            true,
        )
        .await
        .expect("remove_value")
        .expect("row still updated (version bumps)");
    assert_eq!(row.status, SecretStatus::Declared);
    assert!(tasks.is_empty());
    assert_eq!(
        debt_tasks(&repo).await.len(),
        2,
        "only the first removal recorded debts"
    );
}

#[tokio::test]
async fn remove_value_version_mismatch_returns_none() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "rm-stale", SharingMode::Tenant).await;

    let result = repo
        .remove_value(
            &scope,
            id,
            Some(99),
            SharingMode::Tenant,
            Fallback::None,
            None,
            true,
        )
        .await
        .expect("remove_value");
    assert!(result.is_none());
}

#[tokio::test]
async fn switch_value_accepts_a_declared_row_and_reactivates_it() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _old) = seed_active(&repo, tenant, owner, "reactivate", SharingMode::Tenant).await;
    repo.remove_value(
        &scope,
        id,
        Some(1),
        SharingMode::Tenant,
        Fallback::None,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("declared");

    let IntentCommit::Committed { value: row, .. } =
        switch_with_intent(&repo, tenant, id, 2, "2").await
    else {
        panic!("declared row reactivated");
    };
    assert_eq!(row.status, SecretStatus::Active);
    assert_eq!(row.value_version, Some(vv("2")));
    assert_eq!(row.fallback, Fallback::Inherit);
}

#[tokio::test]
async fn find_own_and_find_for_write_see_a_declared_row() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "own-declared", SharingMode::Tenant).await;
    repo.remove_value(
        &scope,
        id,
        Some(1),
        SharingMode::Tenant,
        Fallback::Inherit,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("declared");

    let own = repo
        .find_own(
            &scope,
            TenantId(tenant),
            OwnerId(owner),
            &sref("own-declared"),
        )
        .await
        .expect("find_own")
        .expect("declared row is still an own record");
    assert_eq!(own.status, SecretStatus::Declared);

    let for_write = repo
        .find_for_write(
            &scope,
            TenantId(tenant),
            OwnerId(owner),
            &sref("own-declared"),
            SharingMode::Tenant,
        )
        .await
        .expect("find_for_write")
        .expect("declared row addressed for a create-only conflict check");
    assert_eq!(for_write.id, id);
}

#[tokio::test]
async fn resolve_for_get_suppressed_row_blocks_the_walk() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    seed_active(&repo, parent, owner, "suppressed", SharingMode::Shared).await;
    let (child_id, _) = seed_active(&repo, child, owner, "suppressed", SharingMode::Tenant).await;
    let scope = AccessScope::for_tenant(child);
    repo.remove_value(
        &scope,
        child_id,
        Some(1),
        SharingMode::Tenant,
        Fallback::None,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("declared/none");

    let resolved = repo
        .resolve_for_get(
            TenantId(child),
            OwnerId(owner),
            &sref("suppressed"),
            &[child, parent],
        )
        .await
        .expect("resolve");
    let winner = resolved.expect("the declared/none row itself is the winner (it blocks)");
    assert_eq!(winner.tenant_id, TenantId(child));
    assert_eq!(winner.status, SecretStatus::Declared);
    assert_eq!(winner.fallback, Fallback::None);
}

#[tokio::test]
async fn resolve_for_get_declared_inherit_row_never_competes() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    seed_active(&repo, parent, owner, "inherit-through", SharingMode::Shared).await;
    let (child_id, _) =
        seed_active(&repo, child, owner, "inherit-through", SharingMode::Tenant).await;
    let scope = AccessScope::for_tenant(child);
    repo.remove_value(
        &scope,
        child_id,
        Some(1),
        SharingMode::Tenant,
        Fallback::Inherit,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("declared/inherit");

    let resolved = repo
        .resolve_for_get(
            TenantId(child),
            OwnerId(owner),
            &sref("inherit-through"),
            &[child, parent],
        )
        .await
        .expect("resolve")
        .expect("the ancestor's shared row must still resolve");
    assert_eq!(resolved.tenant_id, TenantId(parent));
    assert_eq!(resolved.status, SecretStatus::Active);
}

#[tokio::test]
async fn resolve_candidates_includes_own_declared_row_and_ancestor_shared_row() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    seed_active(&repo, parent, owner, "cand", SharingMode::Shared).await;
    let (child_id, _) = seed_active(&repo, child, owner, "cand", SharingMode::Tenant).await;
    let scope = AccessScope::for_tenant(child);
    repo.remove_value(
        &scope,
        child_id,
        Some(1),
        SharingMode::Tenant,
        Fallback::Inherit,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("declared/inherit");

    let candidates = repo
        .resolve_candidates(
            TenantId(child),
            OwnerId(owner),
            &sref("cand"),
            &[child, parent],
        )
        .await
        .expect("resolve_candidates");

    assert_eq!(
        candidates.len(),
        2,
        "own declared row + ancestor shared row"
    );
    assert!(
        candidates
            .iter()
            .any(|r| r.tenant_id == TenantId(child) && r.status == SecretStatus::Declared),
        "own declared/inherit row must be visible for record-view reporting"
    );
    assert!(
        candidates
            .iter()
            .any(|r| r.tenant_id == TenantId(parent) && r.status == SecretStatus::Active),
        "ancestor's resolving shared row must be visible"
    );
}

#[tokio::test]
async fn resolve_candidates_excludes_ancestor_declared_inherit_row() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    let (parent_id, _) = seed_active(&repo, parent, owner, "hidden", SharingMode::Shared).await;
    let parent_scope = AccessScope::for_tenant(parent);
    repo.remove_value(
        &parent_scope,
        parent_id,
        Some(1),
        SharingMode::Shared,
        Fallback::Inherit,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("ancestor declared/inherit");

    let candidates = repo
        .resolve_candidates(
            TenantId(child),
            OwnerId(owner),
            &sref("hidden"),
            &[child, parent],
        )
        .await
        .expect("resolve_candidates");
    assert!(
        candidates.is_empty(),
        "an ancestor's declared/inherit row must not be a candidate at all"
    );
}

// ── collection read: list_visible_types / list_candidate_references /
//    list_candidates_for_references (ADR-0005, ADR-0010) ──────────────────

#[tokio::test]
async fn list_candidate_references_is_distinct_and_keyset_paginated() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();

    for name in ["a", "b", "c", "d"] {
        seed_active(&repo, tenant, owner, name, SharingMode::Tenant).await;
    }

    let first_page = repo
        .list_candidate_references(
            TenantId(tenant),
            OwnerId(owner),
            &[tenant],
            None,
            &AccessScope::allow_all(),
            None,
            false,
            3,
        )
        .await
        .expect("page 1");
    assert_eq!(
        first_page,
        vec!["a", "b", "c"],
        "limit+1-sized fetch, ascending"
    );

    let second_page = repo
        .list_candidate_references(
            TenantId(tenant),
            OwnerId(owner),
            &[tenant],
            None,
            &AccessScope::allow_all(),
            Some("c"),
            false,
            3,
        )
        .await
        .expect("page 2");
    assert_eq!(
        second_page,
        vec!["d"],
        "cursor is exclusive; only the reference after it is returned"
    );

    let descending = repo
        .list_candidate_references(
            TenantId(tenant),
            OwnerId(owner),
            &[tenant],
            None,
            &AccessScope::allow_all(),
            None,
            true,
            10,
        )
        .await
        .expect("descending");
    assert_eq!(descending, vec!["d", "c", "b", "a"]);
}

#[tokio::test]
async fn list_candidate_references_clamps_by_reference_and_type() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api_key_uuid = SecretType::from_name("api-key").expect("known").uuid();

    seed_active(&repo, tenant, owner, "generic-one", SharingMode::Tenant).await;
    seed_active(&repo, tenant, owner, "generic-two", SharingMode::Tenant).await;
    seed_active_typed(
        &repo,
        tenant,
        owner,
        "api-key-one",
        SharingMode::Tenant,
        api_key_uuid,
    )
    .await;

    let by_reference = repo
        .list_candidate_references(
            TenantId(tenant),
            OwnerId(owner),
            &[tenant],
            Some(&["generic-one".to_owned(), "api-key-one".to_owned()]),
            &AccessScope::allow_all(),
            None,
            false,
            10,
        )
        .await
        .expect("reference clamp");
    assert_eq!(by_reference, vec!["api-key-one", "generic-one"]);

    let by_type = repo
        .list_candidate_references(
            TenantId(tenant),
            OwnerId(owner),
            &[tenant],
            None,
            &AccessScope::single(toolkit_security::ScopeConstraint::new(vec![
                toolkit_security::ScopeFilter::in_uuids(
                    crate::domain::authz::SECRET_TYPE_PROP,
                    vec![api_key_uuid],
                ),
            ])),
            None,
            false,
            10,
        )
        .await
        .expect("type clamp");
    assert_eq!(by_type, vec!["api-key-one"]);
}

#[tokio::test]
async fn list_candidate_references_spans_the_ancestor_chain_shared_only() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();

    seed_active(&repo, parent, owner, "parent-shared", SharingMode::Shared).await;
    seed_active(&repo, parent, owner, "parent-tenant", SharingMode::Tenant).await;
    seed_active(&repo, child, owner, "child-own", SharingMode::Tenant).await;

    let refs = repo
        .list_candidate_references(
            TenantId(child),
            OwnerId(owner),
            &[child, parent],
            None,
            &AccessScope::allow_all(),
            None,
            false,
            10,
        )
        .await
        .expect("list");
    assert_eq!(
        refs,
        vec!["child-own", "parent-shared"],
        "the parent's tenant-only row must not be visible to the child"
    );
}

#[tokio::test]
async fn list_candidate_references_type_scope_matches_the_collection_reads_visibility_predicate() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let other_owner = Uuid::new_v4();

    let generic_uuid = SecretType::generic().uuid();
    let api_key_uuid = SecretType::from_name("api-key").expect("known").uuid();
    let personal_token_uuid = SecretType::from_name("personal-token")
        .expect("known")
        .uuid();
    let oauth2_uuid = SecretType::from_name("oauth2-client")
        .expect("known")
        .uuid();
    let basic_auth_uuid = SecretType::from_name("basic-auth").expect("known").uuid();
    let bearer_uuid = SecretType::from_name("bearer-token").expect("known").uuid();
    let cert_uuid = SecretType::from_name("certificate").expect("known").uuid();
    let ssh_key_uuid = SecretType::from_name("ssh-key").expect("known").uuid();

    // Own tenant, private-for-subject: visible.
    seed_active_typed(
        &repo,
        child,
        owner,
        "own-private",
        SharingMode::Private,
        generic_uuid,
    )
    .await;
    // A second own-tenant row of the SAME type: must not duplicate the type
    // in the result.
    seed_active_typed(
        &repo,
        child,
        owner,
        "own-private-dup",
        SharingMode::Private,
        generic_uuid,
    )
    .await;

    // Own tenant, tenant-sharing row: visible.
    seed_active_typed(
        &repo,
        child,
        owner,
        "own-tenant",
        SharingMode::Tenant,
        api_key_uuid,
    )
    .await;

    // Own tenant, declared row: visible (any status counts for the caller's
    // own tenant, exactly like `list_candidate_references`'s predicate).
    let (own_declared_id, _) = seed_active_typed(
        &repo,
        child,
        owner,
        "own-declared",
        SharingMode::Tenant,
        personal_token_uuid,
    )
    .await;
    repo.remove_value(
        &AccessScope::for_tenant(child),
        own_declared_id,
        Some(1),
        SharingMode::Tenant,
        Fallback::Inherit,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("own declared row");

    // Another subject's private row in the SAME (own) tenant: excluded.
    seed_active_typed(
        &repo,
        child,
        other_owner,
        "other-private",
        SharingMode::Private,
        oauth2_uuid,
    )
    .await;

    // Ancestor, shared and resolution-eligible: visible.
    seed_active_typed(
        &repo,
        parent,
        owner,
        "parent-shared",
        SharingMode::Shared,
        basic_auth_uuid,
    )
    .await;

    // Ancestor, tenant-sharing (never inherited): excluded.
    seed_active_typed(
        &repo,
        parent,
        owner,
        "parent-tenant",
        SharingMode::Tenant,
        bearer_uuid,
    )
    .await;

    // Ancestor, private (never inherited): excluded.
    seed_active_typed(
        &repo,
        parent,
        owner,
        "parent-private",
        SharingMode::Private,
        cert_uuid,
    )
    .await;

    // Ancestor, shared but declared/inherit (not resolution-eligible):
    // excluded.
    let (parent_declared_id, _) = seed_active_typed(
        &repo,
        parent,
        owner,
        "parent-declared",
        SharingMode::Shared,
        ssh_key_uuid,
    )
    .await;
    repo.remove_value(
        &AccessScope::for_tenant(parent),
        parent_declared_id,
        Some(1),
        SharingMode::Shared,
        Fallback::Inherit,
        None,
        true,
    )
    .await
    .expect("remove_value")
    .expect("ancestor declared/inherit row");

    let mut refs = repo
        .list_candidate_references(
            TenantId(child),
            OwnerId(owner),
            &[child, parent],
            None,
            &AccessScope::allow_all(),
            None,
            false,
            100,
        )
        .await
        .expect("visible references");
    refs.sort();
    let mut expected = vec![
        "own-declared",
        "own-private",
        "own-private-dup",
        "own-tenant",
        "parent-shared",
    ];
    expected.sort_unstable();
    assert_eq!(
        refs, expected,
        "own rows of any status, other subjects' private rows and ancestor \
         non-shared/non-eligible rows excluded, no duplicates"
    );

    // The type scope is applied in SQL through the secure ORM: a scope on
    // the PDP type property keeps only the rows of those types.
    let type_scope = AccessScope::single(toolkit_security::ScopeConstraint::new(vec![
        toolkit_security::ScopeFilter::in_uuids(
            crate::domain::authz::SECRET_TYPE_PROP,
            vec![api_key_uuid, basic_auth_uuid],
        ),
    ]));
    let clamped = repo
        .list_candidate_references(
            TenantId(child),
            OwnerId(owner),
            &[child, parent],
            None,
            &type_scope,
            None,
            false,
            100,
        )
        .await
        .expect("type-scoped references");
    assert_eq!(
        clamped,
        vec!["own-tenant", "parent-shared"],
        "the type predicate is compiled into the SQL clamp"
    );

    // A deny-all type scope matches nothing.
    let none = repo
        .list_candidate_references(
            TenantId(child),
            OwnerId(owner),
            &[child, parent],
            None,
            &AccessScope::deny_all(),
            None,
            false,
            100,
        )
        .await
        .expect("deny-all type scope");
    assert!(none.is_empty());
}

#[tokio::test]
async fn list_candidates_for_references_fetches_whole_rows_unclamped_by_type() {
    let repo = setup().await;
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let api_key_uuid = SecretType::from_name("api-key").expect("known").uuid();

    seed_active(&repo, parent, owner, "shadowed", SharingMode::Shared).await;
    seed_active_typed(
        &repo,
        child,
        owner,
        "shadowed",
        SharingMode::Tenant,
        api_key_uuid,
    )
    .await;

    let rows = repo
        .list_candidates_for_references(
            TenantId(child),
            OwnerId(owner),
            &[child, parent],
            &["shadowed".to_owned()],
        )
        .await
        .expect("whole rows");

    assert_eq!(
        rows.len(),
        2,
        "both the own and the ancestor row, regardless of type"
    );
    assert!(
        rows.iter()
            .any(|r| r.tenant_id == TenantId(child) && r.secret_type_uuid == api_key_uuid)
    );
    assert!(
        rows.iter().any(|r| r.tenant_id == TenantId(parent)
            && r.secret_type_uuid == SecretType::generic().uuid())
    );
}

// ── PDP type property compiled to SQL (ADR-0010) ─────────────────────────────

/// A PDP scope on the tenant AND the credential-type property (the shape a
/// type-constraining decision on the base credential type compiles to).
fn tenant_and_type_scope(tenant: Uuid, types: Vec<Uuid>) -> AccessScope {
    AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
        ScopeFilter::in_uuids(crate::domain::authz::SECRET_TYPE_PROP, types),
    ]))
}

#[tokio::test]
async fn type_property_scope_filters_row_lookups_in_sql() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let generic_uuid = SecretType::generic().uuid();
    let api_key_uuid = SecretType::from_name("api-key").expect("known").uuid();
    seed_active_typed(&repo, tenant, owner, "g", SharingMode::Tenant, generic_uuid).await;
    seed_active_typed(&repo, tenant, owner, "a", SharingMode::Tenant, api_key_uuid).await;

    let only_generic = tenant_and_type_scope(tenant, vec![generic_uuid]);
    let key_g = SecretRef::new("g").expect("ref");
    let key_a = SecretRef::new("a").expect("ref");
    let t = TenantId(tenant);
    let o = OwnerId(owner);

    assert!(
        repo.find_own(&only_generic, t, o, &key_g)
            .await
            .expect("find")
            .is_some(),
        "a row of an admitted type is found"
    );
    assert!(
        repo.find_own(&only_generic, t, o, &key_a)
            .await
            .expect("find")
            .is_none(),
        "a row of another type is invisible to the scoped lookup"
    );
    assert!(
        repo.find_for_write(&only_generic, t, o, &key_a, SharingMode::Tenant)
            .await
            .expect("find")
            .is_none()
    );
    let both = tenant_and_type_scope(tenant, vec![generic_uuid, api_key_uuid]);
    assert!(
        repo.find_own(&both, t, o, &key_a)
            .await
            .expect("find")
            .is_some()
    );
    // A scope naming no type for this tenant matches nothing.
    assert!(
        repo.find_own(&AccessScope::deny_all(), t, o, &key_g)
            .await
            .expect("find")
            .is_none()
    );
}

#[tokio::test]
async fn type_property_scope_also_clamps_updates_and_deletes() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let generic_uuid = SecretType::generic().uuid();
    let api_key_uuid = SecretType::from_name("api-key").expect("known").uuid();
    let (api_key_id, _) =
        seed_active_typed(&repo, tenant, owner, "a", SharingMode::Tenant, api_key_uuid).await;
    let only_generic = tenant_and_type_scope(tenant, vec![generic_uuid]);

    let updated = repo
        .update_metadata(
            &only_generic,
            api_key_id,
            None,
            SharingMode::Shared,
            Fallback::Inherit,
            None,
        )
        .await
        .expect("update");
    assert!(
        updated.is_none(),
        "the UPDATE is clamped by the type predicate"
    );

    let deleted = repo
        .delete_by_id(
            &only_generic,
            &StoreKey::new(TenantId(tenant), api_key_id),
            None,
        )
        .await;
    assert!(
        matches!(deleted, Err(DomainError::NotFound)),
        "the DELETE is clamped by the type predicate: {deleted:?}"
    );

    let admit_api_key = tenant_and_type_scope(tenant, vec![api_key_uuid]);
    repo.delete_by_id(
        &admit_api_key,
        &StoreKey::new(TenantId(tenant), api_key_id),
        None,
    )
    .await
    .expect("an admitted type is deleted");
}

#[tokio::test]
async fn reference_property_scope_filters_row_lookups_in_sql() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed_active(&repo, tenant, owner, "smtp-password", SharingMode::Tenant).await;
    seed_active(&repo, tenant, owner, "other", SharingMode::Tenant).await;
    let t = TenantId(tenant);
    let o = OwnerId(owner);
    let key_s = SecretRef::new("smtp-password").expect("ref");
    let key_o = SecretRef::new("other").expect("ref");

    let by_reference = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
        ScopeFilter::r#in(
            crate::domain::authz::REFERENCE_PROP,
            vec![toolkit_security::ScopeValue::String(
                "smtp-password".to_owned(),
            )],
        ),
    ]));
    assert!(
        repo.find_own(&by_reference, t, o, &key_s)
            .await
            .expect("find")
            .is_some()
    );
    assert!(
        repo.find_own(&by_reference, t, o, &key_o)
            .await
            .expect("find")
            .is_none()
    );
    assert!(
        repo.find_for_write(&by_reference, t, o, &key_o, SharingMode::Tenant)
            .await
            .expect("find")
            .is_none()
    );

    // OR of a type alternative and a reference alternative.
    let api_key_uuid = SecretType::from_name("api-key").expect("known").uuid();
    let tenant_f = || ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]);
    let mixed = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![
            tenant_f(),
            ScopeFilter::in_uuids(crate::domain::authz::SECRET_TYPE_PROP, vec![api_key_uuid]),
        ]),
        ScopeConstraint::new(vec![
            tenant_f(),
            ScopeFilter::r#in(
                crate::domain::authz::REFERENCE_PROP,
                vec![toolkit_security::ScopeValue::String("other".to_owned())],
            ),
        ]),
    ]);
    assert!(
        repo.find_own(&mixed, t, o, &key_o)
            .await
            .expect("find")
            .is_some()
    );
    assert!(
        repo.find_own(&mixed, t, o, &key_s)
            .await
            .expect("find")
            .is_none()
    );
}

#[tokio::test]
async fn uuid_shaped_reference_in_a_scope_finds_the_row() {
    let repo = setup().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let name = Uuid::new_v4().to_string();
    seed_active(&repo, tenant, owner, &name, SharingMode::Tenant).await;
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
        ScopeFilter::r#in(
            crate::domain::authz::REFERENCE_PROP,
            vec![toolkit_security::ScopeValue::String(name.clone())],
        ),
    ]));
    let key = SecretRef::new(name).expect("ref");
    assert!(
        repo.find_own(&scope, TenantId(tenant), OwnerId(owner), &key)
            .await
            .expect("find")
            .is_some()
    );
}

#[tokio::test]
async fn tenants_with_other_type_ignore_private_rows() {
    let repo = setup().await;
    let type_a = SecretType::generic().uuid();
    let type_b = SecretType::from_name("personal-token")
        .expect("known")
        .uuid();
    let name = Uuid::new_v4().to_string();
    let creator = Uuid::new_v4();
    let private_only = Uuid::new_v4();
    let non_private = Uuid::new_v4();
    let owner = Uuid::new_v4();
    seed_active_typed(
        &repo,
        private_only,
        owner,
        &name,
        SharingMode::Private,
        type_b,
    )
    .await;
    seed_active_typed(
        &repo,
        non_private,
        owner,
        &name,
        SharingMode::Shared,
        type_b,
    )
    .await;

    let got = repo
        .list_tenants_with_other_type(&sref(&name), type_a, TenantId(creator), None, 100)
        .await
        .expect("query");
    assert_eq!(got, vec![non_private], "a private-only tenant is ignored");
}

#[tokio::test]
async fn resolve_non_private_ignores_private_rows() {
    let repo = setup().await;
    let type_b = SecretType::from_name("personal-token")
        .expect("known")
        .uuid();
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let chain = [child, parent];

    // The caller's own private row (another type) is invisible to it...
    seed_active_typed(&repo, child, owner, "k", SharingMode::Private, type_b).await;
    assert!(
        repo.resolve_non_private(TenantId(child), &sref("k"), &chain)
            .await
            .expect("resolve")
            .is_none()
    );
    // ...and an ancestor's private row never resolves.
    seed_active_typed(&repo, parent, owner, "k", SharingMode::Private, type_b).await;
    assert!(
        repo.resolve_non_private(TenantId(child), &sref("k"), &chain)
            .await
            .expect("resolve")
            .is_none()
    );
    // The nearest ancestor's shared row is what it resolves to.
    seed_active_typed(
        &repo,
        parent,
        Uuid::new_v4(),
        "k",
        SharingMode::Shared,
        SecretType::generic().uuid(),
    )
    .await;
    let row = repo
        .resolve_non_private(TenantId(child), &sref("k"), &chain)
        .await
        .expect("resolve")
        .expect("shared row");
    assert_eq!(row.tenant_id, TenantId(parent));
    assert_eq!(row.sharing, SharingMode::Shared);
    assert_eq!(row.secret_type_uuid, SecretType::generic().uuid());
}

#[tokio::test]
async fn tenants_with_other_type_exclude_creator_and_same_type_and_page_by_tenant() {
    let repo = setup().await;
    let type_a = SecretType::generic().uuid();
    let type_b = SecretType::from_name("personal-token")
        .expect("known")
        .uuid();
    let name = Uuid::new_v4().to_string();
    let other = Uuid::new_v4().to_string();
    let creator = Uuid::new_v4();
    let mut holders = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    holders.sort();
    let same_type_tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();

    // Holders with another type: one tenant holds two rows (a tenant-shared
    // and another owner's private one) and must be returned once.
    for t in holders {
        seed_active_typed(&repo, t, owner, &name, SharingMode::Tenant, type_b).await;
    }
    seed_active_typed(
        &repo,
        holders[1],
        Uuid::new_v4(),
        &name,
        SharingMode::Private,
        type_b,
    )
    .await;
    // Excluded: the creator's own tenant, a same-type holder, another reference.
    seed_active_typed(&repo, creator, owner, &name, SharingMode::Tenant, type_b).await;
    seed_active_typed(
        &repo,
        same_type_tenant,
        owner,
        &name,
        SharingMode::Tenant,
        type_a,
    )
    .await;
    seed_active_typed(
        &repo,
        Uuid::new_v4(),
        owner,
        &other,
        SharingMode::Tenant,
        type_b,
    )
    .await;

    let key = sref(&name);
    let all = repo
        .list_tenants_with_other_type(&key, type_a, TenantId(creator), None, 100)
        .await
        .expect("query");
    assert_eq!(all, holders.to_vec(), "distinct, ordered, filtered");

    let mut walked = Vec::new();
    let mut after = None;
    loop {
        let page = repo
            .list_tenants_with_other_type(&key, type_a, TenantId(creator), after, 2)
            .await
            .expect("page");
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 2);
        after = page.last().copied();
        walked.extend(page);
    }
    assert_eq!(
        walked,
        holders.to_vec(),
        "keyset paging visits each tenant once"
    );
}

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

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{
    DestroySelector, OwnerId, SecretRef, SecretType, SharingMode, StoreKey, TenantId, ValueVersion,
};
use sea_orm::{ActiveValue, EntityTrait};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::Wake;
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    CleanupTask, Fallback, IntentCommit, NewDeclaredSecret, NewSecret, SecretRow, SecretStatus,
    WriteAttempt,
};
use crate::domain::secret::repo::SecretRepo;
use crate::infra::outbox::CleanupEnqueuer;
use crate::infra::storage::entity;
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo_impl::SecretRepoImpl;

/// Records every cleanup task a repo transaction enqueues. The enqueue itself
/// does not write a row (the real outbox is exercised in
/// `infra::outbox_tests`).
#[derive(Default)]
struct RecordingEnqueuer {
    tasks: Mutex<Vec<CleanupTask>>,
}

#[async_trait]
impl CleanupEnqueuer for RecordingEnqueuer {
    async fn enqueue(
        &self,
        _runner: &(dyn DBRunner + Sync),
        task: &CleanupTask,
    ) -> Result<Wake, DomainError> {
        self.tasks.lock().expect("lock").push(task.clone());
        Ok(Wake::empty())
    }
}

/// Build a repo backed by a fresh, isolated in-memory SQLite database,
/// together with the enqueuer recording its purges.
async fn setup_with_purges() -> (SecretRepoImpl, Arc<RecordingEnqueuer>) {
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

    let purges = Arc::new(RecordingEnqueuer::default());
    let repo = SecretRepoImpl::new(
        Arc::new(DBProvider::<DomainError>::new(db)),
        Arc::clone(&purges) as Arc<dyn CleanupEnqueuer>,
    );
    (repo, purges)
}

/// Build a repo backed by a fresh, isolated in-memory SQLite database.
async fn setup() -> SecretRepoImpl {
    setup_with_purges().await.0
}

fn purged(purges: &RecordingEnqueuer) -> Vec<StoreKey> {
    purges
        .tasks
        .lock()
        .expect("lock")
        .iter()
        .filter_map(|t| match t {
            CleanupTask::Purge(key) => Some(key.clone()),
            CleanupTask::Destroy { .. } => None,
        })
        .collect()
}

/// Every task enqueued so far, purges and destroys, in order.
fn enqueued(purges: &RecordingEnqueuer) -> Vec<CleanupTask> {
    purges.tasks.lock().expect("lock").clone()
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

fn enqueued_of(purges: &RecordingEnqueuer) -> Vec<CleanupTask> {
    enqueued(purges)
}

/// An attempt that `put`s under the store key of record `id` in `tenant`,
/// for a plugin that supports `destroy`.
fn attempt_for(tenant: Uuid, id: Uuid) -> WriteAttempt {
    WriteAttempt {
        attempt_id: Uuid::new_v4(),
        key: StoreKey::new(TenantId(tenant), id),
        destroy_supported: true,
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
/// tests can probe the raw migration `CHECK` contracts directly (an
/// out-of-domain status, or a pointer/status pairing the domain would never
/// construct).
async fn insert_raw_secret(
    repo: &SecretRepoImpl,
    status: i16,
    value_version: Option<String>,
) -> Result<(), ScopeError> {
    let conn = repo.db.conn().expect("conn");
    entity::secrets::Entity::insert(entity::secrets::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        tenant_id: ActiveValue::Set(Uuid::new_v4()),
        reference: ActiveValue::Set("x".to_owned()),
        sharing: ActiveValue::Set(2),
        owner_id: ActiveValue::Set(Uuid::new_v4()),
        status: ActiveValue::Set(status),
        created_at: ActiveValue::NotSet,
        updated_at: ActiveValue::NotSet,
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(SecretType::generic().uuid()),
        expires_at: ActiveValue::Set(None),
        value_version: ActiveValue::Set(value_version),
        fallback: ActiveValue::Set(1),
    })
    .secure()
    .scope_unchecked(&AccessScope::allow_all())?
    .exec(&conn)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn migration_final_schema_rejects_retired_status_codes() {
    let repo = setup().await;
    for bad_status in [1_i16, 3_i16] {
        let err = insert_raw_secret(&repo, bad_status, None)
            .await
            .expect_err("retired status code must violate the narrowed CHECK");
        assert!(
            err.to_string().to_lowercase().contains("check"),
            "expected a CHECK violation, got: {err}"
        );
    }
}

#[tokio::test]
async fn migration_final_schema_enforces_value_version_with_status_pairing() {
    let repo = setup().await;
    // An active row with no value version violates
    // credstore_secrets_value_version_check.
    let err = insert_raw_secret(&repo, 2, None)
        .await
        .expect_err("active row without a value version must violate the pairing CHECK");
    assert!(err.to_string().to_lowercase().contains("check"));

    // A declared row carrying a value version equally violates it.
    let err = insert_raw_secret(&repo, 4, Some("1".to_owned()))
        .await
        .expect_err("declared row with a value version must violate the pairing CHECK");
    assert!(err.to_string().to_lowercase().contains("check"));

    // The two consistent shapes are accepted.
    insert_raw_secret(&repo, 2, Some("1".to_owned()))
        .await
        .expect("active with a value version");
    insert_raw_secret(&repo, 4, None)
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
    let (repo, purges) = setup_with_purges().await;
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
    let IntentCommit::Lost { enqueued } = outcome else {
        panic!("duplicate non-private insert violates the unique index: {outcome:?}");
    };
    assert_eq!(enqueued, vec![CleanupTask::Purge(key.clone())]);
    // The intent deletion committed in the same transaction as the purge.
    assert_eq!(intent_ids(&repo).await, Vec::<Uuid>::new());
    assert_eq!(enqueued_of(&purges), vec![CleanupTask::Purge(key)]);
}

#[tokio::test]
async fn insert_over_an_expired_row_conflicts_and_changes_nothing() {
    use time::Duration as TimeDuration;
    let (repo, purges) = setup_with_purges().await;
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
        purged(&purges),
        vec![StoreKey::new(TenantId(tenant), new.id)],
        "only the loser's own fresh key is purged"
    );
}

// ── write-intent journal: begin / drop / settle / reclaim ───────────────────

/// Inserts an intent row directly, with an explicit `lease_until`.
async fn insert_raw_intent(
    repo: &SecretRepoImpl,
    attempt_id: Uuid,
    key: &StoreKey,
    lease_until: time::OffsetDateTime,
) -> Result<(), ScopeError> {
    let conn = repo.db.conn().expect("conn");
    entity::write_intents::Entity::insert(entity::write_intents::ActiveModel {
        attempt_id: ActiveValue::Set(attempt_id),
        tenant_id: ActiveValue::Set(key.tenant_id.0),
        record_id: ActiveValue::Set(key.record_id),
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
    insert_raw_intent(&repo, attempt, &key, hours_ago(-1))
        .await
        .expect("the table exists");

    let err = insert_raw_intent(&repo, attempt, &some_key(Uuid::new_v4()), hours_ago(-1))
        .await
        .expect_err("attempt_id is the primary key");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected a uniqueness violation, got: {err}"
    );
    assert_eq!(intent_ids(&repo).await, vec![attempt]);
}

#[tokio::test]
async fn begin_write_intent_stores_the_key_and_a_database_clock_lease() {
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

#[tokio::test]
async fn drop_write_intent_deletes_the_intent_and_is_idempotent() {
    let repo = setup().await;
    let attempt = begin(&repo, Uuid::new_v4(), Uuid::new_v4()).await;
    assert_eq!(intent_ids(&repo).await, vec![attempt.attempt_id]);

    repo.drop_write_intent(attempt.attempt_id)
        .await
        .expect("drop");
    assert!(intent_ids(&repo).await.is_empty());
    repo.drop_write_intent(attempt.attempt_id)
        .await
        .expect("dropping an absent intent is not an error");
}

#[tokio::test]
async fn reclaim_skips_unexpired_intents_and_takes_expired_ones() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    // Live: lease 300s on the database clock.
    let live = begin(&repo, tenant, Uuid::new_v4()).await;
    // Expired: a writer that crashed an hour ago.
    let dead_key = some_key(tenant);
    let dead = Uuid::new_v4();
    insert_raw_intent(&repo, dead, &dead_key, hours_ago(1))
        .await
        .expect("seed");

    let reclaimed = repo.reclaim_expired(16).await.expect("reclaim");
    assert_eq!(reclaimed.intents, 1);
    assert_eq!(intent_ids(&repo).await, vec![live.attempt_id]);
    assert_eq!(
        reclaimed.enqueued,
        vec![CleanupTask::Purge(dead_key.clone())]
    );
    assert_eq!(enqueued_of(&purges), vec![CleanupTask::Purge(dead_key)]);

    // Nothing else is due.
    let again = repo.reclaim_expired(16).await.expect("reclaim");
    assert_eq!(again.intents, 0);
    assert!(again.enqueued.is_empty());
}

#[tokio::test]
async fn reclaim_purges_only_the_records_without_a_row() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    // A live record whose rotation crashed: its row exists.
    let (live_id, _) = seed_active(&repo, tenant, owner, "live", SharingMode::Tenant).await;
    let live_key = StoreKey::new(TenantId(tenant), live_id);
    // A create that crashed before its insert: no row.
    let dead_key = some_key(tenant);
    insert_raw_intent(&repo, Uuid::new_v4(), &live_key, hours_ago(2))
        .await
        .expect("seed");
    insert_raw_intent(&repo, Uuid::new_v4(), &dead_key, hours_ago(1))
        .await
        .expect("seed");

    let reclaimed = repo.reclaim_expired(16).await.expect("reclaim");

    assert_eq!(reclaimed.intents, 2, "both intents are deleted");
    assert!(intent_ids(&repo).await.is_empty());
    assert_eq!(
        enqueued_of(&purges),
        vec![CleanupTask::Purge(dead_key)],
        "the live record's orphan is not purged: it is above a live pointer"
    );
}

#[tokio::test]
async fn reclaim_takes_at_most_limit_intents_oldest_first() {
    let (repo, _purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let oldest = Uuid::new_v4();
    let middle = Uuid::new_v4();
    let newest = Uuid::new_v4();
    insert_raw_intent(&repo, newest, &some_key(tenant), hours_ago(1))
        .await
        .expect("seed");
    insert_raw_intent(&repo, oldest, &some_key(tenant), hours_ago(3))
        .await
        .expect("seed");
    insert_raw_intent(&repo, middle, &some_key(tenant), hours_ago(2))
        .await
        .expect("seed");

    let first = repo.reclaim_expired(2).await.expect("reclaim");
    assert_eq!(first.intents, 2);
    assert_eq!(intent_ids(&repo).await, vec![newest], "oldest leases first");

    let second = repo.reclaim_expired(2).await.expect("reclaim");
    assert_eq!(
        second.intents, 1,
        "fewer than the limit: the backlog is done"
    );
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn settle_lost_intent_destroys_exactly_when_the_row_exists_else_purges() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, owner, "live", SharingMode::Tenant).await;
    let live_key = StoreKey::new(TenantId(tenant), id);

    let tasks = repo
        .settle_lost_intent(&live_key, &vv("2"), true)
        .await
        .expect("settle");
    assert_eq!(
        tasks,
        vec![CleanupTask::Destroy {
            key: live_key.clone(),
            selector: DestroySelector::Exactly(vv("2")),
        }]
    );

    // The row exists but the plugin has no destroy: nothing to enqueue.
    let tasks = repo
        .settle_lost_intent(&live_key, &vv("2"), false)
        .await
        .expect("settle");
    assert!(tasks.is_empty());

    // No row: the whole key is dead, whatever the plugin supports.
    let dead_key = some_key(tenant);
    for destroy_supported in [true, false] {
        let tasks = repo
            .settle_lost_intent(&dead_key, &vv("1"), destroy_supported)
            .await
            .expect("settle");
        assert_eq!(tasks, vec![CleanupTask::Purge(dead_key.clone())]);
    }
    assert_eq!(enqueued_of(&purges).len(), 3, "exactly the tasks returned");
}

#[tokio::test]
async fn commit_with_its_intent_deleted_by_a_concurrent_reclaim_is_intent_lost() {
    // The sequence of ADR-0006 step 5c, end to end on SQL: the writer
    // announces, a reclaim (lease over) deletes the intent, the writer's
    // commit finds it gone.
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, old_value) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let attempt = attempt_for(tenant, id);
    // Expired lease for this very attempt.
    insert_raw_intent(&repo, attempt.attempt_id, &attempt.key, hours_ago(1))
        .await
        .expect("seed");
    let reclaimed = repo.reclaim_expired(16).await.expect("reclaim");
    assert_eq!(reclaimed.intents, 1);
    assert!(
        reclaimed.enqueued.is_empty(),
        "the record has a row: nothing to purge"
    );

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
    assert_eq!(row.value_version, Some(old_value));
    assert!(enqueued_of(&purges).is_empty());
}

// ── generated SQL of the PostgreSQL-specific statements ──────────────────────
//
// The SQLite tests above run the same builders, but the PostgreSQL text
// (database-clock lease, `FOR UPDATE SKIP LOCKED`, a target-less `ON
// CONFLICT DO NOTHING`) is pinned here without a server.

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
            r#"INSERT INTO "credstore_write_intents" ("attempt_id", "tenant_id", "record_id", "lease_until") SELECT "#
        ),
        "{sql}"
    );
    assert!(
        sql.contains("now() + make_interval(0, 0, 0, 0, 0, 0, 300"),
        "lease_until must be the database clock plus the lease: {sql}"
    );

    // The prepared form binds the lease as the fourth parameter: the
    // placeholder syntax is the backend's own (`$n`).
    let (prepared, values) = insert.build(PostgresQueryBuilder);
    assert!(
        prepared.ends_with("now() + make_interval(0, 0, 0, 0, 0, 0, $4)"),
        "{prepared}"
    );
    assert_eq!(values.0.len(), 4);
}

#[test]
fn postgres_reclaim_batch_locks_with_skip_locked_oldest_first() {
    use sea_orm::QueryTrait;

    let sql = super::intents::expired_intents_select(sea_orm::DbBackend::Postgres, 16)
        .build(sea_orm::DbBackend::Postgres)
        .to_string();
    assert!(sql.contains(r#""lease_until" < (now())"#), "{sql}");
    assert!(
        sql.contains(
            r#"ORDER BY "credstore_write_intents"."lease_until" ASC LIMIT 16 FOR UPDATE SKIP LOCKED"#
        ),
        "{sql}"
    );
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

// ── write protocol: insert_declared (ADR-0004 Amendment B) ──────────────────

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
    let (repo, purges) = setup_with_purges().await;
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
        purged(&purges).is_empty(),
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
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, old_value) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let outcome = switch_with_intent(&repo, tenant, id, 99 /* stale */, "2").await;
    let IntentCommit::Lost { enqueued } = outcome else {
        panic!("a stale CAS is a definite loss: {outcome:?}");
    };
    // The row still exists, so only the loser's own version is dead.
    let key = StoreKey::new(TenantId(tenant), id);
    let expected = vec![CleanupTask::Destroy {
        key,
        selector: DestroySelector::Exactly(vv("2")),
    }];
    assert_eq!(enqueued, expected);
    assert_eq!(enqueued_of(&purges), expected);
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
async fn switch_value_loss_without_destroy_support_enqueues_nothing_for_a_live_row() {
    let (repo, purges) = setup_with_purges().await;
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
    let IntentCommit::Lost { enqueued } = outcome else {
        panic!("a stale CAS is a definite loss: {outcome:?}");
    };
    assert!(enqueued.is_empty());
    assert!(enqueued_of(&purges).is_empty());
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn switch_value_missing_row_is_a_definite_loss_that_purges_the_key() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();

    let outcome = switch_with_intent(&repo, tenant, id, 1, "2").await;
    let IntentCommit::Lost { enqueued } = outcome else {
        panic!("a missing row loses the CAS: {outcome:?}");
    };
    let expected = vec![CleanupTask::Purge(StoreKey::new(TenantId(tenant), id))];
    assert_eq!(enqueued, expected);
    assert_eq!(enqueued_of(&purges), expected);
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn switch_value_commit_enqueues_destroy_below_the_new_version_with_the_intent_deletion() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, _) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    let outcome = switch_with_intent(&repo, tenant, id, 1, "2").await;
    let IntentCommit::Committed { enqueued, .. } = outcome else {
        panic!("{outcome:?}");
    };
    let expected = vec![CleanupTask::Destroy {
        key: StoreKey::new(TenantId(tenant), id),
        selector: DestroySelector::Below(vv("2")),
    }];
    assert_eq!(enqueued, expected);
    assert_eq!(enqueued_of(&purges), expected);
    assert!(intent_ids(&repo).await.is_empty());
}

#[tokio::test]
async fn switch_value_with_a_reclaimed_intent_changes_nothing() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let (id, old_value) = seed_active(&repo, tenant, owner, "k", SharingMode::Tenant).await;

    // The attempt's intent is gone before its commit (reclaimed).
    let attempt = begin(&repo, tenant, id).await;
    repo.drop_write_intent(attempt.attempt_id)
        .await
        .expect("drop");
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
    assert!(enqueued_of(&purges).is_empty(), "nothing enqueued");
}

#[tokio::test]
async fn insert_active_with_a_reclaimed_intent_changes_nothing() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let new = new_secret(tenant, owner, "k", SharingMode::Tenant, vv("1"));

    let attempt = begin(&repo, tenant, new.id).await;
    repo.drop_write_intent(attempt.attempt_id)
        .await
        .expect("drop");
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
    assert!(enqueued_of(&purges).is_empty());
}

// ── write protocol: delete_by_id ─────────────────────────────────────────────

#[tokio::test]
async fn delete_by_id_enqueues_the_key_purge_and_then_not_found() {
    let (repo, purges) = setup_with_purges().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (id, _) = seed_active(&repo, tenant, owner, "gone", SharingMode::Tenant).await;
    let key = StoreKey::new(TenantId(tenant), id);

    repo.delete_by_id(&scope, &key, None).await.expect("delete");
    assert_eq!(purged(&purges), vec![key.clone()]);

    let err = repo
        .delete_by_id(&scope, &key, None)
        .await
        .expect_err("second delete is NotFound");
    assert!(matches!(err, DomainError::NotFound));
    assert_eq!(
        purged(&purges).len(),
        1,
        "no second purge for a missing row"
    );
}

#[tokio::test]
async fn delete_by_id_with_stale_expected_version_is_not_found_and_enqueues_nothing() {
    let (repo, purges) = setup_with_purges().await;
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
    assert!(purged(&purges).is_empty());

    repo.delete_by_id(&scope, &key, Some(1))
        .await
        .expect("matching expected_version deletes");
    assert_eq!(purged(&purges), vec![key]);
}

#[tokio::test]
async fn delete_then_create_only_put_under_the_same_reference_succeeds() {
    // No name retention (ADR-0006): a successor mints its own record id and
    // store key, so it can never collide with the predecessor's lagging purge.
    let (repo, purges) = setup_with_purges().await;
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
        purged(&purges),
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

// ── ADR-0004: update_metadata, remove_value, widened find_own/find_for_write,
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
async fn remove_value_declares_row_and_enqueues_destroy_below_and_exactly_the_old_version() {
    let (repo, purges) = setup_with_purges().await;
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
    assert_eq!(tasks, expected);
    assert_eq!(
        enqueued_of(&purges),
        expected,
        "enqueued by the same transaction as the CAS"
    );
}

#[tokio::test]
async fn remove_value_without_destroy_support_enqueues_nothing() {
    let (repo, purges) = setup_with_purges().await;
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
    assert!(enqueued_of(&purges).is_empty());
}

#[tokio::test]
async fn remove_value_on_already_declared_row_enqueues_nothing() {
    let (repo, purges) = setup_with_purges().await;
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
        enqueued_of(&purges).len(),
        2,
        "only the first removal enqueued"
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

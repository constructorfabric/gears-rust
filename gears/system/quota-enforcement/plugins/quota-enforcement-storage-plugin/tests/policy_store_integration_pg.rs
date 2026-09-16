#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `PostgreSQL`-backed concurrency suite of the SQL policy store. Every policy
//! transition takes `FOR UPDATE` on the header row before it reads the pointer,
//! and that lock is the only thing standing between two concurrent operators
//! and a skipped version, a reused version, or a pointer that disagrees with
//! the version states. `SQLite` serializes writers, so it cannot show any of
//! it. Requires Docker. Run with
//! `cargo test -p cf-gears-quota-enforcement-storage-plugin --features postgres --test policy_store_integration_pg`.

use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::testing::test_metric;
use quota_enforcement_sdk::{
    PageRequest, PolicyDraft, PolicyId, PolicySchemaSnapshot, PolicyScope, PolicyUpdate,
    PolicyVersionState, StorageError,
};
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::SecurityContext;

use quota_enforcement_storage_plugin::infra::storage::Migrator;
use quota_enforcement_storage_plugin::{
    NotificationEnqueuer, QeOutbox, SqlPolicyStore, start_outbox,
};

struct PgHarness {
    store: SqlPolicyStore,
    outbox: OutboxHandle,
    _container: ContainerAsync<Postgres>,
}

fn context() -> SecurityContext {
    SecurityContext::anonymous()
}

fn draft() -> PolicyDraft {
    PolicyDraft {
        scope: PolicyScope::Metric {
            metric: test_metric(),
        },
        engine_id: "most-restrictive-wins".into(),
        engine_config: serde_json::json!({}),
        timeout_ms: None,
        description: None,
        comment: Some("creation".into()),
        created_by: "ignored".into(),
        schema_snapshot: PolicySchemaSnapshot::default(),
    }
}

fn patch(if_match_version: u32, comment: &str) -> PolicyUpdate {
    PolicyUpdate {
        if_match_version,
        engine_id: None,
        engine_config: None,
        timeout_ms: None,
        comment: Some(comment.to_owned()),
        created_by: "ignored".into(),
        schema_snapshot: None,
    }
}

async fn wait_for_tcp(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "postgres never listened"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

impl PgHarness {
    async fn up() -> Self {
        let container = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .start()
            .await
            .expect("start postgres");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("mapped port");
        wait_for_tcp(port).await;
        let dsn = format!("postgres://user:pass@127.0.0.1:{port}/app");
        let mut db: Option<Db> = None;
        for _ in 0..20 {
            match connect_db(&dsn, ConnectOpts::default()).await {
                Ok(connected) => {
                    db = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        let db = db.expect("connect to postgres");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        let outbox = start_outbox(db.clone()).await.expect("outbox");
        let enqueuer = Arc::new(QeOutbox::new());
        enqueuer
            .bind(Arc::clone(outbox.outbox()))
            .expect("bind once");
        let enqueuer: Arc<dyn NotificationEnqueuer> = enqueuer;
        Self {
            store: SqlPolicyStore::new(db, enqueuer),
            outbox,
            _container: container,
        }
    }

    async fn down(self) {
        self.outbox.stop().await;
    }
}

#[tokio::test]
async fn two_updates_from_the_same_version_leave_exactly_one_winner() {
    let harness = PgHarness::up().await;
    let ctx = context();
    let created = harness
        .store
        .create_policy(&ctx, draft(), &[])
        .await
        .expect("create");
    let id = created.policy_id.clone();

    // Both writers read version 1 and both claim it. The header lock forces
    // them through one at a time, so the loser sees the committed version 2
    // rather than allocating a second version 2 of its own.
    let (left, right) = tokio::join!(
        harness
            .store
            .update_policy(&ctx, id.clone(), patch(1, "left"), &[]),
        harness
            .store
            .update_policy(&ctx, id.clone(), patch(1, "right"), &[]),
    );
    let (winner, loser) = match (left, right) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
        (Ok(_), Ok(_)) => panic!("both updates claimed version 1"),
        (Err(_), Err(_)) => panic!("neither update made progress"),
    };
    assert_eq!(winner.version, 2);
    assert!(matches!(
        loser,
        StorageError::VersionConflict {
            expected: 1,
            actual: 2
        }
    ));

    // Exactly two versions exist, and the pointer agrees with the states.
    let history = harness
        .store
        .list_policy_versions(&id, PageRequest::first(10))
        .await
        .expect("history");
    assert_eq!(
        history.items.iter().map(|m| m.version).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        history.items.iter().map(|m| m.state).collect::<Vec<_>>(),
        vec![PolicyVersionState::Superseded, PolicyVersionState::Active]
    );
    assert_eq!(
        harness
            .store
            .read_active_policy_by_id(&id)
            .await
            .expect("active")
            .map(|v| v.version),
        Some(2)
    );
    harness.down().await;
}

#[tokio::test]
async fn a_concurrent_update_and_delete_commit_in_one_order_or_the_other() {
    let harness = PgHarness::up().await;
    let ctx = context();
    let created = harness
        .store
        .create_policy(&ctx, draft(), &[])
        .await
        .expect("create");
    let id = created.policy_id.clone();

    let (updated, deleted) = tokio::join!(
        harness
            .store
            .update_policy(&ctx, id.clone(), patch(1, "update"), &[]),
        harness.store.delete_policy(&ctx, id.clone(), None, &[]),
    );

    // Whichever order the lock grants, the reader never sees a mixed state: the
    // policy is either active at a single version or has no active version.
    let active = harness
        .store
        .read_active_policy_by_id(&id)
        .await
        .expect("read");
    match (updated, deleted) {
        // Update first: the delete then retires version 2.
        (Ok(version), Ok(outcome)) => {
            assert_eq!(version.version, 2);
            assert!(outcome.is_applied());
            assert!(active.is_none(), "the delete cleared the pointer");
        }
        // Delete first: the update finds no active version to supersede.
        (Err(error), Ok(outcome)) => {
            assert!(matches!(error, StorageError::PolicyDeleted { .. }));
            assert!(outcome.is_applied());
            assert!(active.is_none());
        }
        (result, delete) => panic!("unexpected interleaving: {result:?} / {delete:?}"),
    }

    // Version 1 is retained either way; a delete never erases history, and
    // whichever transition reached it left it in a terminal state.
    let first = harness
        .store
        .read_policy_version(&id, 1)
        .await
        .expect("read")
        .expect("version 1 is retained");
    assert!(
        matches!(
            first.state,
            PolicyVersionState::Superseded | PolicyVersionState::Deleted
        ),
        "version 1 ended in {:?}",
        first.state
    );
    assert_eq!(first.comment.as_deref(), Some("creation"));
    harness.down().await;
}

#[tokio::test]
async fn concurrent_creates_at_one_scope_leave_a_single_live_policy() {
    let harness = PgHarness::up().await;
    let ctx = context();

    // Creation does not lock a header, because there is none yet. The partial
    // unique index on the live scope is what arbitrates, so this proves the
    // constraint and not the lock.
    let (left, right) = tokio::join!(
        harness.store.create_policy(&ctx, draft(), &[]),
        harness.store.create_policy(&ctx, draft(), &[]),
    );
    let winner = match (left, right) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => {
            assert!(matches!(loser, StorageError::PolicyScopeOccupied { .. }));
            winner
        }
        (Ok(_), Ok(_)) => panic!("two live policies occupy one scope"),
        (Err(left), Err(right)) => panic!("neither create succeeded: {left:?} / {right:?}"),
    };
    assert_eq!(winner.version, 1);
    assert_eq!(
        harness
            .store
            .read_policy(&PolicyScope::Metric {
                metric: test_metric()
            })
            .await
            .expect("read")
            .map(|v| v.policy_id),
        Some(winner.policy_id)
    );
    assert_eq!(
        harness
            .store
            .read_active_policies()
            .await
            .expect("active")
            .len(),
        1
    );
    harness.down().await;
}

#[tokio::test]
async fn a_rollback_racing_an_update_never_reuses_a_version_number() {
    let harness = PgHarness::up().await;
    let ctx = context();
    let created = harness
        .store
        .create_policy(&ctx, draft(), &[])
        .await
        .expect("create");
    let id = created.policy_id.clone();
    harness
        .store
        .update_policy(&ctx, id.clone(), patch(1, "v2"), &[])
        .await
        .expect("v2");
    harness
        .store
        .update_policy(&ctx, id.clone(), patch(2, "v3"), &[])
        .await
        .expect("v3");

    // The high-water mark is carried on the header, not derived from the
    // pointer, so a rollback that moves the pointer back to 1 must not let a
    // later update mint a second version 2.
    let (rolled, updated) = tokio::join!(
        harness
            .store
            .rollback_policy(&ctx, id.clone(), 1, Some("back".into()), &[]),
        harness
            .store
            .update_policy(&ctx, id.clone(), patch(3, "v4"), &[]),
    );
    assert!(rolled.expect("rollback").is_applied());
    if let Ok(version) = updated {
        assert_eq!(version.version, 4, "version numbers never repeat");
    }

    let next = harness
        .store
        .update_policy(
            &ctx,
            id.clone(),
            patch(
                harness
                    .store
                    .read_active_policy_by_id(&id)
                    .await
                    .expect("active")
                    .expect("present")
                    .version,
                "next",
            ),
            &[],
        )
        .await
        .expect("next version");
    assert!(
        next.version >= 4,
        "the high-water mark survives a pointer move, got {}",
        next.version
    );

    let history = harness
        .store
        .list_policy_versions(&id, PageRequest::first(20))
        .await
        .expect("history");
    let mut versions = history.items.iter().map(|m| m.version).collect::<Vec<_>>();
    let before = versions.len();
    versions.dedup();
    assert_eq!(before, versions.len(), "a version number was reused");
    assert_eq!(
        history
            .items
            .iter()
            .filter(|m| m.state == PolicyVersionState::Active)
            .count(),
        1,
        "exactly one version is active"
    );
    harness.down().await;
}

#[tokio::test]
async fn an_unknown_policy_is_absence_on_reads_and_an_error_on_transitions() {
    let harness = PgHarness::up().await;
    let ctx = context();
    let missing = PolicyId::new("never-created");
    assert!(
        harness
            .store
            .read_active_policy_by_id(&missing)
            .await
            .expect("read")
            .is_none()
    );
    assert!(matches!(
        harness
            .store
            .list_policy_versions(&missing, PageRequest::first(10))
            .await,
        Err(StorageError::PolicyNotFound { .. })
    ));
    assert!(matches!(
        harness
            .store
            .delete_policy(&ctx, missing.clone(), None, &[])
            .await,
        Err(StorageError::PolicyNotFound { .. })
    ));
    assert!(matches!(
        harness
            .store
            .delete_policy(&ctx, PolicyId::global(), None, &[])
            .await,
        Err(StorageError::CannotDeleteSeededGlobalPolicy)
    ));
    harness.down().await;
}

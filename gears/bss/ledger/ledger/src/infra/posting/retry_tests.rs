//! Real SQLite transaction rollback and retry-budget regression coverage.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::infra::posting::error_transport::{business, repo_to_db};
use crate::infra::storage::{entity::currency_scale_registry as registry, migrations::Migrator};
use sea_orm::{ActiveValue::Set, EntityTrait};
use sea_orm_migration::MigratorTrait;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use toolkit_db::secure::{AccessScope, SecureEntityExt, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

async fn database() -> DBProvider<DbError> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap();
    DBProvider::new(db)
}

async fn insert(tx: &DbTx<'_>, tenant: Uuid) -> Result<(), AttemptError> {
    let row = registry::ActiveModel {
        tenant_id: Set(tenant),
        currency: Set("EUR".to_owned()),
        currency_scale: Set(2),
        source: Set("test".to_owned()),
    };
    registry::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::for_tenant(tenant), &row)
        .map_err(|error| scope_to_repo(error, DbBackend::Sqlite))?
        .exec(tx)
        .await
        .map_err(|error| insert_to_repo(error, DbBackend::Sqlite))?;
    Ok(())
}

async fn present(db: &DBProvider<DbError>, tenant: Uuid) -> bool {
    registry::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .one(&db.conn().unwrap())
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn conflicts_rollback_all_prior_writes_then_commit_once() {
    let db = database().await;
    let tenant = Uuid::new_v4();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    retry_transaction(&db.db(), move |tx| {
        let attempt = count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            // Inserting the same primary key again proves the preceding attempt rolled back.
            insert(tx, tenant).await?;
            if attempt < 2 {
                return Err(business(DomainError::ConcurrentModification("sidecar".into())).into());
            }
            Ok(())
        })
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(present(&db, tenant).await);
}

#[tokio::test]
async fn exhausted_conflicts_leave_no_partial_effect() {
    let db = database().await;
    let tenant = Uuid::new_v4();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let result: Result<(), _> = retry_transaction_for(
        &db.db(),
        RetryOp::new("invoice post", Some(tenant)),
        move |tx| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                insert(tx, tenant).await?;
                Err(RepoError::Conflict("stale version".into()).into())
            })
        },
    )
    .await;
    // The exhausted budget names the operation that conflicted.
    match result {
        Err(DomainError::ConcurrentModification(detail)) => {
            assert!(
                detail.starts_with("invoice post conflicted after"),
                "{detail}"
            );
        }
        other => panic!("expected ConcurrentModification, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(!present(&db, tenant).await);
}

#[tokio::test]
async fn real_unique_insert_violation_is_a_conflict_and_rolls_back() {
    let db = database().await;
    let tenant = Uuid::new_v4();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let result = retry_transaction(&db.db(), move |tx| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            insert(tx, tenant).await?;
            insert(tx, tenant).await
        })
    })
    .await;
    assert!(matches!(
        result,
        Err(DomainError::ConcurrentModification(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(!present(&db, tenant).await);
}

#[tokio::test]
async fn business_and_corrupt_storage_diagnostics_never_retry() {
    let db = database().await;
    for corrupt in [false, true] {
        let tenant = Uuid::new_v4();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let result: Result<(), _> = retry_transaction(&db.db(), move |tx| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                insert(tx, tenant).await?;
                let text =
                    "database is locked; could not serialize access; SQLSTATE 40001".to_owned();
                Err(if corrupt {
                    repo_to_db(RepoError::InvalidStoredMoney(text))
                } else {
                    business(DomainError::InvalidRequest(text))
                }
                .into())
            })
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!present(&db, tenant).await);
        assert!(matches!(
            result,
            Err(DomainError::Internal(_) | DomainError::InvalidRequest(_))
        ));
    }
}

#[test]
fn arbitrary_custom_and_non_driver_errors_cannot_spoof_contention() {
    for error in [
        DbErr::Custom("database is locked".into()),
        DbErr::Query(RuntimeErr::Internal("database is locked".into())),
    ] {
        assert!(!is_driver_contention(DbBackend::Sqlite, &error));
        assert!(matches!(
            scope_to_repo(ScopeError::Db(error), DbBackend::Sqlite),
            RepoError::Db(_)
        ));
    }
}

#[test]
fn registry_repo_conflict_survives_resolver_adapter() {
    let error =
        crate::domain::money::ScaleError::from(RepoError::Conflict("registry changed".into()));
    assert!(matches!(AttemptError::from(error), AttemptError::Conflict));
}

#[tokio::test]
async fn actual_sqlite_write_contention_consumes_three_attempts() {
    let path = std::env::temp_dir().join(format!("ledger-retry-{}.sqlite", Uuid::new_v4()));
    let dsn = format!("sqlite://{}?mode=rwc&busy_timeout=1", path.display());
    let handle = connect_db(&dsn, ConnectOpts::default()).await.unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&handle, Migrator::migrations())
        .await
        .unwrap();
    let holder = DBProvider::<DbError>::new(handle);
    let contender =
        DBProvider::<DbError>::new(connect_db(&dsn, ConnectOpts::default()).await.unwrap());
    let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let held = tokio::spawn(async move {
        holder
            .db()
            .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
                Box::pin(async move {
                    insert(tx, Uuid::new_v4()).await?;
                    locked_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok::<_, AttemptError>(())
                })
            })
            .await
            .unwrap();
    });
    locked_rx.await.unwrap();
    let tenant = Uuid::new_v4();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let result = retry_transaction(&contender.db(), move |tx| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { insert(tx, tenant).await })
    })
    .await;
    release_tx.send(()).unwrap();
    held.await.unwrap();
    assert!(matches!(
        result,
        Err(DomainError::ConcurrentModification(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(!present(&contender, tenant).await);
    drop(contender);
    // Best effort: Windows keeps the file locked until the pool's async close completes.
    let _ = std::fs::remove_file(&path);
    // SQLite may retain companion files until its pool's async close completes.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

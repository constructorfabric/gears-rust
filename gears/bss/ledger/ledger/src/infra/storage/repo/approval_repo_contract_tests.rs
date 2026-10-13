//! The documented error contract of the dual-control writes and the fail-closed
//! revalidation of stored policy versions, on migrated SQLite.
#![allow(clippy::unwrap_used)]

use super::*;
use bss_ledger_sdk::{CurrencySpec, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, SecureUpdateExt, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

async fn setup() -> Db {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    db
}

fn usd(text: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("USD".into(), 2).unwrap(),
    )
    .unwrap()
}

fn pending(tenant: Uuid) -> NewPendingApproval {
    NewPendingApproval {
        approval_id: Uuid::now_v7(),
        tenant,
        kind: "REFUND".into(),
        business_key: "refund-1".into(),
        intent: serde_json::json!({}),
        amount: Some(usd("500")),
        threshold_snapshot: serde_json::json!({}),
        reason_code: "TEST".into(),
        prepared_by: Uuid::now_v7(),
        prepared_at: OffsetDateTime::UNIX_EPOCH,
        correlation_id: Uuid::now_v7(),
        expires_at: OffsetDateTime::UNIX_EPOCH + time::Duration::days(7),
    }
}

fn policy(tenant: Uuid, version: i64) -> NewPolicyVersion {
    NewPolicyVersion {
        tenant,
        version,
        effective_from: OffsetDateTime::UNIX_EPOCH,
        policy: DualControlPolicy::try_new(
            D2Thresholds::try_new(vec![usd("1000")]).unwrap(),
            5,
            604_800,
        )
        .unwrap(),
        created_at_utc: OffsetDateTime::UNIX_EPOCH,
    }
}

/// A transaction body error: the repository's, or the transaction's own.
#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Unwrap the repository error a failed write rolled back with.
fn repo_error(result: Result<(), TestError>) -> Result<(), RepoError> {
    match result {
        Ok(()) => Ok(()),
        Err(TestError::Repo(error)) => Err(error),
        Err(TestError::Db(error)) => panic!("transaction failure: {error}"),
    }
}

macro_rules! attempt {
    ($db:expr, |$tx:ident| $body:expr) => {
        repo_error(
            $db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |$tx| {
                Box::pin(async move {
                    $body.await?;
                    Ok::<_, TestError>(())
                })
            })
            .await,
        )
    };
}

/// Insert one pending row in its own transaction, keeping the insert's own error.
async fn insert_pending_once(
    db: &Db,
    scope: &AccessScope,
    row: NewPendingApproval,
) -> Result<(), InsertPendingError> {
    let scope = scope.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(
            async move { Ok::<_, DbError>(ApprovalRepo::insert_pending(tx, &scope, row).await) },
        )
    })
    .await
    .unwrap()
}

/// A lost DC13 race is named, never retryable contention: the winner's row is
/// committed, so a fresh attempt could only hit the same index again.
#[tokio::test]
async fn a_second_active_approval_for_the_key_is_named_not_retryable_contention() {
    let db = setup().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    insert_pending_once(&db, &scope, pending(tenant))
        .await
        .unwrap();
    let duplicate = insert_pending_once(&db, &scope, pending(tenant)).await;
    assert!(
        matches!(duplicate, Err(InsertPendingError::ActiveExists)),
        "{duplicate:?}"
    );
}

#[tokio::test]
async fn policy_version_collision_is_conflict_and_out_of_range_is_named() {
    let db = setup().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let s = scope.clone();
    attempt!(db, |tx| ApprovalRepo::insert_policy_row(
        tx,
        &s,
        policy(tenant, 1)
    ))
    .unwrap();
    let s = scope.clone();
    let collision = attempt!(db, |tx| ApprovalRepo::insert_policy_row(
        tx,
        &s,
        policy(tenant, 1)
    ));
    assert!(
        matches!(collision, Err(RepoError::Conflict(_))),
        "{collision:?}"
    );
    assert!(matches!(
        D2Thresholds::try_new(vec![usd("1")]),
        Err(crate::domain::approval::policy::PolicyConfigError::D2OutOfRange { .. })
    ));
    // An out-of-range TTL cannot reach the repository: the policy type refuses it.
    assert_eq!(
        DualControlPolicy::try_new(D2Thresholds::EMPTY, 5, 0),
        Err(crate::domain::approval::policy::PolicyConfigError::TtlNotPositive(0))
    );
}

#[tokio::test]
async fn a_stored_threshold_outside_the_d2_range_fails_closed_on_read() {
    let db = setup().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let s = scope.clone();
    attempt!(db, |tx| ApprovalRepo::insert_policy_row(
        tx,
        &s,
        policy(tenant, 1)
    ))
    .unwrap();
    let conn = db.conn().unwrap();
    assert_eq!(
        ApprovalRepo::read_policy_versions_in(&conn, &scope, tenant)
            .await
            .unwrap()[0]
            .policy
            .d2_thresholds()
            .clone()
            .into_vec(),
        vec![usd("1000")]
    );
    // SQLite's threshold CHECK only requires a positive amount: 1.00 USD is
    // stored, though it is below the D2 floor of 100.00 at scale 2.
    threshold::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .col_expr(threshold::Column::Amount, Expr::value("1"))
        .exec(&conn)
        .await
        .unwrap();
    assert!(matches!(
        ApprovalRepo::read_policy_versions_in(&conn, &scope, tenant).await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

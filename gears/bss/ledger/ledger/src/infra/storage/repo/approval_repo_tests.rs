//! Real migrated SQLite approval and policy storage tests.
use super::*;
use bss_ledger_sdk::{CurrencySpec, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};
#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Pending(#[from] InsertPendingError),
    #[error(transparent)]
    Db(#[from] DbError),
}
async fn setup() -> (ApprovalRepo, Db, Uuid) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    (
        ApprovalRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
    )
}
fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
fn pending(tenant: Uuid) -> NewPendingApproval {
    NewPendingApproval {
        approval_id: Uuid::now_v7(),
        tenant,
        kind: "REFUND".into(),
        business_key: "test".into(),
        intent: serde_json::json!({"test":"old"}),
        amount: Some(money("123.45", "EUR", 2)),
        threshold_snapshot: serde_json::json!({"test":"policy"}),
        reason_code: "TEST".into(),
        prepared_by: Uuid::now_v7(),
        prepared_at: OffsetDateTime::UNIX_EPOCH,
        correlation_id: Uuid::now_v7(),
        expires_at: OffsetDateTime::UNIX_EPOCH + time::Duration::days(7),
    }
}
fn policy(tenant: Uuid) -> NewPolicyVersion {
    NewPolicyVersion {
        tenant,
        version: 1,
        effective_from: OffsetDateTime::UNIX_EPOCH,
        policy: policy_with(
            D2Thresholds::try_new(vec![
                money("1000", "USD", 2),
                money("100000", "JPY", 0),
                money("0.001", "BTC", 8),
            ])
            .unwrap(),
        ),
        created_at_utc: OffsetDateTime::UNIX_EPOCH,
    }
}
fn policy_with(d2: D2Thresholds) -> DualControlPolicy {
    DualControlPolicy::try_new(d2, 5, 604_800).unwrap()
}
#[tokio::test]
async fn optional_money_reads_and_resubmit_revision_state_are_atomic() {
    let (repo, db, tenant) = setup().await;
    let row = pending(tenant);
    let id = row.approval_id;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
        Box::pin(async move {
            ApprovalRepo::insert_pending(tx, &AccessScope::for_tenant(tenant), row).await?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    assert_eq!(
        repo.read(&AccessScope::for_tenant(tenant), tenant, id)
            .await
            .unwrap()
            .unwrap()
            .amount,
        Some(money("123.45", "EUR", 2))
    );
    assert!(
        repo.read(&AccessScope::for_tenant(tenant), Uuid::now_v7(), id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.list(&AccessScope::for_tenant(tenant), tenant, None, None)
            .await
            .unwrap()
            .len(),
        1
    );
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
        Box::pin(async move {
            let scope = AccessScope::for_tenant(tenant);
            assert_eq!(
                ApprovalRepo::transition(
                    tx,
                    &scope,
                    tenant,
                    id,
                    "PENDING",
                    1,
                    "NEEDS_REWORK",
                    None,
                    None
                )
                .await?,
                0
            );
            assert_eq!(
                ApprovalRepo::transition(
                    tx,
                    &scope,
                    tenant,
                    id,
                    "PENDING",
                    0,
                    "NEEDS_REWORK",
                    None,
                    None
                )
                .await?,
                1
            );
            ApprovalRepo::resubmit(
                tx,
                &scope,
                tenant,
                id,
                serde_json::json!({"test":"new"}),
                serde_json::json!({"test":"resubmission"}),
                Some(money("1.234", "KWD", 3)),
                0,
            )
            .await?;
            let r = ApprovalRepo::read_in_txn(tx, &scope, tenant, id)
                .await?
                .unwrap();
            assert_eq!(r.revision, 1);
            assert_eq!(r.state, "PENDING");
            assert_eq!(r.amount, Some(money("1.234", "KWD", 3)));
            assert_eq!(r.intent["test"], "new");
            assert_eq!(
                ApprovalRepo::transition(
                    tx,
                    &scope,
                    tenant,
                    id,
                    "PENDING",
                    0,
                    "APPROVING",
                    None,
                    None
                )
                .await?,
                0
            );
            ApprovalRepo::transition(
                tx,
                &scope,
                tenant,
                id,
                "PENDING",
                1,
                "NEEDS_REWORK",
                None,
                None,
            )
            .await?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    let error = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                ApprovalRepo::append_comment(
                    tx,
                    &scope,
                    Uuid::now_v7(),
                    id,
                    tenant,
                    1,
                    Uuid::now_v7(),
                    "rollback".into(),
                    OffsetDateTime::UNIX_EPOCH,
                )
                .await?;
                ApprovalRepo::resubmit(
                    tx,
                    &scope,
                    tenant,
                    id,
                    serde_json::json!({}),
                    serde_json::json!({}),
                    None,
                    0,
                )
                .await?;
                Ok::<_, TestError>(())
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, TestError::Repo(RepoError::Conflict(_))));
    assert!(
        repo.read_thread(&AccessScope::for_tenant(tenant), tenant, id)
            .await
            .unwrap()
            .is_empty()
    );
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
        Box::pin(async move {
            ApprovalRepo::resubmit(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                id,
                serde_json::json!({}),
                serde_json::json!({}),
                None,
                1,
            )
            .await?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    let row = repo
        .read(&AccessScope::for_tenant(tenant), tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.amount.is_none());
    assert_eq!(row.revision, 2);
}
#[tokio::test]
async fn policy_children_sorted_and_roll_back_with_parent() {
    let (repo, db, tenant) = setup().await;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
        Box::pin(async move {
            ApprovalRepo::insert_policy_row(tx, &AccessScope::for_tenant(tenant), policy(tenant))
                .await?;
            let versions =
                ApprovalRepo::read_policy_versions_in(tx, &AccessScope::for_tenant(tenant), tenant)
                    .await?;
            assert_eq!(
                versions[0]
                    .policy
                    .d2_thresholds()
                    .iter()
                    .map(|v| v.currency().code())
                    .collect::<Vec<_>>(),
                vec!["BTC", "JPY", "USD"]
            );
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    assert!(
        repo.read_policy_versions(&AccessScope::for_tenant(tenant), Uuid::now_v7())
            .await
            .unwrap()
            .is_empty()
    );
    let result = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
            Box::pin(async move {
                let mut p = policy(tenant);
                p.version = 2;
                ApprovalRepo::insert_policy_row(tx, &AccessScope::for_tenant(tenant), p).await?;
                Err::<(), _>(TestError::Repo(RepoError::Conflict(
                    "after children".into(),
                )))
            })
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        repo.read_policy_versions(&AccessScope::for_tenant(tenant), tenant)
            .await
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn corrupt_text_is_internal_on_read_list_and_resubmit() {
    let (repo, db, tenant) = setup().await;
    let row = pending(tenant);
    let id = row.approval_id;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
        Box::pin(async move {
            let scope = AccessScope::for_tenant(tenant);
            ApprovalRepo::insert_pending(tx, &scope, row).await?;
            approval::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .col_expr(approval::Column::Amount, Expr::value("123.450"))
                .col_expr(approval::Column::State, Expr::value("NEEDS_REWORK"))
                .filter(Condition::all().add(approval::Column::ApprovalId.eq(id)))
                .exec(tx)
                .await
                .map_err(scope_error)?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    assert!(matches!(
        repo.read(&AccessScope::for_tenant(tenant), tenant, id)
            .await,
        Err(DomainError::Internal(_))
    ));
    assert!(matches!(
        repo.list(&AccessScope::for_tenant(tenant), tenant, None, None)
            .await,
        Err(DomainError::Internal(_))
    ));
    let result = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
            Box::pin(async move {
                ApprovalRepo::resubmit(
                    tx,
                    &AccessScope::for_tenant(tenant),
                    tenant,
                    id,
                    serde_json::json!({}),
                    serde_json::json!({}),
                    None,
                    0,
                )
                .await?;
                Ok::<_, TestError>(())
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(TestError::Repo(RepoError::InvalidStoredMoney(_)))
    ));
}

#[tokio::test]
async fn denied_scope_invalid_policy_and_empty_defaults() {
    let (repo, db, tenant) = setup().await;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
        Box::pin(async move {
            let scope = AccessScope::for_tenant(tenant);
            let mut p = policy(tenant);
            p.policy = policy_with(D2Thresholds::default());
            ApprovalRepo::insert_policy_row(tx, &scope, p).await?;
            ApprovalRepo::insert_pending(tx, &scope, pending(tenant)).await?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    let denied = AccessScope::for_tenant(Uuid::now_v7());
    assert!(
        repo.list(&denied, tenant, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.read_policy_versions(&denied, tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.read_policy_versions(&AccessScope::for_tenant(tenant), tenant)
            .await
            .unwrap()[0]
            .policy
            .d2_thresholds()
            .is_empty()
    );
    // A repeated currency cannot be represented, so it never reaches a write.
    assert_eq!(
        D2Thresholds::try_new(vec![money("1000", "USD", 2), money("1000", "USD", 2)]),
        Err(crate::domain::approval::policy::PolicyConfigError::DuplicateCurrency("USD".into()))
    );
    // An out-of-range A6 cannot reach the repository: the policy type refuses it.
    assert_eq!(
        DualControlPolicy::try_new(D2Thresholds::default(), 0, 604_800),
        Err(crate::domain::approval::policy::PolicyConfigError::A6OutOfRange(0))
    );
    let e = crate::infra::posting::error_transport::repo_to_db(
        RepoError::ApprovalPolicyOutOfRange("bad".into()),
    );
    assert!(matches!(
        crate::infra::posting::error_transport::decode_business_error(&e),
        DomainError::DualControlPolicyOutOfRange(_)
    ));
}

#[tokio::test]
async fn impossible_metadata_and_exhausted_revision_fail_closed() {
    let (repo, db, tenant) = setup().await;
    let row = pending(tenant);
    let id = row.approval_id;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
        Box::pin(async move {
            let scope = AccessScope::for_tenant(tenant);
            ApprovalRepo::insert_pending(tx, &scope, row).await?;
            let raw = approval::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(Condition::all().add(approval::Column::ApprovalId.eq(id)))
                .one(tx)
                .await
                .map_err(scope_error)?
                .unwrap();
            let mut partial = raw.clone();
            partial.currency_scale = None;
            assert!(matches!(
                ApprovalRow::try_from(partial),
                Err(RepoError::InvalidStoredMoney(_))
            ));
            let mut invalid = raw;
            invalid.currency_scale = Some(29);
            assert!(matches!(
                ApprovalRow::try_from(invalid),
                Err(RepoError::InvalidStoredMoney(_))
            ));
            approval::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .col_expr(approval::Column::Revision, Expr::value(i32::MAX))
                .col_expr(approval::Column::State, Expr::value("NEEDS_REWORK"))
                .filter(Condition::all().add(approval::Column::ApprovalId.eq(id)))
                .exec(tx)
                .await
                .map_err(scope_error)?;
            Ok::<_, TestError>(())
        })
    })
    .await
    .unwrap();
    let result = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), |tx| {
            Box::pin(async move {
                ApprovalRepo::resubmit(
                    tx,
                    &AccessScope::for_tenant(tenant),
                    tenant,
                    id,
                    serde_json::json!({}),
                    serde_json::json!({}),
                    None,
                    i32::MAX,
                )
                .await?;
                Ok::<_, TestError>(())
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(TestError::Repo(RepoError::InvalidStoredMoney(_)))
    ));
    let row = repo
        .read(&AccessScope::for_tenant(tenant), tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.revision, i32::MAX);
    assert_eq!(row.amount, Some(money("123.45", "EUR", 2)));
}

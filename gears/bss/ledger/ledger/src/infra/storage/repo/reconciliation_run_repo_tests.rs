//! Actual migrated SQLite checks for strict diagnostic reads and conservative purge.
use super::*;
use crate::infra::posting::retry::AttemptError;
use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

async fn setup() -> (ReconciliationRunRepo, Db, Uuid) {
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
        ReconciliationRunRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
    )
}
fn money(code: &str, scale: u8, amount: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(amount).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
async fn start(
    repo: &ReconciliationRunRepo,
    db: &Db,
    tenant: Uuid,
    id: Uuid,
    check: &str,
) -> Result<(), AttemptError> {
    let repo = repo.clone();
    let check = check.to_owned();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            repo.start(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                id,
                "2026-10",
                &check,
            )
            .await?;
            Ok(())
        })
    })
    .await
}
async fn finish(
    repo: &ReconciliationRunRepo,
    db: &Db,
    tenant: Uuid,
    id: Uuid,
    result: ReconciliationVariance,
    within: bool,
) -> Result<(), AttemptError> {
    let repo = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
        Box::pin(async move {
            repo.finalize(
                tx,
                &AccessScope::for_tenant(tenant),
                tenant,
                id,
                "DONE",
                &result,
                within,
                Some(9),
                Some(serde_json::json!({"source": "unchanged business detail"})),
            )
            .await?;
            Ok(())
        })
    })
    .await
}
async fn set_text(
    repo: &ReconciliationRunRepo,
    db: &Db,
    tenant: Uuid,
    id: Uuid,
    column: C,
    text: &str,
) {
    reconciliation_run::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .filter(key(tenant, id))
        .col_expr(column, Expr::value(text))
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    // Read through the production portable projection; no JSON-aware fixture decoder.
    assert_eq!(
        repo.stored(
            &db.conn().unwrap(),
            &AccessScope::for_tenant(tenant),
            key(tenant, id),
            1
        )
        .await
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn typed_zero_mixed_currency_canonical_order_and_count_roundtrip() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let id = Uuid::now_v7();
    start(&repo, &db, tenant, id, "AR_DERIVED").await.unwrap();
    assert_eq!(
        repo.read(&scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .variance,
        ReconciliationVariance::Money {
            by_currency: vec![]
        }
    );
    finish(
        &repo,
        &db,
        tenant,
        id,
        ReconciliationVariance::Money {
            by_currency: vec![
                money("JPY", 0, "-3"),
                money("EUR", 2, "0.09"),
                money("BTC", 28, "0.0000000000000000000000000001"),
            ],
        },
        false,
    )
    .await
    .unwrap();
    let row = repo.read(&scope, tenant, id).await.unwrap().unwrap();
    let ReconciliationVariance::Money { by_currency } = row.variance else {
        panic!("money expected")
    };
    assert_eq!(
        by_currency
            .iter()
            .map(|m| m.currency().code())
            .collect::<Vec<_>>(),
        vec!["BTC", "EUR", "JPY"]
    );
    assert_eq!(by_currency[1], money("EUR", 2, "0.09"));
    let raw = repo
        .stored(&db.conn().unwrap(), &scope, key(tenant, id), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(raw.variance.contains("0.09"));
    assert!(!raw.variance.contains("0.090"));
    let count = Uuid::now_v7();
    start(&repo, &db, tenant, count, "INVOICE_COMPLETENESS")
        .await
        .unwrap();
    assert_eq!(
        repo.read(&scope, tenant, count)
            .await
            .unwrap()
            .unwrap()
            .variance,
        ReconciliationVariance::MissingInvoices { count: 0 }
    );
    finish(
        &repo,
        &db,
        tenant,
        count,
        ReconciliationVariance::MissingInvoices { count: u64::MAX },
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        repo.read(&scope, tenant, count)
            .await
            .unwrap()
            .unwrap()
            .variance,
        ReconciliationVariance::MissingInvoices { count: u64::MAX }
    );
    assert!(
        repo.read(&AccessScope::for_tenant(Uuid::now_v7()), tenant, id)
            .await
            .unwrap()
            .is_none()
    );
    let rp = repo.clone();
    let observed: Result<ReconciliationVariance, AttemptError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                Ok(rp
                    .read_in(tx, &AccessScope::for_tenant(tenant), tenant, count)
                    .await?
                    .unwrap()
                    .variance)
            })
        })
        .await;
    assert_eq!(
        observed.unwrap(),
        ReconciliationVariance::MissingInvoices { count: u64::MAX }
    );
}

#[tokio::test]
async fn invalid_new_inputs_roll_back_and_cannot_spoof_conflict() {
    let (repo, db, tenant) = setup().await;
    let id = Uuid::now_v7();
    assert!(matches!(
        start(&repo, &db, tenant, id, "database is locked").await,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::InvalidRequest(_)
        ))
    ));
    start(&repo, &db, tenant, id, "AR_DERIVED").await.unwrap();
    for result in [
        ReconciliationVariance::MissingInvoices { count: 0 },
        ReconciliationVariance::Money {
            by_currency: vec![money("EUR", 2, "1"), money("EUR", 3, "2")],
        },
    ] {
        assert!(matches!(
            finish(&repo, &db, tenant, id, result, true).await,
            Err(AttemptError::Business(
                crate::domain::error::DomainError::InvalidRequest(_)
            ))
        ));
    }
    let rp = repo.clone();
    let result: Result<(), AttemptError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                rp.start(
                    tx,
                    &AccessScope::for_tenant(tenant),
                    tenant,
                    Uuid::from_u128(2),
                    "p",
                    "PAYMENTS_PSP",
                )
                .await?;
                rp.finalize(
                    tx,
                    &AccessScope::for_tenant(tenant),
                    tenant,
                    id,
                    "UNKNOWN",
                    &ReconciliationVariance::Money {
                        by_currency: vec![],
                    },
                    true,
                    None,
                    None,
                )
                .await?;
                Ok(())
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(AttemptError::Business(
            crate::domain::error::DomainError::InvalidRequest(_)
        ))
    ));
    assert!(
        repo.read(&AccessScope::for_tenant(tenant), tenant, Uuid::from_u128(2))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.read(&AccessScope::for_tenant(tenant), tenant, id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "RUNNING"
    );
    let rp = repo.clone();
    let result: Result<(), AttemptError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                rp.start(
                    tx,
                    &AccessScope::for_tenant(Uuid::now_v7()),
                    tenant,
                    Uuid::from_u128(3),
                    "p",
                    "AR_DERIVED",
                )
                .await?;
                Ok(())
            })
        })
        .await;
    assert!(result.is_err());
    assert!(start(&repo, &db, tenant, id, "AR_DERIVED").await.is_err());
}

#[tokio::test]
async fn malformed_noncanonical_closed_shape_and_metadata_preserve_evidence() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let id = Uuid::now_v7();
    start(&repo, &db, tenant, id, "AR_DERIVED").await.unwrap();
    finish(
        &repo,
        &db,
        tenant,
        id,
        ReconciliationVariance::Money {
            by_currency: vec![],
        },
        true,
    )
    .await
    .unwrap();
    let bad = [
        "{",
        r#"{"kind":"money","by_currency":[{"amount":"0.00","currency":"EUR","currency_scale":2}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"-0","currency":"EUR","currency_scale":2}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"1e0","currency":"EUR","currency_scale":2}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"0.001","currency":"EUR","currency_scale":2}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"0","currency":"eur","currency_scale":2}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"0","currency":"EUR","currency_scale":29}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"0","currency":"EUR"}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":0,"currency":"EUR","currency_scale":2}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"0","currency":"EUR","currency_scale":2,"extra":0}]}"#,
        r#"{"kind":"money","by_currency":[],"extra":0}"#,
        r#"{"kind":"missing_invoices","count":0}"#,
        r#"{"kind":"other","count":0}"#,
        r#"{"kind":"money","by_currency":[{"amount":"0","currency":"JPY","currency_scale":0},{"amount":"0","currency":"EUR","currency_scale":2}]}"#,
        r#"{"kind":"money","by_currency":[{"amount":"0","currency":"EUR","currency_scale":2},{"amount":"0","currency":"EUR","currency_scale":3}]}"#,
    ];
    for text in bad {
        set_text(&repo, &db, tenant, id, C::Variance, text).await;
        assert!(
            matches!(
                repo.read(&scope, tenant, id).await,
                Err(RepoError::InvalidStoredMoney(_))
            ),
            "{text}"
        );
        assert!(
            matches!(
                finish(
                    &repo,
                    &db,
                    tenant,
                    id,
                    ReconciliationVariance::Money {
                        by_currency: vec![]
                    },
                    true
                )
                .await,
                Err(AttemptError::Business(
                    crate::domain::error::DomainError::Internal(_)
                ))
            ),
            "{text}"
        );
        assert_eq!(
            repo.purge_uneventful_runs(tenant, 1).await.unwrap(),
            0,
            "{text}"
        );
        assert_eq!(
            repo.stored(&db.conn().unwrap(), &scope, key(tenant, id), 1)
                .await
                .unwrap()[0]
                .variance,
            text
        );
    }
    set_text(
        &repo,
        &db,
        tenant,
        id,
        C::Variance,
        r#"{"kind":"money","by_currency":[]}"#,
    )
    .await;
    set_text(&repo, &db, tenant, id, C::Detail, "{").await;
    assert!(matches!(
        repo.read(&scope, tenant, id).await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    assert_eq!(repo.purge_uneventful_runs(tenant, 1).await.unwrap(), 0);
}

#[tokio::test]
async fn purge_skips_preserved_prefix_retains_structure_and_respects_limit_and_tenant() {
    let (repo, db, tenant) = setup().await;
    let other = Uuid::now_v7();
    // More than one scan batch of malformed/nonzero evidence precedes the zeros.
    let rp = repo.clone();
    let result: Result<(), AttemptError> = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |tx| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                for n in 1..=132 {
                    let id = Uuid::from_u128(n);
                    rp.start(tx, &scope, tenant, id, "p", "AR_DERIVED").await?;
                    rp.finalize(
                        tx,
                        &scope,
                        tenant,
                        id,
                        "DONE",
                        &ReconciliationVariance::Money {
                            by_currency: vec![money("EUR", 2, "0.01")],
                        },
                        true,
                        None,
                        None,
                    )
                    .await?;
                }
                for (n, check, within) in [
                    (133, "AR_DERIVED", true),
                    (134, "INVOICE_COMPLETENESS", true),
                    (135, "AR_DERIVED", false),
                    (136, "INVOICE_COMPLETENESS", false),
                ] {
                    let id = Uuid::from_u128(n);
                    rp.start(tx, &scope, tenant, id, "p", check).await?;
                    rp.finalize(
                        tx,
                        &scope,
                        tenant,
                        id,
                        "DONE",
                        &variance::zero(check)?,
                        within,
                        None,
                        None,
                    )
                    .await?;
                }
                rp.start(tx, &scope, tenant, Uuid::from_u128(137), "p", "AR_DERIVED")
                    .await?;
                Ok(())
            })
        })
        .await;
    result.unwrap();
    set_text(&repo, &db, tenant, Uuid::from_u128(1), C::Variance, "{").await;
    start(&repo, &db, other, Uuid::from_u128(133), "AR_DERIVED")
        .await
        .unwrap();
    finish(
        &repo,
        &db,
        other,
        Uuid::from_u128(133),
        ReconciliationVariance::Money {
            by_currency: vec![],
        },
        true,
    )
    .await
    .unwrap();
    assert_eq!(repo.purge_uneventful_runs(tenant, 0).await.unwrap(), 0);
    assert_eq!(repo.purge_uneventful_runs(tenant, 1).await.unwrap(), 1);
    assert!(
        repo.read(
            &AccessScope::for_tenant(tenant),
            tenant,
            Uuid::from_u128(133)
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(repo.purge_uneventful_runs(tenant, 10).await.unwrap(), 1);
    assert_eq!(repo.purge_uneventful_runs(tenant, 10).await.unwrap(), 0);
    for n in [135, 136, 137] {
        assert!(
            repo.read(&AccessScope::for_tenant(tenant), tenant, Uuid::from_u128(n))
                .await
                .unwrap()
                .is_some()
        );
    }
    assert!(
        repo.read(&AccessScope::for_tenant(other), other, Uuid::from_u128(133))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        repo.stored(
            &db.conn().unwrap(),
            &AccessScope::for_tenant(tenant),
            C::TenantId.eq(tenant).into(),
            1000
        )
        .await
        .unwrap()
        .len(),
        135
    );
}

#[tokio::test]
async fn deletion_guard_rejects_stale_state_money_detail_and_tolerance() {
    let (repo, db, tenant) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    let id = Uuid::now_v7();
    start(&repo, &db, tenant, id, "AR_DERIVED").await.unwrap();
    for column in [
        C::Status,
        C::Variance,
        C::Detail,
        C::WithinTolerance,
        C::Watermark,
        C::CheckType,
    ] {
        finish(
            &repo,
            &db,
            tenant,
            id,
            ReconciliationVariance::Money {
                by_currency: vec![],
            },
            true,
        )
        .await
        .unwrap();
        let conn = db.conn().unwrap();
        let old = repo
            .stored(&conn, &scope, key(tenant, id), 1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let value = match column {
            C::Status => Expr::value("RUNNING"),
            C::Variance => Expr::value(
                serde_json::json!({"kind":"money","by_currency":[{"amount":"1","currency":"EUR","currency_scale":2}]}),
            ),
            C::Detail => Expr::value(serde_json::json!({"different":true})),
            C::WithinTolerance => Expr::value(false),
            C::Watermark => Expr::value(10_i64),
            C::CheckType => Expr::value("PAYMENTS_PSP"),
            _ => unreachable!(),
        };
        reconciliation_run::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .filter(key(tenant, id))
            .col_expr(column, value)
            .exec(&conn)
            .await
            .unwrap();
        assert_eq!(
            repo.delete_observed_zero(&conn, &scope, &old)
                .await
                .unwrap(),
            0
        );
        assert!(repo.read(&scope, tenant, id).await.unwrap().is_some());
    }
}

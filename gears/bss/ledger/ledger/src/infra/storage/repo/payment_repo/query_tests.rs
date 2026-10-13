//! Actual production queries against the full fresh SQLite schema and SecureORM.
use super::*;
use crate::infra::posting::retry::AttemptError;
use sea_orm::{ActiveValue::Set, TryIntoModel};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{Db, SecureInsertExt, SecureUpdateExt, TxConfig};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};

async fn setup() -> (PaymentRepo, Db, Uuid, Uuid) {
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
        PaymentRepo::new(DBProvider::new(db.clone())),
        db,
        Uuid::now_v7(),
        Uuid::now_v7(),
    )
}
fn ar(
    tenant: Uuid,
    payer: Uuid,
    account: Uuid,
    id: &str,
    balance: &str,
    functional: Option<&str>,
) -> ar_invoice_balance::ActiveModel {
    ar_invoice_balance::ActiveModel {
        tenant_id: Set(tenant),
        payer_tenant_id: Set(payer),
        account_id: Set(account),
        invoice_id: Set(id.into()),
        currency: Set("EUR".into()),
        currency_scale: Set(2),
        balance: Set(balance.into()),
        disputed: Set("0".into()),
        functional_balance: Set(functional.map(str::to_owned)),
        functional_currency: Set(functional.map(|_| "USD".into())),
        functional_currency_scale: Set(functional.map(|_| 2)),
        original_posted_at: Set(Some(OffsetDateTime::UNIX_EPOCH)),
        due_date: Set(None),
        last_entry_seq: Set(None),
        version: Set(7),
    }
}
async fn insert_ar(db: &Db, row: ar_invoice_balance::ActiveModel) {
    ar_invoice_balance::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::allow_all(), &row)
        .unwrap()
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
}
fn amount(m: &PostedMoney) -> String {
    bss_ledger_sdk::canonical_decimal(m.amount())
}

#[tokio::test]
async fn candidates_revaluation_and_carry_preserve_numeric_values_keys_and_order() {
    let (repo, db, tenant, payer) = setup().await;
    let scope = AccessScope::for_tenant(tenant);
    for (id, value) in [("a", "2"), ("b", "10"), ("c", "0.01"), ("d", "0")] {
        insert_ar(
            &db,
            ar(tenant, payer, Uuid::now_v7(), id, value, Some(value)),
        )
        .await;
        let row = reusable_credit_subbalance::ActiveModel {
            tenant_id: Set(tenant),
            payer_tenant_id: Set(payer),
            account_id: Set(Uuid::now_v7()),
            currency: Set("EUR".into()),
            currency_scale: Set(2),
            credit_grant_event_type: Set(id.into()),
            first_granted_at: Set(Some(OffsetDateTime::UNIX_EPOCH)),
            balance: Set(value.into()),
            functional_balance: Set(Some(value.into())),
            functional_currency: Set(Some("USD".into())),
            functional_currency_scale: Set(Some(2)),
            last_entry_seq: Set(None),
            version: Set(3),
        };
        reusable_credit_subbalance::Entity::insert(row.clone())
            .secure()
            .scope_with_model(&scope, &row)
            .unwrap()
            .exec(&db.conn().unwrap())
            .await
            .unwrap();
    }
    let invoices = repo
        .list_open_ar_invoices(&scope, tenant, payer, "EUR")
        .await
        .unwrap();
    assert_eq!(
        invoices
            .iter()
            .map(|r| (r.invoice_id.as_str(), amount(&r.balance)))
            .collect::<Vec<_>>(),
        vec![("a", "2".into()), ("b", "10".into()), ("c", "0.01".into())]
    );
    assert!(
        invoices
            .iter()
            .all(|r| r.version == 7 && r.balance.currency().scale() == 2)
    );
    let credits = repo
        .list_credit_subgrains(&scope, tenant, payer, "EUR")
        .await
        .unwrap();
    assert_eq!(
        credits
            .iter()
            .map(|r| (r.credit_grant_event_type.as_str(), amount(&r.available)))
            .collect::<Vec<_>>(),
        vec![("a", "2".into()), ("b", "10".into()), ("c", "0.01".into())]
    );
    assert!(credits.iter().all(|r| r.version == 3));
    assert_eq!(
        repo.list_ar_invoices_to_revalue(&scope, tenant)
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        repo.list_reusable_credit_to_revalue(&scope, tenant)
            .await
            .unwrap()
            .len(),
        3
    );
    let zero = repo
        .read_ar_invoice_carried(&scope, tenant, payer, "d", "EUR")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(amount(&zero.balance), "0");
    assert_eq!(zero.rows.len(), 1);
    assert!(
        repo.read_ar_invoice_carried(&scope, tenant, payer, "absent", "EUR")
            .await
            .unwrap()
            .is_none()
    );
    for denied in [
        AccessScope::for_tenant(Uuid::now_v7()),
        AccessScope::default(),
    ] {
        assert!(
            repo.list_open_ar_invoices(&denied, tenant, payer, "EUR")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.list_credit_subgrains(&denied, tenant, payer, "EUR")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.list_ar_invoices_to_revalue(&denied, tenant)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.list_reusable_credit_to_revalue(&denied, tenant)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.read_ar_invoice_carried(&denied, tenant, payer, "a", "EUR")
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn invoice_totals_narrow_only_after_all_actual_rows_and_reject_mixed_metadata() {
    let (repo, db, tenant, payer) = setup().await;
    let s = AccessScope::allow_all();
    let max = "9999999999999999999999999999";
    // Functional amounts may be signed; first two terms exceed the posted bound.
    for (n, value) in [
        (1, max.to_owned()),
        (2, max.to_owned()),
        (3, format!("-{max}")),
    ] {
        insert_ar(
            &db,
            ar(
                tenant,
                payer,
                Uuid::from_u128(n),
                "cancel",
                "0.01",
                Some(&value),
            ),
        )
        .await;
    }
    let total = repo
        .read_ar_invoice_carried(&s, tenant, payer, "cancel", "EUR")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(amount(&total.balance), "0.03");
    assert_eq!(amount(total.functional_balance.as_ref().unwrap()), max);
    assert_eq!(
        total.rows.iter().map(|r| r.account_id).collect::<Vec<_>>(),
        vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)]
    );
    for n in [4, 5] {
        insert_ar(
            &db,
            ar(tenant, payer, Uuid::from_u128(n), "overflow", max, None),
        )
        .await;
    }
    assert!(matches!(
        repo.read_ar_invoice_carried(&s, tenant, payer, "overflow", "EUR")
            .await,
        Err(RepoError::Money(
            bss_ledger_sdk::MoneyError::AmountOutOfRange
        ))
    ));
    for (id, functional) in [("mixed", None), ("mixed", Some("1"))] {
        insert_ar(&db, ar(tenant, payer, Uuid::now_v7(), id, "1", functional)).await;
    }
    assert!(matches!(
        repo.read_ar_invoice_carried(&s, tenant, payer, "mixed", "EUR")
            .await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    for n in [6, 7] {
        insert_ar(
            &db,
            ar(
                tenant,
                payer,
                Uuid::from_u128(n),
                "functional-overflow",
                "0",
                Some(max),
            ),
        )
        .await;
    }
    assert!(matches!(
        repo.read_ar_invoice_carried(&s, tenant, payer, "functional-overflow", "EUR")
            .await,
        Err(RepoError::Money(
            bss_ledger_sdk::MoneyError::AmountOutOfRange
        ))
    ));
    let mut fscale = ar(
        tenant,
        payer,
        Uuid::now_v7(),
        "functional-scale",
        "1",
        Some("1"),
    );
    fscale.functional_currency_scale = Set(Some(0));
    insert_ar(&db, fscale).await;
    insert_ar(
        &db,
        ar(
            tenant,
            payer,
            Uuid::now_v7(),
            "functional-scale",
            "1",
            Some("1"),
        ),
    )
    .await;
    assert!(matches!(
        repo.read_ar_invoice_carried(&s, tenant, payer, "functional-scale", "EUR")
            .await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    let mut other_scale = ar(tenant, payer, Uuid::now_v7(), "scale", "1", Some("1"));
    other_scale.currency_scale = Set(0);
    insert_ar(&db, other_scale).await;
    insert_ar(
        &db,
        ar(tenant, payer, Uuid::now_v7(), "scale", "1", Some("1")),
    )
    .await;
    assert!(matches!(
        repo.read_ar_invoice_carried(&s, tenant, payer, "scale", "EUR")
            .await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    let mut other_functional = ar(tenant, payer, Uuid::now_v7(), "functional", "1", Some("1"));
    other_functional.functional_currency = Set(Some("JPY".into()));
    insert_ar(&db, other_functional).await;
    insert_ar(
        &db,
        ar(tenant, payer, Uuid::now_v7(), "functional", "1", Some("1")),
    )
    .await;
    assert!(matches!(
        repo.read_ar_invoice_carried(&s, tenant, payer, "functional", "EUR")
            .await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[tokio::test]
async fn unique_pool_account_absence_signed_money_and_caller_transaction_reads() {
    let (repo, db, tenant, payer) = setup().await;
    let s = AccessScope::allow_all();
    let account = Uuid::now_v7();
    assert!(
        repo.read_unallocated(&s, tenant, payer, "EUR")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.read_unallocated_carried(&s, tenant, payer, "EUR")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.read_account_carried(&s, tenant, account, "EUR")
            .await
            .unwrap()
            .is_none()
    );
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let row = unallocated_balance::ActiveModel {
                tenant_id: Set(tenant),
                payer_tenant_id: Set(payer),
                account_id: Set(account),
                currency: Set("EUR".into()),
                currency_scale: Set(28),
                balance: Set("0.01".into()),
                functional_balance: Set(Some("-2".into())),
                functional_currency: Set(Some("USD".into())),
                functional_currency_scale: Set(Some(0)),
                last_entry_seq: Set(None),
                version: Set(9),
            };
            unallocated_balance::Entity::insert(row.clone())
                .secure()
                .scope_with_model(&s, &row)
                .unwrap()
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, sea_orm::DbBackend::Sqlite))?;
            let row = account_balance::ActiveModel {
                tenant_id: Set(tenant),
                account_id: Set(account),
                currency: Set("EUR".into()),
                currency_scale: Set(28),
                account_class: Set("REVENUE".into()),
                normal_side: Set("CR".into()),
                balance: Set("-10".into()),
                functional_balance: Set(None),
                functional_currency: Set(None),
                functional_currency_scale: Set(None),
                last_entry_seq: Set(None),
                version: Set(11),
            };
            account_balance::Entity::insert(row.clone())
                .secure()
                .scope_with_model(&s, &row)
                .unwrap()
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, sea_orm::DbBackend::Sqlite))?;
            assert_eq!(
                amount(
                    &r.read_unallocated_in(txn, &s, tenant, payer, "EUR")
                        .await?
                        .unwrap()
                ),
                "0.01"
            );
            let pool = r
                .read_unallocated_carried_in(txn, &s, tenant, payer, "EUR")
                .await?
                .unwrap();
            assert_eq!(
                (
                    pool.account_id,
                    pool.version,
                    pool.balance.currency().scale()
                ),
                (account, 9, 28)
            );
            assert_eq!(
                pool.functional_balance.as_ref().unwrap().currency().scale(),
                0
            );
            assert_eq!(amount(pool.functional_balance.as_ref().unwrap()), "-2");
            let account = r
                .read_account_carried_in(txn, &s, tenant, account, "EUR")
                .await?
                .unwrap();
            assert_eq!(
                (amount(&account.balance), account.version),
                ("-10".into(), 11)
            );
            assert_eq!(
                r.list_unallocated_to_revalue_in(txn, &s, tenant)
                    .await?
                    .len(),
                1
            );
            assert!(
                r.list_open_ar_invoices_in(txn, &s, tenant, payer, "EUR")
                    .await?
                    .is_empty()
            );
            assert!(
                r.list_credit_subgrains_in(txn, &s, tenant, payer, "EUR")
                    .await?
                    .is_empty()
            );
            assert!(
                r.list_ar_invoices_to_revalue_in(txn, &s, tenant)
                    .await?
                    .is_empty()
            );
            assert!(
                r.list_reusable_credit_to_revalue_in(txn, &s, tenant)
                    .await?
                    .is_empty()
            );
            assert!(
                r.read_ar_invoice_carried_in(txn, &s, tenant, payer, "none", "EUR")
                    .await?
                    .is_none()
            );
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    assert_eq!(
        repo.list_unallocated_to_revalue(&s, tenant)
            .await
            .unwrap()
            .len(),
        1
    );
    for denied in [
        AccessScope::for_tenant(Uuid::now_v7()),
        AccessScope::default(),
    ] {
        assert!(
            repo.read_unallocated(&denied, tenant, payer, "EUR")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.read_unallocated_carried(&denied, tenant, payer, "EUR")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.read_account_carried(&denied, tenant, account, "EUR")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.list_unallocated_to_revalue(&denied, tenant)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn stored_corruption_fails_before_monetary_filtering() {
    let (repo, db, tenant, payer) = setup().await;
    let account = Uuid::now_v7();
    insert_ar(&db, ar(tenant, payer, account, "corrupt", "0", None)).await;
    // Cross-column caps are application checks on SQLite; zero must not hide corruption.
    ar_invoice_balance::Entity::update_many()
        .col_expr(
            ar_invoice_balance::Column::Disputed,
            sea_orm::sea_query::Expr::value("1"),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        repo.list_open_ar_invoices(&AccessScope::allow_all(), tenant, payer, "EUR")
            .await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    assert!(matches!(
        repo.list_ar_invoices_to_revalue(&AccessScope::allow_all(), tenant)
            .await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
    // Fresh DB checks reject malformed text/triples; exercise the same production decoder with stored-row models.
    let mut model: ar_invoice_balance::Model = ar(tenant, payer, account, "x", "0", None)
        .try_into_model()
        .unwrap();
    for text in ["01", "1.0", "1e2", "not money"] {
        model.balance = text.into();
        assert!(matches!(
            invoice(model.clone()),
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
    model.balance = "0".into();
    model.functional_currency = Some("USD".into());
    assert!(matches!(
        invoice(model.clone()),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    model.functional_currency = None;
    model.currency_scale = 29;
    assert!(matches!(
        invoice(model.clone()),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    model.currency_scale = 2;
    model.version = -1;
    assert!(matches!(
        invoice(model),
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[tokio::test]
async fn dedup_and_effective_policy_use_the_supplied_transaction_and_scope() {
    let (repo, db, tenant, _) = setup().await;
    let r = repo.clone();
    let entry = Uuid::now_v7();
    let at = OffsetDateTime::UNIX_EPOCH;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            for (id, status, result) in [
                ("posted", "POSTED", Some(entry)),
                ("claimed", "CLAIMED", None),
                ("queued", "QUEUED", None),
            ] {
                let row = idempotency_dedup::ActiveModel {
                    tenant_id: Set(tenant),
                    flow: Set(SourceDocType::ManualAdjustment.as_str().into()),
                    business_id: Set(id.into()),
                    payload_hash: Set("hash".into()),
                    result_entry_id: Set(result),
                    result_schedule_id: Set(None),
                    posted_at_utc: Set(result.map(|_| at)),
                    status: Set(status.into()),
                    retain_until: Set(None),
                };
                idempotency_dedup::Entity::insert(row.clone())
                    .secure()
                    .scope_with_model(&s, &row)
                    .unwrap()
                    .exec(txn)
                    .await
                    .map_err(|e| scope_to_repo(e, sea_orm::DbBackend::Sqlite))?;
                assert_eq!(
                    r.lookup_dedup_status_in(txn, &s, tenant, SourceDocType::ManualAdjustment, id)
                        .await?
                        .unwrap(),
                    (status.into(), result, "hash".into())
                );
                assert_eq!(
                    r.lookup_finalized_post_in(
                        txn,
                        &s,
                        tenant,
                        SourceDocType::ManualAdjustment,
                        id
                    )
                    .await?,
                    result.map(|id| (id, "hash".into()))
                );
            }
            assert!(
                r.read_effective_policy_in(txn, &s, tenant, at)
                    .await?
                    .is_none()
            );
            for (version, seconds, strategy) in [
                (1, 0, "oldest-first.v1"),
                (2, 0, "highest-amount-first.v1"),
                (3, 1, "oldest-first.v1"),
            ] {
                let row = tenant_precedence_policy::ActiveModel {
                    tenant_id: Set(tenant),
                    version: Set(version),
                    effective_from: Set(at + time::Duration::seconds(seconds)),
                    strategy: Set(strategy.into()),
                    created_at_utc: Set(at),
                };
                tenant_precedence_policy::Entity::insert(row.clone())
                    .secure()
                    .scope_with_model(&s, &row)
                    .unwrap()
                    .exec(txn)
                    .await
                    .map_err(|e| scope_to_repo(e, sea_orm::DbBackend::Sqlite))?;
            }
            assert_eq!(
                r.read_effective_policy_in(txn, &s, tenant, at).await?,
                Some((PrecedenceStrategy::HighestAmountFirst, 2))
            );
            assert_eq!(
                r.read_effective_policy_in(txn, &s, tenant, at + time::Duration::seconds(1))
                    .await?,
                Some((PrecedenceStrategy::OldestFirst, 3))
            );
            for denied in [
                AccessScope::for_tenant(Uuid::now_v7()),
                AccessScope::default(),
            ] {
                assert!(
                    r.lookup_dedup_status_in(
                        txn,
                        &denied,
                        tenant,
                        SourceDocType::ManualAdjustment,
                        "posted"
                    )
                    .await?
                    .is_none()
                );
                assert!(
                    r.lookup_finalized_post_in(
                        txn,
                        &denied,
                        tenant,
                        SourceDocType::ManualAdjustment,
                        "posted"
                    )
                    .await?
                    .is_none()
                );
                assert!(
                    r.read_effective_policy_in(txn, &denied, tenant, at)
                        .await?
                        .is_none()
                );
            }
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let s = AccessScope::for_tenant(tenant);
    assert_eq!(
        repo.lookup_finalized_post(&s, tenant, SourceDocType::ManualAdjustment, "posted")
            .await
            .unwrap(),
        Some((entry, "hash".into()))
    );
    assert_eq!(
        repo.lookup_dedup_status(&s, tenant, SourceDocType::ManualAdjustment, "claimed")
            .await
            .unwrap()
            .unwrap()
            .0,
        "CLAIMED"
    );
    assert_eq!(
        repo.read_effective_policy(&s, tenant, at).await.unwrap(),
        Some((PrecedenceStrategy::HighestAmountFirst, 2))
    );
}

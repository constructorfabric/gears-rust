//! Tests for [`super`]'s pure `derive_grains` — grain fan-out, delta sign,
//! and the missing-`normal_side` error. (DE1101: kept out of the impl file.)

use super::*;
use crate::infra::posting::retry::AttemptError;
use bss_ledger_sdk::parse_decimal;
use bss_ledger_sdk::{MappingStatus, SourceDocType};
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, connect_db};

fn entry(tenant: Uuid) -> NewEntry {
    NewEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: "202606".to_owned(),
        entry_currency: "USD".to_owned(),
        source_doc_type: SourceDocType::ManualAdjustment,
        source_business_id: "biz-1".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: tenant,
        correlation_id: tenant,
        rounding_evidence: serde_json::Value::Null,
        rate_snapshot_ref: None,
    }
}

fn line(account: Uuid, class: AccountClass, side: Side, amount: i64, payer: Uuid) -> NewLine {
    NewLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: payer,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: account,
        account_class: class,
        gl_code: None,
        side,
        money: money(&amount.to_string(), "USD", 2),
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
        mapping_status: MappingStatus::Resolved,
        functional_money: None,
        tax_jurisdiction: None,
        tax_filing_period: None,
        tax_rate_ref: None,
        legal_entity_id: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        ar_status: None,
    }
}

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
fn grain(class: AccountClass) -> GrainDelta {
    let id = Uuid::now_v7();
    derive_grains(
        &entry(id),
        &[line(id, class, Side::Debit, 1, id)],
        &HashMap::from([(id, Side::Debit)]),
    )
    .unwrap()
    .remove(0)
}
#[test]
fn exact_delta_can_exceed_bound_then_cancel_with_original() {
    let mut g = grain(AccountClass::CashClearing);
    let max = money("9999999999999999999999999999", "USD", 2);
    g.delta = ExactAmount::from_decimal(max.amount())
        .checked_add(&ExactAmount::from_decimal(Decimal::ONE))
        .unwrap();
    let original = money("-1", "USD", 2);
    assert_eq!(final_balances(&g, Some(&original), None).unwrap().0, max);
    assert!(matches!(
        final_balances(&g, None, None),
        Err(ProjectError::Repo(RepoError::Money(
            MoneyError::AmountOutOfRange
        )))
    ));
}
#[test]
fn actual_keys_and_metadata_are_enforced() {
    let id = Uuid::now_v7();
    let sides = HashMap::from([(id, Side::Debit)]);
    let a = line(id, AccountClass::Ar, Side::Debit, 1, Uuid::now_v7());
    let mut b = a.clone();
    b.payer_tenant_id = Uuid::now_v7();
    let gs = derive_grains(&entry(id), &[a.clone(), b], &sides).unwrap();
    assert_eq!(
        gs.iter()
            .filter(|g| g.table_rank == GrainTable::Account)
            .count(),
        1
    );
    let mut b = a.clone();
    b.money = money("1", "USD", 3);
    assert!(matches!(
        derive_grains(&entry(id), &[a.clone(), b], &sides),
        Err(ProjectError::Repo(RepoError::Money(
            MoneyError::ScaleMismatch
        )))
    ));
    let mut b = a.clone();
    b.functional_money = Some(money("1", "EUR", 2));
    assert!(derive_grains(&entry(id), &[a.clone(), b], &sides).is_err());
    let mut a = a;
    a.invoice_id = Some("invoice".into());
    let mut b = a.clone();
    b.money = money("1", "EUR", 2);
    assert!(matches!(
        derive_grains(&entry(id), &[a, b], &sides),
        Err(ProjectError::Repo(RepoError::Money(
            MoneyError::CurrencyMismatch
        )))
    ));
}
#[test]
fn stored_metadata_and_version_limits_are_checked() {
    let mut g = grain(AccountClass::Ar);
    assert!(final_balances(&g, Some(&money("1", "USD", 3)), None).is_err());
    g.functional_currency = Some(CurrencySpec::try_new("EUR".into(), 2).unwrap());
    assert!(final_balances(&g, Some(&money("1", "USD", 2)), None).is_err());
    assert!(final_balances(&g, None, None).is_ok());
    assert!(next_version(i64::MAX).is_err());
    assert_eq!(next_version(0).unwrap(), 1);
    assert!(matches!(require_one(0), Err(ProjectError::Conflict)));
    assert!(matches!(require_one(2), Err(ProjectError::Conflict)));
    assert!(require_one(1).is_ok());
}
#[test]
fn coalescing_cancels_large_terms_before_narrowing_and_preserves_order() {
    let id = Uuid::now_v7();
    let mut a = line(id, AccountClass::Ar, Side::Debit, 1, id);
    a.money = money("9999999999999999999999999999", "USD", 2);
    let mut b = a.clone();
    b.side = Side::Credit;
    let gs = derive_grains(
        &entry(id),
        &[a.clone(), a, b],
        &HashMap::from([(id, Side::Debit)]),
    )
    .unwrap();
    assert_eq!(gs.len(), 2);
    assert!(gs.windows(2).all(|w| w[0].sort_key() < w[1].sort_key()));
    assert_eq!(
        final_balances(&gs[0], None, None)
            .unwrap()
            .0
            .amount()
            .to_string(),
        "9999999999999999999999999999"
    );
}

/// Exercise all six actual writers against the full fresh SQLite migration chain.
#[allow(clippy::too_many_lines)] // one scenario end to end; splitting hides the state transitions
#[tokio::test]
async fn sqlite_all_caches_insert_cas_metadata_and_rollback() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let tenant = Uuid::now_v7();
    let header = entry(tenant);
    let mut lines = Vec::new();
    let mut sides = HashMap::new();
    for class in [
        AccountClass::Ar,
        AccountClass::Unallocated,
        AccountClass::ReusableCredit,
        AccountClass::TaxPayable,
    ] {
        let id = Uuid::now_v7();
        sides.insert(id, Side::Debit);
        let mut l = line(id, class, Side::Debit, 10, tenant);
        l.functional_money = Some(money("12", "EUR", 3));
        l.invoice_id = Some("invoice".into());
        l.credit_grant_event_type = Some("grant".into());
        l.tax_jurisdiction = Some("US".into());
        l.tax_filing_period = Some("2026".into());
        lines.push(l);
    }
    for seq in [1, 2] {
        let mut header = header.clone();
        if seq == 2 {
            header.posted_at_utc += time::Duration::days(1);
        }
        let mut lines = lines.clone();
        if seq == 2 {
            for l in &mut lines {
                l.due_date = Some(chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
            }
        }
        let sides = sides.clone();
        db.transaction_ref_mapped_with_config(
            toolkit_db::secure::TxConfig::serializable(),
            move |txn| {
                Box::pin(async move {
                    BalanceProjector::new(sea_orm::DbBackend::Sqlite)
                        .project(txn, &AccessScope::allow_all(), &header, &lines, &sides, seq)
                        .await
                        .map_err(project_error)
                })
            },
        )
        .await
        .unwrap();
    }
    let original_time = header.posted_at_utc;
    let check = |expected: &'static str| {
        db.transaction_ref(move |txn| {
            Box::pin(async move {
                let scope = AccessScope::allow_all();
                macro_rules! check {
                    ($entity:ident) => {{
                        let rows = $entity::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .all(txn)
                            .await
                            .unwrap();
                        assert!(!rows.is_empty());
                        for r in rows {
                            assert_eq!(r.balance, expected);
                            assert_eq!(r.version, 1);
                            assert_eq!(r.last_entry_seq, Some(2));
                        }
                    }};
                }
                check!(account_balance);
                check!(ar_payer_balance);
                check!(ar_invoice_balance);
                check!(unallocated_balance);
                check!(reusable_credit_subbalance);
                check!(tax_subbalance);
                let r = ar_invoice_balance::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .one(txn)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(r.functional_balance.as_deref(), Some("24"));
                assert_eq!(r.functional_currency_scale, Some(3));
                assert_eq!(r.original_posted_at, Some(original_time));
                assert_eq!(r.due_date, None);
                let r = reusable_credit_subbalance::Entity::find()
                    .secure()
                    .scope_with(&scope)
                    .one(txn)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(r.first_granted_at, Some(original_time));
                Ok::<_, toolkit_db::DbError>(())
            })
        })
    };
    check("20").await.unwrap();
    // Each writer must reject metadata drift against its own existing row.
    let grains = derive_grains(&header, &lines, &sides).unwrap();
    db.transaction_ref(move |txn| {
        Box::pin(async move {
            let p = BalanceProjector::new(sea_orm::DbBackend::Sqlite);
            let scope = AccessScope::allow_all();
            for original in grains {
                let mut g = original.clone();
                g.currency_spec = CurrencySpec::try_new("USD".into(), 3).unwrap();
                let result = write_grain(&p, txn, &scope, &g).await;
                assert!(matches!(
                    result,
                    Err(ProjectError::Repo(RepoError::Money(
                        MoneyError::ScaleMismatch
                    )))
                ));
                if original.table_rank != GrainTable::Tax {
                    let mut g = original;
                    g.functional_currency = None;
                    assert!(matches!(
                        write_grain(&p, txn, &scope, &g).await,
                        Err(ProjectError::Repo(RepoError::Money(
                            MoneyError::CurrencyMismatch
                        )))
                    ));
                }
            }
            Ok::<_, toolkit_db::DbError>(())
        })
    })
    .await
    .unwrap();
    // The actual intended-key duplicate insert is a typed retry conflict.
    let conflict = db
        .transaction_ref_mapped_with_config(
            toolkit_db::secure::TxConfig::serializable(),
            move |txn| {
                Box::pin(async move {
                    use sea_orm::IntoActiveModel;
                    let scope = AccessScope::allow_all();
                    let am = account_balance::Entity::find()
                        .secure()
                        .scope_with(&scope)
                        .one(txn)
                        .await
                        .unwrap()
                        .unwrap()
                        .into_active_model();
                    let result = account_balance::Entity::insert(am.clone())
                        .secure()
                        .scope_with_model(&scope, &am)
                        .unwrap()
                        .exec(txn)
                        .await;
                    Err::<(), _>(AttemptError::from(insert_to_repo(
                        result.unwrap_err(),
                        sea_orm::DbBackend::Sqlite,
                    )))
                })
            },
        )
        .await;
    assert!(matches!(conflict, Err(AttemptError::Conflict)));
    check("20").await.unwrap();
    // A last-table metadata error must roll back the earlier five cache writes.
    let mut bad = lines.clone();
    let tax = bad.last_mut().unwrap();
    tax.money = money("10", "EUR", 2);
    let h = header.clone();
    let n = sides.clone();
    let result = db
        .transaction_ref_mapped_with_config(
            toolkit_db::secure::TxConfig::serializable(),
            move |txn| {
                Box::pin(async move {
                    BalanceProjector::new(sea_orm::DbBackend::Sqlite)
                        .project(txn, &AccessScope::allow_all(), &h, &bad, &n, 3)
                        .await
                        .map_err(project_error)
                })
            },
        )
        .await;
    assert!(result.is_err());
    check("20").await.unwrap();
    // Disputed cannot exceed the correlated total even on SQLite TEXT storage.
    let mut bad = lines.clone();
    bad[0].ar_status = Some(AR_STATUS_DISPUTED.into());
    bad[0].money = money("30", "USD", 2);
    let mut offset = bad[0].clone();
    offset.ar_status = None;
    offset.side = Side::Credit;
    bad.push(offset);
    let h = header.clone();
    let n = sides.clone();
    assert!(
        db.transaction_ref_mapped_with_config(
            toolkit_db::secure::TxConfig::serializable(),
            move |txn| Box::pin(async move {
                BalanceProjector::new(sea_orm::DbBackend::Sqlite)
                    .project(txn, &AccessScope::allow_all(), &h, &bad, &n, 3)
                    .await
                    .map_err(project_error)
            })
        )
        .await
        .is_err()
    );
    check("20").await.unwrap();
    // Spending below zero aborts all caches.
    let bad: Vec<_> = lines
        .iter()
        .cloned()
        .map(|mut l| {
            l.side = Side::Credit;
            l.money = money("21", "USD", 2);
            l
        })
        .collect();
    assert!(
        db.transaction_ref_mapped_with_config(
            toolkit_db::secure::TxConfig::serializable(),
            move |txn| Box::pin(async move {
                BalanceProjector::new(sea_orm::DbBackend::Sqlite)
                    .project(txn, &AccessScope::allow_all(), &header, &bad, &sides, 3)
                    .await
                    .map_err(project_error)
            })
        )
        .await
        .is_err()
    );
    check("20").await.unwrap();
}
fn project_error(error: ProjectError) -> AttemptError {
    match error {
        ProjectError::Repo(e) => e.into(),
        ProjectError::Conflict => AttemptError::Conflict,
        e => crate::domain::error::DomainError::Internal(e.to_string()).into(),
    }
}

/// Direct writer calls make every cache's existing-row checks independently observable.
async fn write_grain(
    p: &BalanceProjector,
    txn: &DbTx<'_>,
    scope: &AccessScope,
    g: &GrainDelta,
) -> Result<(), ProjectError> {
    match g.table_rank {
        GrainTable::Account => p.upsert_account_balance(txn, scope, g, 3).await,
        GrainTable::ArPayer => p.upsert_ar_payer(txn, scope, g, 3).await,
        GrainTable::ArInvoice => p.upsert_ar_invoice(txn, scope, g, 3).await,
        GrainTable::Unallocated => p.upsert_unallocated(txn, scope, g, 3).await,
        GrainTable::ReusableCredit => p.upsert_reusable_credit(txn, scope, g, 3).await,
        GrainTable::Tax => p.upsert_tax(txn, scope, g, 3).await,
    }
}

#[test]
fn wallet_identity_omits_account_and_tax_identity_omits_payer_and_currency() {
    let id = Uuid::now_v7();
    let other = Uuid::now_v7();
    let sides = HashMap::from([(id, Side::Debit), (other, Side::Debit)]);
    for class in [AccountClass::Unallocated, AccountClass::ReusableCredit] {
        let mut a = line(id, class, Side::Debit, 1, id);
        a.credit_grant_event_type = Some("grant".into());
        let mut b = a.clone();
        b.account_id = other;
        assert!(derive_grains(&entry(id), &[a, b], &sides).is_err());
    }
    let mut a = line(id, AccountClass::TaxPayable, Side::Debit, 1, id);
    a.tax_jurisdiction = Some("US".into());
    a.tax_filing_period = Some("2026".into());
    let mut b = a.clone();
    b.payer_tenant_id = other;
    let grains = derive_grains(&entry(id), &[a.clone(), b], &sides).unwrap();
    assert_eq!(
        grains
            .iter()
            .filter(|g| g.table_rank == GrainTable::Tax)
            .count(),
        1
    );
    let mut b = a.clone();
    b.money = money("1", "EUR", 2);
    assert!(matches!(
        derive_grains(&entry(id), &[a, b], &sides),
        Err(ProjectError::Repo(RepoError::Money(
            MoneyError::CurrencyMismatch
        )))
    ));
}

#[test]
fn functional_only_and_wallet_nonnegative_policies_survive() {
    let mut g = grain(AccountClass::CashClearing);
    g.delta = ExactAmount::from_decimal(Decimal::ZERO);
    g.functional_currency = Some(CurrencySpec::try_new("EUR".into(), 2).unwrap());
    g.functional_delta = ExactAmount::from_decimal(Decimal::from(3));
    let (total, functional) = final_balances(&g, None, None).unwrap();
    assert_eq!(total.amount(), Decimal::ZERO);
    assert_eq!(functional.unwrap().amount(), Decimal::from(3));
    let max = money("9999999999999999999999999999", "EUR", 2);
    g.functional_delta = ExactAmount::from_decimal(max.amount())
        .checked_add(&ExactAmount::from_decimal(Decimal::ONE))
        .unwrap();
    assert!(final_balances(&g, None, None).is_err());
    assert_eq!(
        final_balances(
            &g,
            Some(&money("0", "USD", 2)),
            Some(&money("-1", "EUR", 2)),
        )
        .unwrap()
        .1
        .unwrap(),
        max
    );
    g.functional_currency = None;
    g.account_class = AccountClass::ReusableCredit;
    g.table_rank = GrainTable::ReusableCredit;
    g.delta = ExactAmount::from_decimal(Decimal::NEGATIVE_ONE);
    assert!(matches!(
        final_balances(&g, None, None),
        Err(ProjectError::NegativeBalance { .. })
    ));
    g.table_rank = GrainTable::Account;
    assert!(final_balances(&g, None, None).is_ok());
}

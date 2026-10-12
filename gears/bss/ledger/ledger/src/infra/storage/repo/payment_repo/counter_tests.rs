//! Permanent real-migration SQLite coverage for exact payment counters and their CAS.
use super::*;
use crate::domain::error::DomainError;
use crate::infra::posting::retry::{AttemptError, retry_transaction};
use bss_ledger_sdk::MoneyError;
use bss_ledger_sdk::{CurrencySpec, parse_decimal};
use sea_orm_migration::MigratorTrait;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use toolkit_db::secure::{Db, TxConfig};
use toolkit_db::{ConnectOpts, connect_db};

fn money(text: &str) -> PostedMoney {
    currency_money(text, "EUR", 2)
}
fn currency_money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
async fn setup() -> (PaymentRepo, Db, Uuid) {
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
    )
}

#[tokio::test]
async fn settlement_decimal_caps_signed_releases_and_return_order() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let scope = AccessScope::allow_all();
            r.seed_settlement(txn, &scope, tenant, "p", &money("1"), &money("0.1"))
                .await?;
            // Gross settled is the cap: allocating 0.95 is valid despite the 0.10 fee.
            r.add_allocated(txn, &scope, tenant, "p", &money("0.95"))
                .await?;
            r.add_refunded_unallocated(txn, &scope, tenant, "p", &money("0.05"))
                .await?;
            assert!(matches!(
                r.add_refunded_unallocated(txn, &scope, tenant, "p", &money("0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            r.add_refunded_unallocated(txn, &scope, tenant, "p", &money("-0.05"))
                .await?;
            assert!(matches!(
                r.add_allocated(txn, &scope, tenant, "p", &money("0.09"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            r.add_refunded(txn, &scope, tenant, "p", &money("0.7"))
                .await?;
            r.add_clawed_back(txn, &scope, tenant, "p", &money("0.3"))
                .await?;
            assert!(matches!(
                r.add_refunded(txn, &scope, tenant, "p", &money("0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            assert!(matches!(
                r.add_clawed_back(txn, &scope, tenant, "p", &money("0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            r.add_refunded(txn, &scope, tenant, "p", &money("-0.7"))
                .await?;
            r.add_clawed_back(txn, &scope, tenant, "p", &money("-0.3"))
                .await?;
            r.add_allocated(txn, &scope, tenant, "p", &money("-0.95"))
                .await?;
            assert!(matches!(
                r.add_settled(txn, &scope, tenant, "p", &money("-0.95"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            // Preserve the fee-before-settled sequencing used by settlement returns.
            r.add_fee(txn, &scope, tenant, "p", &money("-0.09")).await?;
            r.add_settled(txn, &scope, tenant, "p", &money("-0.95"))
                .await?;
            let before = r
                .read_settlement_in(txn, &scope, tenant, "p")
                .await?
                .unwrap();
            assert_eq!(before.settled, money("0.05"));
            assert_eq!(before.fee, money("0.01"));
            for counter in [
                SettlementCounter::Settled,
                SettlementCounter::Fee,
                SettlementCounter::Allocated,
                SettlementCounter::Refunded,
                SettlementCounter::RefundedUnallocated,
                SettlementCounter::ClawedBack,
            ] {
                assert!(matches!(
                    r.add_settlement_counter(txn, &scope, tenant, "p", counter, &money("-1"))
                        .await,
                    Err(RepoError::MoneyOutCapExceeded(_))
                ));
            }
            assert!(matches!(
                r.add_fee(txn, &scope, tenant, "p", &money("0.05")).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            assert_eq!(
                r.read_settlement_in(txn, &scope, tenant, "p")
                    .await?
                    .unwrap(),
                before
            );
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let row = payment_settlement::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.settled, "0.05");
    assert_eq!(row.fee, "0.01");
    assert_eq!(row.allocated, "0");
    assert_eq!(
        repo.read_settlement(&AccessScope::allow_all(), tenant, "missing")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn all_correlated_caps_are_checked_when_settled_decreases() {
    let (repo, db, tenant) = setup().await;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let scope = AccessScope::allow_all();
            for (id, counter) in [
                ("fee", SettlementCounter::Fee),
                ("allocated", SettlementCounter::Allocated),
                ("refunded", SettlementCounter::Refunded),
                ("unallocated", SettlementCounter::RefundedUnallocated),
                ("claw", SettlementCounter::ClawedBack),
            ] {
                repo.seed_settlement(txn, &scope, tenant, id, &money("1"), &money("0"))
                    .await?;
                repo.add_settlement_counter(txn, &scope, tenant, id, counter, &money("1"))
                    .await?;
                assert!(matches!(
                    repo.add_settled(txn, &scope, tenant, id, &money("-0.01"))
                        .await,
                    Err(RepoError::MoneyOutCapExceeded(_))
                ));
                assert_eq!(
                    repo.read_settlement_in(txn, &scope, tenant, id)
                        .await?
                        .unwrap()
                        .version,
                    1
                );
            }
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn allocation_refund_create_add_release_caps_and_metadata() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let s = AccessScope::allow_all();
            assert!(
                r.read_allocation_refund_for_update(txn, &s, tenant, "p", "i")
                    .await?
                    .is_none()
            );
            assert!(matches!(
                r.add_allocation_refund_refunded(txn, &s, tenant, "p", "i", &money("0.01"))
                    .await,
                Err(RepoError::Db(_))
            ));
            assert!(matches!(
                r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("-0.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("0.01"))
                .await?;
            r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("0.09"))
                .await?;
            r.add_allocation_refund_refunded(txn, &s, tenant, "p", "i", &money("0.1"))
                .await?;
            for (counter, delta) in [
                (AllocationRefundCounter::Allocated, "-0.01"),
                (AllocationRefundCounter::Refunded, "0.01"),
                (AllocationRefundCounter::Refunded, "-0.11"),
            ] {
                let state = r
                    .read_allocation_refund_in(txn, &s, tenant, "p", "i")
                    .await?
                    .unwrap();
                assert!(matches!(
                    r.update_allocation_refund(txn, &s, &state, counter, &money(delta))
                        .await,
                    Err(RepoError::MoneyOutCapExceeded(_))
                ));
            }
            for delta in [currency_money("0", "EUR", 3), currency_money("0", "USD", 2)] {
                assert!(matches!(
                    r.bump_allocation_refund(txn, &s, tenant, "p", "i", &delta)
                        .await,
                    Err(RepoError::Money(_))
                ));
            }
            r.add_allocation_refund_refunded(txn, &s, tenant, "p", "i", &money("-0.09"))
                .await?;
            r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("-0.09"))
                .await?;
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let state = repo
        .read_allocation_refund(&AccessScope::allow_all(), tenant, "p", "i")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.allocated, money("0.01"));
    assert_eq!(state.refunded, money("0.01"));
    assert_eq!(state.version, 4);
}

#[tokio::test]
async fn scale_extremes_range_overflow_and_mismatch_preserve_rows() {
    let (repo, db, tenant) = setup().await;
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let s = AccessScope::allow_all();
            for (id, value, scale) in [
                ("zero", "1", 0),
                ("tiny", "0.0000000000000000000000000001", 28),
                ("max", "9999999999999999999999999999", 2),
            ] {
                let m = currency_money(value, "EUR", scale);
                repo.seed_settlement(txn, &s, tenant, id, &m, &currency_money("0", "EUR", scale))
                    .await?;
                assert_eq!(
                    repo.read_settlement_in(txn, &s, tenant, id)
                        .await?
                        .unwrap()
                        .settled,
                    m
                );
            }
            assert!(matches!(
                repo.add_settled(txn, &s, tenant, "max", &money("0.01"))
                    .await,
                Err(RepoError::Money(MoneyError::AmountOutOfRange))
            ));
            repo.bump_allocation_refund(
                txn,
                &s,
                tenant,
                "max",
                "i",
                &money("9999999999999999999999999999"),
            )
            .await?;
            assert!(matches!(
                repo.bump_allocation_refund(txn, &s, tenant, "max", "i", &money("0.01"))
                    .await,
                Err(RepoError::Money(MoneyError::AmountOutOfRange))
            ));
            assert!(matches!(
                repo.add_settled(txn, &s, tenant, "max", &currency_money("0", "EUR", 3))
                    .await,
                Err(RepoError::Money(MoneyError::ScaleMismatch))
            ));
            assert!(matches!(
                repo.add_settled(txn, &s, tenant, "max", &currency_money("0", "USD", 2))
                    .await,
                Err(RepoError::Money(MoneyError::CurrencyMismatch))
            ));
            assert!(matches!(
                repo.seed_settlement(txn, &s, tenant, "bad", &money("1"), &money("1.01"))
                    .await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            assert!(matches!(
                repo.seed_settlement(
                    txn,
                    &s,
                    tenant,
                    "bad",
                    &money("1"),
                    &currency_money("0", "EUR", 3)
                )
                .await,
                Err(RepoError::Money(MoneyError::ScaleMismatch))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stale_settlement_cas_rolls_back_all_preceding_tables_and_rebuilds_three_times() {
    let (repo, db, tenant) = setup().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = attempts.clone();
    let r = repo.clone();
    let result = retry_transaction(&db, move |txn| {
        let r = r.clone();
        let seen = seen.clone();
        Box::pin(async move {
            seen.fetch_add(1, Ordering::SeqCst);
            let s = AccessScope::allow_all();
            assert!(r.read_settlement_in(txn, &s, tenant, "p").await?.is_none());
            r.seed_settlement(txn, &s, tenant, "p", &money("1"), &money("0"))
                .await?;
            let observed = r.read_settlement_in(txn, &s, tenant, "p").await?.unwrap();
            r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("0.1"))
                .await?;
            r.add_allocated(txn, &s, tenant, "p", &money("0.1")).await?;
            r.write_settlement_counter(
                txn,
                &s,
                &observed,
                SettlementCounter::Allocated,
                &money("0.2"),
            )
            .await?;
            Ok(())
        })
    })
    .await;
    assert!(matches!(
        result,
        Err(DomainError::ConcurrentModification(_))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert!(
        repo.read_settlement(&AccessScope::allow_all(), tenant, "p")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.read_allocation_refund(&AccessScope::allow_all(), tenant, "p", "i")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn stale_allocation_cas_and_intended_insert_race_are_typed_conflicts() {
    let (repo, db, tenant) = setup().await;
    for insert_race in [false, true] {
        let r = repo.clone();
        let result = db
            .transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
                Box::pin(async move {
                    let s = AccessScope::allow_all();
                    r.seed_settlement(txn, &s, tenant, "p", &money("1"), &money("0"))
                        .await?;
                    // A real duplicate-key error exercises the missing-grain insertion branch.
                    r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("0.5"))
                        .await?;
                    if insert_race {
                        r.insert_allocation_refund(txn, &s, tenant, "p", "i", &money("0.1"))
                            .await?;
                    } else {
                        let observed = r
                            .read_allocation_refund_in(txn, &s, tenant, "p", "i")
                            .await?
                            .unwrap();
                        r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("0.1"))
                            .await?;
                        r.update_allocation_refund(
                            txn,
                            &s,
                            &observed,
                            AllocationRefundCounter::Refunded,
                            &money("0.1"),
                        )
                        .await?;
                    }
                    Ok::<_, AttemptError>(())
                })
            })
            .await;
        assert!(matches!(result, Err(AttemptError::Conflict)));
        assert!(
            repo.read_settlement(&AccessScope::allow_all(), tenant, "p")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            repo.read_allocation_refund(&AccessScope::allow_all(), tenant, "p", "i")
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn allocation_rows_are_positive_append_only_and_tenant_scoped() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    let time = OffsetDateTime::now_utc();
    let allocation = Uuid::now_v7();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let s = AccessScope::for_tenant(tenant);
            let mut row = NewAllocationRow {
                tenant_id: tenant,
                allocation_id: allocation,
                payer_tenant_id: tenant,
                payment_id: "p".into(),
                invoice_id: "b".into(),
                amount: money("0.09"),
                precedence_policy_ref: "oldest-first@1".into(),
                allocated_at_utc: time,
            };
            r.insert_allocation_rows(txn, &s, &[row.clone()]).await?;
            row.invoice_id = "a".into();
            row.amount = money("0.01");
            r.insert_allocation_rows(txn, &s, &[row.clone()]).await?;
            row.amount = money("0");
            assert!(matches!(
                r.insert_allocation_rows(txn, &s, &[row.clone()]).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            row.amount = money("-0.01");
            assert!(matches!(
                r.insert_allocation_rows(txn, &s, &[row]).await,
                Err(RepoError::MoneyOutCapExceeded(_))
            ));
            r.seed_settlement(txn, &s, tenant, "p", &money("1"), &money("0"))
                .await?;
            r.bump_allocation_refund(txn, &s, tenant, "p", "a", &money("0.01"))
                .await?;
            let foreign = AccessScope::for_tenant(Uuid::now_v7());
            assert!(
                r.read_settlement_for_update(txn, &foreign, tenant, "p")
                    .await?
                    .is_none()
            );
            assert!(
                r.read_allocation_refund_in(txn, &foreign, tenant, "p", "a")
                    .await?
                    .is_none()
            );
            assert!(
                r.list_payment_allocations_in(txn, &foreign, tenant, "p")
                    .await?
                    .is_empty()
            );
            assert!(matches!(
                r.add_allocated(txn, &foreign, tenant, "p", &money("0.01"))
                    .await,
                Err(RepoError::Db(_))
            ));
            assert!(matches!(
                r.seed_settlement(txn, &foreign, tenant, "other", &money("1"), &money("0"))
                    .await,
                Err(RepoError::Db(_))
            ));
            assert!(matches!(
                r.bump_allocation_refund(txn, &foreign, tenant, "p", "a", &money("0.01"))
                    .await,
                Err(RepoError::Db(_))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    let rows = repo
        .list_payment_allocations(&AccessScope::for_tenant(tenant), tenant, "p")
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.invoice_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(rows[0].amount, money("0.01"));
    assert_eq!(rows[1].amount, money("0.09"));
    assert_eq!(rows[0].allocation_id, allocation);
    assert_eq!(rows[0].precedence_policy_ref, "oldest-first@1");
    assert_eq!(rows[0].allocated_at_utc, time);
    // Duplicate allocation is an infrastructure error, never the rebuildable insert adapter.
    let row = NewAllocationRow {
        tenant_id: tenant,
        allocation_id: allocation,
        payer_tenant_id: tenant,
        payment_id: "p".into(),
        invoice_id: "a".into(),
        amount: money("0.1"),
        precedence_policy_ref: "changed".into(),
        allocated_at_utc: time,
    };
    let r = repo.clone();
    let result = db
        .transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
            Box::pin(async move {
                r.insert_allocation_rows(txn, &AccessScope::for_tenant(tenant), &[row])
                    .await
                    .map_err(AttemptError::from)
            })
        })
        .await;
    assert!(matches!(
        result,
        Err(AttemptError::Business(DomainError::Internal(_)))
    ));
    assert_eq!(
        repo.list_payment_allocations(&AccessScope::for_tenant(tenant), tenant, "p")
            .await
            .unwrap(),
        rows
    );
}

#[tokio::test]
async fn corrupted_stored_caps_and_version_overflow_fail_closed() {
    let (repo, db, tenant) = setup().await;
    let r = repo.clone();
    db.transaction_ref_mapped_with_config(TxConfig::serializable(), move |txn| {
        Box::pin(async move {
            let s = AccessScope::allow_all();
            r.seed_settlement(txn, &s, tenant, "p", &money("1"), &money("0"))
                .await?;
            // SQLite deliberately has no cross-column numeric TEXT checks. Corrupt via
            // secure fixture update, then verify a read refuses the entire stored state.
            payment_settlement::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(payment_settlement::Column::Fee, Expr::value("2"))
                .filter(settlement_key(tenant, "p"))
                .exec(txn)
                .await
                .unwrap();
            assert!(matches!(
                r.read_settlement_in(txn, &s, tenant, "p").await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            assert!(matches!(
                r.add_settled(txn, &s, tenant, "p", &money("1")).await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            payment_settlement::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(payment_settlement::Column::Fee, Expr::value("0"))
                .col_expr(payment_settlement::Column::Version, Expr::value(i64::MAX))
                .filter(settlement_key(tenant, "p"))
                .exec(txn)
                .await
                .unwrap();
            assert!(matches!(
                r.add_settled(txn, &s, tenant, "p", &money("0.01")).await,
                Err(RepoError::Db(_))
            ));
            r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("1"))
                .await?;
            payment_allocation_refund::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(
                    payment_allocation_refund::Column::Refunded,
                    Expr::value("2"),
                )
                .filter(allocation_refund_key(tenant, "p", "i"))
                .exec(txn)
                .await
                .unwrap();
            assert!(matches!(
                r.read_allocation_refund_in(txn, &s, tenant, "p", "i").await,
                Err(RepoError::InvalidStoredMoney(_))
            ));
            payment_allocation_refund::Entity::update_many()
                .secure()
                .scope_with(&s)
                .col_expr(
                    payment_allocation_refund::Column::Refunded,
                    Expr::value("0"),
                )
                .col_expr(
                    payment_allocation_refund::Column::Version,
                    Expr::value(i64::MAX),
                )
                .filter(allocation_refund_key(tenant, "p", "i"))
                .exec(txn)
                .await
                .unwrap();
            assert!(matches!(
                r.bump_allocation_refund(txn, &s, tenant, "p", "i", &money("0.01"))
                    .await,
                Err(RepoError::Db(_))
            ));
            Ok::<_, AttemptError>(())
        })
    })
    .await
    .unwrap();
    assert_eq!(
        repo.read_settlement(&AccessScope::allow_all(), tenant, "p")
            .await
            .unwrap()
            .unwrap()
            .settled,
        money("1")
    );
    // Canonical/scale checks at the storage boundary reject text the fresh schema
    // itself prevents us from inserting. Exercise the exact same production decoder.
    let row = payment_settlement::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    for text in ["01", "1.00", "NaN", "database is locked", "0.001"] {
        let mut bad = row.clone();
        bad.settled = text.into();
        assert!(matches!(
            SettlementState::decode(bad),
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
    let mut bad = row.clone();
    bad.currency_scale = 29;
    assert!(matches!(
        SettlementState::decode(bad),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    let mut bad = row;
    bad.version = -1;
    assert!(matches!(
        SettlementState::decode(bad),
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[test]
fn cap_comparisons_do_not_narrow_large_intermediates() {
    let max = exact(&money("9999999999999999999999999999"));
    let zero = exact(&money("0"));
    // allocated + refunded_unallocated exceeds settled only past the bound.
    let mut counters = SettlementTotals {
        settled: max.clone(),
        fee: zero.clone(),
        allocated: max.clone(),
        refunded: zero.clone(),
        refunded_unallocated: max.clone(),
        clawed_back: zero.clone(),
    };
    assert!(matches!(
        validate_settlement(&counters),
        Err(RepoError::MoneyOutCapExceeded(_))
    ));
    // refunded + clawed_back exceeds settled only past the bound.
    counters.allocated = zero.clone();
    counters.refunded_unallocated = zero;
    counters.refunded = max.clone();
    counters.clawed_back = max;
    assert!(matches!(
        validate_settlement(&counters),
        Err(RepoError::MoneyOutCapExceeded(_))
    ));
    assert!(matches!(require_one(0), Err(RepoError::Conflict(_))));
    assert!(matches!(require_one(2), Err(RepoError::Conflict(_))));
}

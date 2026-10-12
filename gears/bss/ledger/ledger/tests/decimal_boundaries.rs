//! Full-path decimal money boundaries: wire DTO → validated money → the real
//! `PostingService` on a fresh SQLite database → stored journal and balance
//! cache read back as canonical decimal text.
//!
//! Other rows of the boundary table are proved where their owner lives:
//! allocation thirds and residual policies in `domain::allocate_tests`; FX
//! translation and target-scale rounding in `domain::fx` tests and
//! `postgres_*_fx`; cumulative notes in `postgres_credit_note` /
//! `postgres_debit_note`; stored reversal after a registry change in
//! `infra::posting::service::decimal_tests`; concurrent debit and overflow in
//! `postgres_payment_concurrency` and `decimal_tests::balance_range_failure_*`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bss_ledger::api::rest::money::MoneyDto;
use bss_ledger::domain::model::{
    AccountRow, CurrencyScaleRow, EntryKey, FiscalPeriodRow, NewEntry, NewLine,
};
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::posting::service::PostingService;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::{JournalRepo, ReferenceRepo};
use bss_ledger_sdk::{
    AccountClass, CurrencySpec, MappingStatus, MoneyError, ODataQuery, PostedMoney, Side,
    SourceDocType, canonical_decimal, parse_decimal,
};
use chrono::NaiveDate;
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A non-ISO currency provisioned at the maximum scale for the tiny-amount case.
const TINY: &str = "XTNY";

struct Fixture {
    provider: DBProvider<DbError>,
    service: PostingService,
    tenant: Uuid,
}

fn money(amount: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(amount).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

/// Fresh SQLite schema, one OPEN period, EUR (ISO, scale 2) and `XTNY` at scale
/// 28, and a CASH/REVENUE account pair per currency.
async fn fixture() -> Fixture {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap();
    let provider = DBProvider::new(db);
    let tenant = Uuid::now_v7();
    let reference = ReferenceRepo::new(provider.clone());
    let seed = reference.clone();
    provider
        .transaction(move |txn| {
            Box::pin(async move {
                seed.insert_fiscal_period_if_absent_txn(
                    txn,
                    FiscalPeriodRow {
                        tenant_id: tenant,
                        legal_entity_id: tenant,
                        period_id: "202610".to_owned(),
                        fiscal_tz: "UTC".to_owned(),
                        status: "OPEN".to_owned(),
                    },
                )
                .await
                .map_err(|e| DbError::Other(anyhow::anyhow!(e)))?;
                seed.insert_currency_scale_if_absent_txn(
                    txn,
                    CurrencyScaleRow {
                        tenant_id: tenant,
                        currency: TINY.to_owned(),
                        currency_scale: 28,
                        source: "test".to_owned(),
                    },
                )
                .await
                .map_err(|e| DbError::Other(anyhow::anyhow!(e)))?;
                Ok(())
            })
        })
        .await
        .unwrap();
    for currency in ["EUR", TINY] {
        for (class, side) in [
            (AccountClass::CashClearing, Side::Debit),
            (AccountClass::Revenue, Side::Credit),
        ] {
            reference
                .insert_account(AccountRow {
                    account_id: account_id(tenant, currency, class),
                    tenant_id: tenant,
                    legal_entity_id: tenant,
                    account_class: class.as_str().to_owned(),
                    currency: currency.to_owned(),
                    revenue_stream: None,
                    normal_side: side.as_str().to_owned(),
                    may_go_negative: false,
                    lifecycle_state: "OPEN".to_owned(),
                })
                .await
                .unwrap();
        }
    }
    let service = PostingService::new(provider.clone(), Arc::new(LedgerEventPublisher::noop()));
    Fixture {
        provider,
        service,
        tenant,
    }
}

/// Deterministic account id per (tenant, currency, class), so a test can find
/// the account it posted to without carrying the fixture's ids around.
fn account_id(tenant: Uuid, currency: &str, class: AccountClass) -> Uuid {
    Uuid::new_v5(&tenant, format!("{currency}/{}", class.as_str()).as_bytes())
}

/// A balanced DR CASH / CR REVENUE entry of `amount`.
fn entry(f: &Fixture, amount: &PostedMoney) -> (NewEntry, Vec<NewLine>) {
    let entry_id = Uuid::now_v7();
    let currency = amount.currency().code();
    let header = NewEntry {
        entry_id,
        tenant_id: f.tenant,
        legal_entity_id: f.tenant,
        period_id: "202610".to_owned(),
        entry_currency: currency.to_owned(),
        source_doc_type: SourceDocType::ManualAdjustment,
        source_business_id: entry_id.to_string(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: f.tenant,
        correlation_id: entry_id,
        rounding_evidence: serde_json::Value::Null,
        rate_snapshot_ref: None,
    };
    let line = |class: AccountClass, side: Side| NewLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: f.tenant,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: account_id(f.tenant, currency, class),
        account_class: class,
        gl_code: None,
        side,
        money: amount.clone(),
        invoice_id: None,
        due_date: None,
        revenue_stream: Some("test".to_owned()),
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
    };
    (
        header,
        vec![
            line(AccountClass::CashClearing, Side::Debit),
            line(AccountClass::Revenue, Side::Credit),
        ],
    )
}

/// Post through the real service; the error is returned, not unwrapped.
async fn post(
    f: &Fixture,
    amount: &PostedMoney,
) -> Result<EntryKey, bss_ledger::domain::error::DomainError> {
    let (header, lines) = entry(f, amount);
    let key = EntryKey {
        entry_id: header.entry_id,
        tenant_id: f.tenant,
        period_id: header.period_id.clone(),
    };
    f.service
        .post(
            &SecurityContext::anonymous(),
            &AccessScope::for_tenant(f.tenant),
            header,
            lines,
            None,
        )
        .await
        .map(|_| key)
}

/// Every stored balance row as `(currency, canonical balance, stored scale)`.
async fn balances(f: &Fixture) -> Vec<(String, String, i16)> {
    let mut rows: Vec<_> = JournalRepo::new(f.provider.clone())
        .list_balances(
            &AccessScope::for_tenant(f.tenant),
            f.tenant,
            &ODataQuery::default(),
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|b| (b.currency, b.balance, b.currency_scale))
        .collect();
    rows.sort();
    rows
}

#[tokio::test]
async fn eur_12_34_posts_and_reads_back_exactly() {
    let f = fixture().await;
    let wire: MoneyDto =
        serde_json::from_str(r#"{"amount":"12.340","currency":"EUR","currency_scale":2}"#).unwrap();
    let amount = PostedMoney::try_from(wire).unwrap();
    let key = post(&f, &amount).await.unwrap();

    let record = JournalRepo::new(f.provider.clone())
        .find_entry(&AccessScope::for_tenant(f.tenant), key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.lines.len(), 2);
    for line in &record.lines {
        assert_eq!(canonical_decimal(line.money.amount()), "12.34");
        assert_eq!(line.money.currency().code(), "EUR");
        assert_eq!(line.money.currency().scale(), 2);
    }
    assert_eq!(
        balances(&f).await,
        vec![
            ("EUR".to_owned(), "12.34".to_owned(), 2),
            ("EUR".to_owned(), "12.34".to_owned(), 2),
        ]
    );
    let echoed = MoneyDto::from(&record.lines[0].money);
    assert_eq!(
        serde_json::to_value(&echoed).unwrap(),
        serde_json::json!({"amount": "12.34", "currency": "EUR", "currency_scale": 2})
    );
}

#[tokio::test]
async fn sub_increment_posting_is_rejected_before_any_write() {
    let f = fixture().await;
    // On the wire 0.047 EUR at scale 2 never becomes money (the DTO tests own
    // that). The only way it reaches the service is relabelled at a finer
    // scale, which the currency registry refuses before anything is written.
    let finer = post(&f, &money("0.047", "EUR", 3)).await;
    assert!(
        matches!(
            finer,
            Err(bss_ledger::domain::error::DomainError::InconsistentScale(_))
        ),
        "a sub-increment EUR amount is refused, never rounded or stored: {finer:?}"
    );
    assert!(balances(&f).await.is_empty(), "nothing reached storage");
    assert_eq!(
        PostedMoney::try_new(
            parse_decimal("0.047").unwrap(),
            money("1", "EUR", 2).currency().clone()
        ),
        Err(MoneyError::InvalidPostingIncrement),
        "and at the registered scale it is not a posted amount at all"
    );
}

#[tokio::test]
async fn maximum_amount_posts_and_the_next_value_is_out_of_range() {
    let f = fixture().await;
    // The published bound is a magnitude below 10^28 with at most 28
    // significant digits after normalization: the largest EUR posting is the
    // 28-digit integer.
    let max = "9999999999999999999999999999";
    post(&f, &money(max, "EUR", 2)).await.unwrap();
    assert_eq!(
        balances(&f).await,
        vec![
            ("EUR".to_owned(), max.to_owned(), 2),
            ("EUR".to_owned(), max.to_owned(), 2),
        ]
    );
    assert_eq!(
        parse_decimal("10000000000000000000000000000"),
        Err(MoneyError::AmountOutOfRange),
        "10^28 is outside the published bound; the parser refuses it before any constructor"
    );
    assert_eq!(
        parse_decimal("999999999999999999999999999.99"),
        Err(MoneyError::AmountOutOfRange),
        "29 significant digits exceed the published 28-digit coefficient bound"
    );
    let over = post(&f, &money("0.01", "EUR", 2)).await;
    assert!(
        over.is_err(),
        "a balance that would exceed the bound is a named range error: {over:?}"
    );
    assert_eq!(
        balances(&f).await,
        vec![
            ("EUR".to_owned(), max.to_owned(), 2),
            ("EUR".to_owned(), max.to_owned(), 2),
        ],
        "the refused posting left no partial write"
    );
}

#[tokio::test]
async fn scale_28_tiny_amount_round_trips_exactly() {
    let f = fixture().await;
    let tiny = "0.0000000000000000000000000001";
    post(&f, &money(tiny, TINY, 28)).await.unwrap();
    assert_eq!(
        balances(&f).await,
        vec![
            (TINY.to_owned(), tiny.to_owned(), 28),
            (TINY.to_owned(), tiny.to_owned(), 28),
        ]
    );
}

#[tokio::test]
async fn same_currency_with_another_scale_is_a_mismatch_never_a_second_bucket() {
    let f = fixture().await;
    post(&f, &money("1", "EUR", 2)).await.unwrap();
    let mismatch = post(&f, &money("1", "EUR", 3)).await;
    assert!(
        mismatch.is_err(),
        "EUR at scale 3 contradicts the currency registry: {mismatch:?}"
    );
    assert_eq!(
        balances(&f).await,
        vec![
            ("EUR".to_owned(), "1".to_owned(), 2),
            ("EUR".to_owned(), "1".to_owned(), 2),
        ],
        "one bucket per currency, untouched by the refused posting"
    );
}

//! Exact journal and registry checks against a fresh SQLite database.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_ledger::domain::model::{CurrencyScaleRow, EntryKey, NewEntry, NewLine, RepoError};
use bss_ledger::infra::currency_scale::CurrencyScaleResolver;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::{JournalRepo, ReferenceRepo};
use bss_ledger_sdk::{
    AccountClass, CurrencySpec, MappingStatus, PostedMoney, Side, SourceDocType, parse_decimal,
};
use chrono::NaiveDate;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

async fn database() -> DBProvider<DbError> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap();
    DBProvider::new(db)
}

fn money(amount: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(amount).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

async fn post(
    provider: &DBProvider<DbError>,
    tenant: Uuid,
    posted: PostedMoney,
    functional: Option<PostedMoney>,
    doc: SourceDocType,
) -> EntryKey {
    let entry_id = Uuid::new_v4();
    let period_id = "202610".to_owned();
    let header = NewEntry {
        entry_id,
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: period_id.clone(),
        entry_currency: posted.currency().code().to_owned(),
        source_doc_type: doc,
        source_business_id: entry_id.to_string(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: time::OffsetDateTime::now_utc(),
        effective_at: NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: tenant,
        correlation_id: entry_id,
        rounding_evidence: serde_json::json!({}),
        rate_snapshot_ref: None,
    };
    let unallocated_side = if doc == SourceDocType::SettlementReturn {
        Side::Debit
    } else {
        Side::Credit
    };
    let line = |class, side| NewLine {
        line_id: Uuid::new_v4(),
        payer_tenant_id: tenant,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::new_v4(),
        account_class: class,
        gl_code: None,
        side,
        money: posted.clone(),
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
        mapping_status: MappingStatus::Resolved,
        functional_money: functional.clone(),
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
    let opposite = if unallocated_side == Side::Credit {
        Side::Debit
    } else {
        Side::Credit
    };
    let lines = vec![
        line(AccountClass::Unallocated, unallocated_side),
        line(AccountClass::CashClearing, opposite),
    ];
    let repo = JournalRepo::new(provider.clone());
    provider
        .db()
        .transaction_ref(move |txn| {
            Box::pin(async move {
                repo.insert_entry_with_lines(txn, header, lines)
                    .await
                    .map_err(|e| DbError::Other(anyhow::anyhow!(e)))
            })
        })
        .await
        .unwrap();
    EntryKey {
        tenant_id: tenant,
        period_id,
        entry_id,
    }
}

#[tokio::test]
async fn large_journal_round_trip_preserves_both_stored_scales() {
    let provider = database().await;
    let tenant = Uuid::new_v4();
    let amount = "99999999999999999999999999.99";
    let key = post(
        &provider,
        tenant,
        money(amount, "EUR", 2),
        Some(money("1.001", "USD", 3)),
        SourceDocType::PaymentSettle,
    )
    .await;
    let record = JournalRepo::new(provider)
        .find_entry(&AccessScope::for_tenant(tenant), key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.lines.len(), 2);
    for line in record.lines {
        assert_eq!(line.money.amount().to_string(), amount);
        assert_eq!(line.money.currency().scale(), 2);
        assert_eq!(line.functional_money.unwrap().currency().scale(), 3);
    }
}

#[tokio::test]
async fn registry_validates_boundaries_and_locks_both_historical_money_columns() {
    let provider = database().await;
    let tenant = Uuid::new_v4();
    let reference = ReferenceRepo::new(provider.clone());
    let row = |code: &str, scale| CurrencyScaleRow {
        tenant_id: tenant,
        currency: code.to_owned(),
        currency_scale: scale,
        source: "TEST".to_owned(),
    };
    reference
        .upsert_currency_scale(row("TOKEN", 28))
        .await
        .unwrap();
    reference
        .upsert_currency_scale(row("JPY", 0))
        .await
        .unwrap();
    assert!(matches!(
        reference.upsert_currency_scale(row("EUR", 29)).await,
        Err(RepoError::ScaleOutOfRange(_))
    ));
    assert!(matches!(
        reference.upsert_currency_scale(row("eur", 2)).await,
        Err(RepoError::Money(_))
    ));
    let resolver = CurrencyScaleResolver::new(reference.clone());
    let scope = AccessScope::for_tenant(tenant);
    assert_eq!(resolver.resolve(&scope, tenant, "TOKEN").await.unwrap(), 28);
    assert!(resolver.resolve(&scope, tenant, "UNKNOWN").await.is_err());
    post(
        &provider,
        tenant,
        money("1", "EUR", 2),
        Some(money("1.001", "USD", 3)),
        SourceDocType::PaymentSettle,
    )
    .await;
    for (code, matching, different) in [("EUR", 2, 3), ("USD", 3, 2)] {
        assert!(matches!(
            reference.upsert_currency_scale(row(code, different)).await,
            Err(RepoError::CurrencyScaleLocked(_))
        ));
        reference
            .upsert_currency_scale(row(code, matching))
            .await
            .unwrap();
        assert!(matches!(
            reference.upsert_currency_scale(row(code, different)).await,
            Err(RepoError::CurrencyScaleLocked(_))
        ));
    }
}

#[tokio::test]
async fn psp_sum_and_count_use_the_report_currency_population() {
    let provider = database().await;
    let tenant = Uuid::new_v4();
    post(
        &provider,
        tenant,
        money("5", "EUR", 2),
        None,
        SourceDocType::PaymentSettle,
    )
    .await;
    post(
        &provider,
        tenant,
        money("2", "EUR", 2),
        None,
        SourceDocType::SettlementReturn,
    )
    .await;
    post(
        &provider,
        tenant,
        money("10", "USD", 2),
        None,
        SourceDocType::PaymentSettle,
    )
    .await;
    let repo = JournalRepo::new(provider.clone());
    let scope = AccessScope::for_tenant(tenant);
    let conn = provider.conn().unwrap();
    let currency = CurrencySpec::try_new("EUR".to_owned(), 2).unwrap();
    let total = repo
        .sum_period_settled_net(&conn, &scope, tenant, "202610", &currency)
        .await
        .unwrap();
    assert_eq!(total.settled.amount().to_string(), "3");
    assert_eq!(total.settlement_count, 1);
    let wrong_scale = CurrencySpec::try_new("EUR".to_owned(), 3).unwrap();
    assert!(matches!(
        repo.sum_period_settled_net(&conn, &scope, tenant, "202610", &wrong_scale)
            .await,
        Err(RepoError::Money(bss_ledger_sdk::MoneyError::ScaleMismatch))
    ));
    let empty = repo
        .sum_period_settled_net(
            &conn,
            &AccessScope::for_tenant(Uuid::new_v4()),
            tenant,
            "202610",
            &currency,
        )
        .await
        .unwrap();
    assert_eq!(empty.settled, money("0", "EUR", 2));
    assert_eq!(empty.settlement_count, 0);
}

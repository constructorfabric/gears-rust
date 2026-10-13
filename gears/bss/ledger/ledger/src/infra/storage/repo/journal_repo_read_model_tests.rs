//! The journal repository's decoded read models: list and point reads return
//! stored money already restored with its own currency and scale (corruption is
//! refused here, never in the API layer), and the business-key entry lookup
//! runs on the caller's runner. SQLite-backed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney, SourceDocType};
use rust_decimal::Decimal;
use sea_orm::{EntityTrait, IntoActiveModel};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::{AccessScope, Db, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use super::{JournalRepo, decode_ar_invoice_view, decode_balance_view, line_to_record};
use crate::domain::model::RepoError;
use crate::infra::storage::entity::{
    account_balance, ar_invoice_balance, journal_entry, journal_line,
};

async fn setup() -> (JournalRepo, Db) {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    (JournalRepo::new(DBProvider::new(db.clone())), db)
}

/// Insert one row through the secure scope, as every repository does.
macro_rules! insert {
    ($db:expr, $entity:path, $model:expr) => {{
        let am = $model.into_active_model();
        <$entity>::insert(am.clone())
            .secure()
            .scope_with_model(&AccessScope::allow_all(), &am)
            .unwrap()
            .exec(&$db.conn().unwrap())
            .await
            .unwrap();
    }};
}

fn money(amount: i64, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(amount, u32::from(scale)),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn balance_row(tenant: Uuid, account: Uuid, balance: &str) -> account_balance::Model {
    account_balance::Model {
        tenant_id: tenant,
        account_id: account,
        currency: "USD".to_owned(),
        currency_scale: 2,
        account_class: "REVENUE".to_owned(),
        normal_side: "CR".to_owned(),
        balance: balance.to_owned(),
        functional_balance: None,
        functional_currency: None,
        functional_currency_scale: None,
        last_entry_seq: None,
        version: 1,
    }
}

fn invoice_row(tenant: Uuid, balance: &str, disputed: &str) -> ar_invoice_balance::Model {
    ar_invoice_balance::Model {
        tenant_id: tenant,
        payer_tenant_id: tenant,
        account_id: Uuid::now_v7(),
        invoice_id: "inv-1".to_owned(),
        currency: "JPY".to_owned(),
        currency_scale: 0,
        balance: balance.to_owned(),
        disputed: disputed.to_owned(),
        functional_balance: None,
        functional_currency: None,
        functional_currency_scale: None,
        original_posted_at: None,
        due_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 1),
        last_entry_seq: None,
        version: 0,
    }
}

fn line_row(tenant: Uuid, entry_id: Uuid) -> journal_line::Model {
    journal_line::Model {
        line_id: Uuid::now_v7(),
        entry_id,
        tenant_id: tenant,
        period_id: "202606".to_owned(),
        payer_tenant_id: tenant,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::now_v7(),
        account_class: "AR".to_owned(),
        gl_code: None,
        side: "DR".to_owned(),
        amount: "10.5".to_owned(),
        currency: "EUR".to_owned(),
        currency_scale: 2,
        invoice_id: Some("inv-1".to_owned()),
        due_date: None,
        revenue_stream: None,
        mapping_status: "RESOLVED".to_owned(),
        functional_amount: None,
        functional_currency: None,
        functional_currency_scale: None,
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
        rate_snapshot_ref: None,
    }
}

fn entry_row(tenant: Uuid, entry_id: Uuid, doc: &str, business: &str) -> journal_entry::Model {
    journal_entry::Model {
        entry_id,
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: "202606".to_owned(),
        entry_currency: "EUR".to_owned(),
        source_doc_type: doc.to_owned(),
        source_business_id: business.to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: time::OffsetDateTime::now_utc(),
        effective_at: chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: tenant,
        correlation_id: tenant,
        rounding_evidence: serde_json::Value::Null,
        created_seq: 1,
        row_hash: None,
        prev_hash: None,
        prev_entry_id: None,
        prev_period_id: None,
    }
}

#[test]
fn a_balance_row_decodes_with_its_stored_currency_and_scale() {
    let tenant = Uuid::now_v7();
    let account = Uuid::now_v7();
    let decoded = decode_balance_view(balance_row(tenant, account, "10.5")).unwrap();
    assert_eq!(decoded.account_id, account);
    assert_eq!(decoded.account_class, AccountClass::Revenue);
    assert_eq!(decoded.balance, money(1050, "USD", 2));
    assert!(decoded.functional_balance.is_none());
}

/// The class is parsed where the row is decoded: an unknown stored literal is
/// stored corruption from the repository, not a later API-layer parse failure.
#[test]
fn an_unknown_stored_account_class_is_refused_in_the_repository() {
    let mut row = balance_row(Uuid::now_v7(), Uuid::now_v7(), "1");
    row.account_class = "WAT".to_owned();
    assert!(matches!(
        decode_balance_view(row),
        Err(RepoError::InvalidStoredMoney(detail)) if detail.contains("WAT")
    ));
}

#[test]
fn corrupt_stored_balances_are_refused_in_the_repository() {
    let tenant = Uuid::now_v7();
    // Noncanonical text is corruption, never normalized.
    assert!(matches!(
        decode_balance_view(balance_row(tenant, Uuid::now_v7(), "10.000")),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    let mut half = balance_row(tenant, Uuid::now_v7(), "1");
    half.functional_balance = Some("1".to_owned());
    assert!(matches!(
        decode_balance_view(half),
        Err(RepoError::InvalidStoredMoney(_))
    ));
    let mut line = line_row(tenant, Uuid::now_v7());
    line.functional_amount = Some("55".to_owned());
    assert!(matches!(
        line_to_record(line),
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[test]
fn an_invoice_row_decodes_and_refuses_a_dispute_above_its_balance() {
    let tenant = Uuid::now_v7();
    let decoded = decode_ar_invoice_view(invoice_row(tenant, "1200", "0")).unwrap();
    assert_eq!(decoded.payer_tenant_id, tenant);
    assert_eq!(decoded.invoice_id, "inv-1");
    assert_eq!(decoded.balance, money(1200, "JPY", 0));
    assert_eq!(
        decoded.due_date,
        chrono::NaiveDate::from_ymd_opt(2026, 7, 1)
    );
    assert!(matches!(
        decode_ar_invoice_view(invoice_row(tenant, "10", "11")),
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[tokio::test]
async fn the_account_balance_point_read_returns_decoded_money_under_scope() {
    let (repo, db) = setup().await;
    let tenant = Uuid::now_v7();
    let account = Uuid::now_v7();
    insert!(
        db,
        account_balance::Entity,
        balance_row(tenant, account, "7.25")
    );
    let scope = AccessScope::for_tenant(tenant);
    assert_eq!(
        repo.read_account_balance(&scope, tenant, account)
            .await
            .unwrap(),
        Some(money(725, "USD", 2))
    );
    assert_eq!(
        repo.read_account_balance(&scope, tenant, Uuid::now_v7())
            .await
            .unwrap(),
        None,
        "an account with no balance row reads None"
    );
    let other = Uuid::now_v7();
    assert_eq!(
        repo.read_account_balance(&AccessScope::for_tenant(other), tenant, account)
            .await
            .unwrap(),
        None,
        "a foreign scope sees nothing"
    );
    let corrupt = Uuid::now_v7();
    insert!(
        db,
        account_balance::Entity,
        balance_row(tenant, corrupt, "1.0")
    );
    assert!(matches!(
        repo.read_account_balance(&scope, tenant, corrupt).await,
        Err(RepoError::InvalidStoredMoney(_))
    ));
}

#[tokio::test]
async fn the_list_reads_return_decoded_records() {
    let (repo, db) = setup().await;
    let tenant = Uuid::now_v7();
    let entry_id = Uuid::now_v7();
    insert!(
        db,
        journal_entry::Entity,
        entry_row(tenant, entry_id, "INVOICE_POST", "inv-1")
    );
    insert!(db, journal_line::Entity, line_row(tenant, entry_id));
    insert!(
        db,
        account_balance::Entity,
        balance_row(tenant, Uuid::now_v7(), "3")
    );
    insert!(
        db,
        ar_invoice_balance::Entity,
        invoice_row(tenant, "1200", "200")
    );
    let scope = AccessScope::for_tenant(tenant);

    let lines = repo
        .list_lines(&scope, tenant, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(lines.items.len(), 1);
    assert_eq!(lines.items[0].money, money(1050, "EUR", 2));
    assert_eq!(lines.items[0].entry_id, entry_id);

    let balances = repo
        .list_balance_records(&scope, tenant, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(balances.items.len(), 1);
    assert_eq!(balances.items[0].balance, money(300, "USD", 2));

    let invoices = repo
        .list_ar_invoice_records(&scope, tenant, None)
        .await
        .unwrap();
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0].balance, money(1200, "JPY", 0));
}

#[tokio::test]
async fn the_business_key_lookup_matches_tenant_doc_type_and_business_id() {
    let (repo, db) = setup().await;
    let tenant = Uuid::now_v7();
    let entry_id = Uuid::now_v7();
    insert!(
        db,
        journal_entry::Entity,
        entry_row(tenant, entry_id, "INVOICE_POST", "inv-1")
    );
    let scope = AccessScope::for_tenant(tenant);
    let conn = db.conn().unwrap();
    assert_eq!(
        repo.find_entry_id_by_business_key_in(
            &conn,
            &scope,
            tenant,
            SourceDocType::InvoicePost,
            "inv-1"
        )
        .await
        .unwrap(),
        Some(entry_id)
    );
    for (doc, business) in [
        (SourceDocType::CreditNote, "inv-1"),
        (SourceDocType::InvoicePost, "inv-2"),
    ] {
        assert_eq!(
            repo.find_entry_id_by_business_key_in(&conn, &scope, tenant, doc, business)
                .await
                .unwrap(),
            None,
            "{}/{business} names no entry",
            doc.as_str()
        );
    }
    let other = Uuid::now_v7();
    assert_eq!(
        repo.find_entry_id_by_business_key_in(
            &conn,
            &AccessScope::for_tenant(other),
            tenant,
            SourceDocType::InvoicePost,
            "inv-1"
        )
        .await
        .unwrap(),
        None,
        "a foreign scope sees nothing"
    );
}

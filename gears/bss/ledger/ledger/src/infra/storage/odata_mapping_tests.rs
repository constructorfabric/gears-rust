//! Unit tests for the OData field→column mappers in `odata_mapping.rs`.
//!
//! `Column` enums emitted by `DeriveEntityModel` implement `IdenStatic` but
//! not `PartialEq`, so we compare via `IdenStatic::as_str()` (the DB column
//! name). This is a stronger check: it validates the actual SQL identifier, not
//! just the enum discriminant.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::inconsistent_struct_constructor
)]

use sea_orm::IdenStatic;
use uuid::Uuid;

use crate::infra::storage::entity::{account_balance, journal_line, tenant_account};
use crate::odata::{AccountInfoFilterField, BalanceFilterField, JournalLineFilterField};

use super::{
    AccountInfoODataMapper, BalanceColumn, BalanceODataMapper, JournalLineColumn,
    JournalLineODataMapper, TenantAccountColumn,
};
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sample_tenant_account() -> tenant_account::Model {
    tenant_account::Model {
        account_id: Uuid::from_u128(0x0A),
        tenant_id: Uuid::from_u128(0x1A),
        legal_entity_id: Uuid::from_u128(0x2A),
        account_class: "AR".to_owned(),
        currency: "USD".to_owned(),
        revenue_stream: Some("SAAS".to_owned()),
        normal_side: "DR".to_owned(),
        may_go_negative: false,
        lifecycle_state: "active".to_owned(),
    }
}

fn sample_journal_line() -> journal_line::Model {
    journal_line::Model {
        line_id: Uuid::from_u128(0x0B),
        entry_id: Uuid::from_u128(0x1B),
        tenant_id: Uuid::from_u128(0x2B),
        period_id: "2025-01".to_owned(),
        payer_tenant_id: Uuid::from_u128(0x3B),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::from_u128(0x4B),
        account_class: "AR".to_owned(),
        gl_code: None,
        side: "DR".to_owned(),
        amount_minor: 5000,
        currency: "EUR".to_owned(),
        currency_scale: 2,
        invoice_id: Some("INV-001".to_owned()),
        due_date: None,
        revenue_stream: None,
        mapping_status: "RESOLVED".to_owned(),
        functional_amount_minor: None,
        functional_currency: None,
        rate_snapshot_ref: None,
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

fn sample_account_balance() -> account_balance::Model {
    account_balance::Model {
        tenant_id: Uuid::from_u128(0xA1),
        account_id: Uuid::from_u128(0xB1),
        currency: "GBP".to_owned(),
        account_class: "REVENUE".to_owned(),
        normal_side: "CR".to_owned(),
        balance_minor: 9000,
        functional_balance_minor: None,
        functional_currency: None,
        last_entry_seq: Some(42),
        version: 3,
    }
}

// ---------------------------------------------------------------------------
// AccountInfoODataMapper — map_field (column name lock)
// ---------------------------------------------------------------------------

#[test]
fn account_info_map_account_id() {
    assert_eq!(
        AccountInfoODataMapper::map_field(AccountInfoFilterField::AccountId).as_str(),
        TenantAccountColumn::AccountId.as_str(),
    );
}

#[test]
fn account_info_map_account_class() {
    assert_eq!(
        AccountInfoODataMapper::map_field(AccountInfoFilterField::AccountClass).as_str(),
        TenantAccountColumn::AccountClass.as_str(),
    );
}

#[test]
fn account_info_map_currency() {
    assert_eq!(
        AccountInfoODataMapper::map_field(AccountInfoFilterField::Currency).as_str(),
        TenantAccountColumn::Currency.as_str(),
    );
}

#[test]
fn account_info_map_revenue_stream() {
    assert_eq!(
        AccountInfoODataMapper::map_field(AccountInfoFilterField::RevenueStream).as_str(),
        TenantAccountColumn::RevenueStream.as_str(),
    );
}

#[test]
fn account_info_map_lifecycle_state() {
    assert_eq!(
        AccountInfoODataMapper::map_field(AccountInfoFilterField::LifecycleState).as_str(),
        TenantAccountColumn::LifecycleState.as_str(),
    );
}

// ---------------------------------------------------------------------------
// AccountInfoODataMapper — extract_cursor_value
// ---------------------------------------------------------------------------

#[test]
fn account_info_cursor_account_id_is_uuid() {
    let model = sample_tenant_account();
    let val =
        AccountInfoODataMapper::extract_cursor_value(&model, AccountInfoFilterField::AccountId);
    assert_eq!(val, sea_orm::Value::Uuid(Some(Box::new(model.account_id))));
}

#[test]
fn account_info_cursor_account_class_is_string() {
    let model = sample_tenant_account();
    let val =
        AccountInfoODataMapper::extract_cursor_value(&model, AccountInfoFilterField::AccountClass);
    assert_eq!(val, sea_orm::Value::String(Some(Box::new("AR".to_owned()))));
}

#[test]
fn account_info_cursor_currency_is_string() {
    let model = sample_tenant_account();
    let val =
        AccountInfoODataMapper::extract_cursor_value(&model, AccountInfoFilterField::Currency);
    assert_eq!(
        val,
        sea_orm::Value::String(Some(Box::new("USD".to_owned()))),
    );
}

#[test]
fn account_info_cursor_revenue_stream_some() {
    let model = sample_tenant_account();
    let val =
        AccountInfoODataMapper::extract_cursor_value(&model, AccountInfoFilterField::RevenueStream);
    assert_eq!(
        val,
        sea_orm::Value::String(Some(Box::new("SAAS".to_owned()))),
    );
}

#[test]
fn account_info_cursor_revenue_stream_none_is_null_string() {
    let mut model = sample_tenant_account();
    model.revenue_stream = None;
    let val =
        AccountInfoODataMapper::extract_cursor_value(&model, AccountInfoFilterField::RevenueStream);
    assert_eq!(val, sea_orm::Value::String(None));
}

#[test]
fn account_info_cursor_lifecycle_state_is_string() {
    let model = sample_tenant_account();
    let val = AccountInfoODataMapper::extract_cursor_value(
        &model,
        AccountInfoFilterField::LifecycleState,
    );
    assert_eq!(
        val,
        sea_orm::Value::String(Some(Box::new("active".to_owned()))),
    );
}

// ---------------------------------------------------------------------------
// JournalLineODataMapper — map_field (column name lock)
// ---------------------------------------------------------------------------

#[test]
fn journal_line_map_line_id() {
    assert_eq!(
        JournalLineODataMapper::map_field(JournalLineFilterField::LineId).as_str(),
        JournalLineColumn::LineId.as_str(),
    );
}

#[test]
fn journal_line_map_payer_tenant_id() {
    assert_eq!(
        JournalLineODataMapper::map_field(JournalLineFilterField::PayerTenantId).as_str(),
        JournalLineColumn::PayerTenantId.as_str(),
    );
}

#[test]
fn journal_line_map_account_class() {
    assert_eq!(
        JournalLineODataMapper::map_field(JournalLineFilterField::AccountClass).as_str(),
        JournalLineColumn::AccountClass.as_str(),
    );
}

#[test]
fn journal_line_map_period_id() {
    assert_eq!(
        JournalLineODataMapper::map_field(JournalLineFilterField::PeriodId).as_str(),
        JournalLineColumn::PeriodId.as_str(),
    );
}

#[test]
fn journal_line_map_invoice_id() {
    assert_eq!(
        JournalLineODataMapper::map_field(JournalLineFilterField::InvoiceId).as_str(),
        JournalLineColumn::InvoiceId.as_str(),
    );
}

// ---------------------------------------------------------------------------
// JournalLineODataMapper — extract_cursor_value
// ---------------------------------------------------------------------------

#[test]
fn journal_line_cursor_line_id_is_uuid() {
    let model = sample_journal_line();
    let val = JournalLineODataMapper::extract_cursor_value(&model, JournalLineFilterField::LineId);
    assert_eq!(val, sea_orm::Value::Uuid(Some(Box::new(model.line_id))));
}

#[test]
fn journal_line_cursor_payer_tenant_id_is_uuid() {
    let model = sample_journal_line();
    let val =
        JournalLineODataMapper::extract_cursor_value(&model, JournalLineFilterField::PayerTenantId);
    assert_eq!(
        val,
        sea_orm::Value::Uuid(Some(Box::new(Uuid::from_u128(0x3B)))),
    );
}

#[test]
fn journal_line_cursor_account_class_is_string() {
    let model = sample_journal_line();
    let val =
        JournalLineODataMapper::extract_cursor_value(&model, JournalLineFilterField::AccountClass);
    assert_eq!(val, sea_orm::Value::String(Some(Box::new("AR".to_owned()))));
}

#[test]
fn journal_line_cursor_period_id_is_string() {
    let model = sample_journal_line();
    let val =
        JournalLineODataMapper::extract_cursor_value(&model, JournalLineFilterField::PeriodId);
    assert_eq!(
        val,
        sea_orm::Value::String(Some(Box::new("2025-01".to_owned()))),
    );
}

#[test]
fn journal_line_cursor_invoice_id_some() {
    let model = sample_journal_line();
    let val =
        JournalLineODataMapper::extract_cursor_value(&model, JournalLineFilterField::InvoiceId);
    assert_eq!(
        val,
        sea_orm::Value::String(Some(Box::new("INV-001".to_owned()))),
    );
}

#[test]
fn journal_line_cursor_invoice_id_none_is_null_string() {
    let mut model = sample_journal_line();
    model.invoice_id = None;
    let val =
        JournalLineODataMapper::extract_cursor_value(&model, JournalLineFilterField::InvoiceId);
    assert_eq!(val, sea_orm::Value::String(None));
}

// ---------------------------------------------------------------------------
// BalanceODataMapper — map_field (column name lock)
// ---------------------------------------------------------------------------

#[test]
fn balance_map_account_id() {
    assert_eq!(
        BalanceODataMapper::map_field(BalanceFilterField::AccountId).as_str(),
        BalanceColumn::AccountId.as_str(),
    );
}

#[test]
fn balance_map_account_class() {
    assert_eq!(
        BalanceODataMapper::map_field(BalanceFilterField::AccountClass).as_str(),
        BalanceColumn::AccountClass.as_str(),
    );
}

#[test]
fn balance_map_currency() {
    assert_eq!(
        BalanceODataMapper::map_field(BalanceFilterField::Currency).as_str(),
        BalanceColumn::Currency.as_str(),
    );
}

// ---------------------------------------------------------------------------
// BalanceODataMapper — extract_cursor_value
// ---------------------------------------------------------------------------

#[test]
fn balance_cursor_account_id_is_uuid() {
    let model = sample_account_balance();
    let val = BalanceODataMapper::extract_cursor_value(&model, BalanceFilterField::AccountId);
    assert_eq!(
        val,
        sea_orm::Value::Uuid(Some(Box::new(Uuid::from_u128(0xB1)))),
    );
}

#[test]
fn balance_cursor_account_class_is_string() {
    let model = sample_account_balance();
    let val = BalanceODataMapper::extract_cursor_value(&model, BalanceFilterField::AccountClass);
    assert_eq!(
        val,
        sea_orm::Value::String(Some(Box::new("REVENUE".to_owned()))),
    );
}

#[test]
fn balance_cursor_currency_is_string() {
    let model = sample_account_balance();
    let val = BalanceODataMapper::extract_cursor_value(&model, BalanceFilterField::Currency);
    assert_eq!(
        val,
        sea_orm::Value::String(Some(Box::new("GBP".to_owned()))),
    );
}

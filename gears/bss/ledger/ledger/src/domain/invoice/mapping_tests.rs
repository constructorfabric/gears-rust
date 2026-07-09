//! Tests for the account-mapping resolver ([`super::resolve`]).

use super::*;
use crate::domain::invoice::builder::InvoiceItem;

/// An item with the two mapping inputs set; everything else fixed.
fn item(catalog: Option<AccountClass>, contract: Option<AccountClass>) -> InvoiceItem {
    InvoiceItem {
        amount_minor_ex_tax: 1000,
        deferred_minor: 0,
        currency: "USD".to_owned(),
        revenue_stream: "subscription".to_owned(),
        catalog_class: catalog,
        contract_class: contract,
        gl_code: Some("4000".to_owned()),
        recognition: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
    }
}

#[test]
fn catalog_class_is_used_when_present() {
    let m = resolve(&item(Some(AccountClass::Revenue), None));
    assert_eq!(m.account_class, AccountClass::Revenue);
    assert_eq!(m.mapping_status, MappingStatus::Resolved);
    assert_eq!(m.gl_code.as_deref(), Some("4000"));
}

#[test]
fn contract_override_wins_over_catalog() {
    // Both present: the Contract override beats the Catalog default.
    let m = resolve(&item(
        Some(AccountClass::Revenue),
        Some(AccountClass::ContraRevenue),
    ));
    assert_eq!(
        m.account_class,
        AccountClass::ContraRevenue,
        "the contract override must win over the catalog class"
    );
    assert_eq!(m.mapping_status, MappingStatus::Resolved);
}

#[test]
fn contract_class_is_used_when_only_it_is_present() {
    let m = resolve(&item(None, Some(AccountClass::Revenue)));
    assert_eq!(m.account_class, AccountClass::Revenue);
    assert_eq!(m.mapping_status, MappingStatus::Resolved);
}

#[test]
fn miss_routes_to_suspense_pending() {
    // Neither mapping present: route to SUSPENSE/PENDING, NEVER a silent
    // wrong-revenue mapping.
    let m = resolve(&item(None, None));
    assert_eq!(m.account_class, AccountClass::Suspense);
    assert_eq!(m.mapping_status, MappingStatus::Pending);
    // The Catalog gl_code is still carried through for the operator's context.
    assert_eq!(m.gl_code.as_deref(), Some("4000"));
}

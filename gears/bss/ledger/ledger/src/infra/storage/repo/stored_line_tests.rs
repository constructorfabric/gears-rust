//! Stored journal line decoding: a valid round trip and every corrupt column.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_ledger_sdk::{AccountClass, MappingStatus, Side};
use uuid::Uuid;

use super::decode_line;
use crate::domain::model::RepoError;
use crate::infra::storage::entity::journal_line;

fn row() -> journal_line::Model {
    let tenant = Uuid::now_v7();
    journal_line::Model {
        line_id: Uuid::now_v7(),
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        period_id: "202606".to_owned(),
        payer_tenant_id: tenant,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::now_v7(),
        account_class: AccountClass::Ar.as_str().to_owned(),
        gl_code: None,
        side: Side::Debit.as_str().to_owned(),
        amount: "-12.34".to_owned(),
        currency: "USD".to_owned(),
        currency_scale: 2,
        invoice_id: Some("inv-1".to_owned()),
        due_date: None,
        revenue_stream: None,
        mapping_status: MappingStatus::Resolved.as_str().to_owned(),
        functional_amount: Some("1500".to_owned()),
        functional_currency: Some("JPY".to_owned()),
        functional_currency_scale: Some(0),
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

/// The diagnostic of an `InvalidStoredMoney`, or a panic naming what came back.
fn stored_error(row: &journal_line::Model) -> String {
    match decode_line(row) {
        Err(RepoError::InvalidStoredMoney(detail)) => detail,
        other => panic!("expected InvalidStoredMoney, got {other:?}"),
    }
}

#[test]
fn a_valid_row_round_trips_money_metadata_and_enums() {
    let row = row();
    let line = decode_line(&row).unwrap();
    assert_eq!(line.line_id, row.line_id);
    assert_eq!(line.account_class, AccountClass::Ar);
    assert_eq!(line.side, Side::Debit);
    assert_eq!(line.mapping_status, MappingStatus::Resolved);
    assert_eq!(line.money.to_string(), "-12.34 USD");
    assert_eq!(line.money.currency().scale(), 2);
    let functional = line.functional_money.unwrap();
    assert_eq!(functional.to_string(), "1500 JPY");
    assert_eq!(functional.currency().scale(), 0);
    assert_eq!(line.invoice_id.as_deref(), Some("inv-1"));
}

#[test]
fn a_corrupt_enum_names_the_line_column_and_literal() {
    for (column, set) in [
        (
            "account_class",
            (|r: &mut journal_line::Model| r.account_class = "BOGUS".into())
                as fn(&mut journal_line::Model),
        ),
        ("side", |r| r.side = "BOGUS".into()),
        ("mapping_status", |r| r.mapping_status = "BOGUS".into()),
    ] {
        let mut corrupt = row();
        set(&mut corrupt);
        let detail = stored_error(&corrupt);
        assert!(detail.contains(&corrupt.line_id.to_string()), "{detail}");
        assert!(detail.contains(column), "{detail}");
        assert!(detail.contains("\"BOGUS\""), "{detail}");
    }
}

#[test]
fn corrupt_money_and_half_null_functional_triples_are_invalid_stored_money() {
    type Corrupt = fn(&mut journal_line::Model);
    let corruptions: [(&str, Corrupt); 7] = [
        ("amount", |r| r.amount = "12.340".into()),
        ("amount", |r| r.amount = "12.345".into()),
        ("amount", |r| r.amount = "garbage".into()),
        ("amount", |r| r.currency_scale = 29),
        ("functional_amount", |r| r.functional_currency = None),
        ("functional_amount", |r| r.functional_currency_scale = None),
        ("functional_amount", |r| {
            r.functional_amount = Some("1500.0".into());
        }),
    ];
    for (column, set) in corruptions {
        let mut corrupt = row();
        set(&mut corrupt);
        let detail = stored_error(&corrupt);
        assert!(detail.contains(&corrupt.line_id.to_string()), "{detail}");
        assert!(detail.contains(column), "{column}: {detail}");
    }
}

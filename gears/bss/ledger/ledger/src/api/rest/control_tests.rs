//! Control-feed intake validation: the issued-invoice manifest's per-currency
//! gross totals and the PSP report's settled money.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::api::rest::money::MoneyDto;
use crate::domain::error::DomainError;

fn dto(amount: &str, currency: &str, scale: u8) -> MoneyDto {
    MoneyDto {
        amount: amount.to_owned(),
        currency: currency.to_owned(),
        currency_scale: scale,
    }
}

#[test]
fn a_valid_multi_currency_manifest_is_accepted_and_sorted_by_code() {
    let totals = validate_gross_totals(vec![
        dto("10.50", "USD", 2),
        dto("1200", "JPY", 0),
        dto("3.25", "EUR", 2),
    ])
    .unwrap();
    let codes: Vec<_> = totals.iter().map(|t| t.currency().code()).collect();
    assert_eq!(codes, ["EUR", "JPY", "USD"]);
    assert_eq!(
        totals.as_slice()[2].amount(),
        rust_decimal::Decimal::new(105, 1)
    );
    assert_eq!(totals.get("JPY").map(|t| t.currency().scale()), Some(0));
    assert!(validate_gross_totals(Vec::new()).unwrap().is_empty());
}

#[test]
fn a_repeated_currency_is_rejected_at_any_scale() {
    for second in [dto("2", "USD", 2), dto("2", "USD", 3)] {
        match validate_gross_totals(vec![dto("1", "USD", 2), dto("5", "EUR", 2), second]) {
            Err(DomainError::InvalidRequest(detail)) => {
                assert!(detail.contains("repeats currency USD"), "{detail}");
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }
}

#[test]
fn malformed_totals_and_oversized_lists_are_rejected() {
    assert!(matches!(
        validate_gross_totals(vec![dto("1.234", "USD", 2)]),
        Err(DomainError::InvalidPostingIncrement(_))
    ));
    assert!(matches!(
        validate_gross_totals(vec![dto("abc", "USD", 2)]),
        Err(DomainError::InvalidRequest(_))
    ));
    let many = (0..=MAX_GROSS_TOTALS)
        .map(|i| dto("1", &format!("C{i}"), 2))
        .collect();
    match validate_gross_totals(many) {
        Err(DomainError::InvalidRequest(detail)) => assert!(detail.contains("at most"), "{detail}"),
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

#[test]
fn a_malformed_settled_amount_is_a_named_client_error() {
    let body: PspSettlementReportRequest = serde_json::from_value(serde_json::json!({
        "tenant_id": uuid::Uuid::now_v7(),
        "period_id": "202610",
        "report_id": "R-1",
        "settled": {"amount": "10.001", "currency": "USD", "currency_scale": 2}
    }))
    .unwrap();
    match parse_money("settled", body.settled) {
        Err(DomainError::InvalidPostingIncrement(detail)) => {
            assert!(detail.starts_with("settled:"), "{detail}");
        }
        other => panic!("expected InvalidPostingIncrement, got {other:?}"),
    }
}

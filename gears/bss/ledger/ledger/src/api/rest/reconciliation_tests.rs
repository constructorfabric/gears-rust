//! Wire shape of the reconciliation-run read: the tagged variance (`kind` =
//! `money` / `missing_invoices`) and the run projection from the repo row.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::domain::reconciliation::ReconciliationVariance;
use crate::infra::storage::repo::reconciliation_run_repo::ReconciliationRunView as RunRow;

fn row(variance: ReconciliationVariance) -> RunRow {
    RunRow {
        tenant_id: Uuid::from_u128(7),
        run_id: Uuid::from_u128(1),
        period_id: "202610".to_owned(),
        check_type: "PSP_SETTLEMENT".to_owned(),
        variance,
        within_tolerance: false,
        status: "OPEN".to_owned(),
        watermark: Some(42),
        detail: Some(serde_json::json!({"note": "server-side only"})),
        at_utc: crate::domain::instant::utc_ymd_hms(2026, 10, 9, 12, 0, 0),
    }
}

fn money(text: &str, code: &str, scale: u8) -> bss_ledger_sdk::PostedMoney {
    bss_ledger_sdk::PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        bss_ledger_sdk::CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

#[test]
fn money_variance_run_serializes_with_its_kind_tag_and_per_currency_values() {
    let view = ReconciliationRunView::from(row(ReconciliationVariance::Money {
        by_currency: vec![money("-12.5", "EUR", 2), money("300", "JPY", 0)],
    }));
    assert_eq!(
        serde_json::to_value(&view).unwrap(),
        serde_json::json!({
            "run_id": Uuid::from_u128(1),
            "period_id": "202610",
            "check_type": "PSP_SETTLEMENT",
            "variance": {
                "kind": "money",
                "by_currency": [
                    {"amount": "-12.5", "currency": "EUR", "currency_scale": 2},
                    {"amount": "300", "currency": "JPY", "currency_scale": 0}
                ]
            },
            "within_tolerance": false,
            "status": "OPEN",
            "at_utc": "2026-10-09T12:00:00.000000Z"
        })
    );
}

#[test]
fn missing_invoices_variance_serializes_with_its_kind_tag_and_count() {
    let view =
        ReconciliationRunView::from(row(ReconciliationVariance::MissingInvoices { count: 3 }));
    assert_eq!(
        serde_json::to_value(&view).unwrap()["variance"],
        serde_json::json!({"kind": "missing_invoices", "count": 3})
    );
}

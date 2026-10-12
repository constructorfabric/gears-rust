//! Closed canonical storage codec for reconciliation evidence; not a wire parser.
use crate::domain::model::RepoError;
use crate::domain::reconciliation::{
    CHECK_AR_DERIVED, CHECK_INVOICE_COMPLETENESS, CHECK_PAYMENTS_PSP, ReconciliationVariance,
};
use crate::infra::storage::money_text::{decode_money, encode_amount};
use bss_ledger_sdk::PostedMoney;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Closed nested money storage shape. SDK/domain money stays transport-free.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredMoney {
    amount: String,
    currency: String,
    currency_scale: i16,
}

/// Closed tagged representation; counts are integer metadata, never money.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StoredVariance {
    Money { by_currency: Vec<StoredMoney> },
    MissingInvoices { count: u64 },
}

/// Validate the check/variant pairing without inventing a currency for empty runs.
pub(super) fn zero(check: &str) -> Result<ReconciliationVariance, RepoError> {
    match check {
        CHECK_AR_DERIVED | CHECK_PAYMENTS_PSP => Ok(ReconciliationVariance::Money {
            by_currency: vec![],
        }),
        CHECK_INVOICE_COMPLETENESS => Ok(ReconciliationVariance::MissingInvoices { count: 0 }),
        _ => Err(RepoError::InvalidRequest(
            "unknown reconciliation check".into(),
        )),
    }
}

/// Require the check's exact variant and one bucket per currency.
fn validate(check: &str, variance: &ReconciliationVariance) -> Result<(), RepoError> {
    if !matches!(
        (zero(check)?, variance),
        (
            ReconciliationVariance::Money { .. },
            ReconciliationVariance::Money { .. }
        ) | (
            ReconciliationVariance::MissingInvoices { .. },
            ReconciliationVariance::MissingInvoices { .. }
        )
    ) {
        return Err(RepoError::InvalidRequest(
            "reconciliation check/variance kind mismatch".into(),
        ));
    }
    if let ReconciliationVariance::Money { by_currency } = variance {
        let mut seen = std::collections::HashSet::new();
        for money in by_currency {
            if !seen.insert(money.currency().code()) {
                return Err(RepoError::InvalidRequest(
                    "duplicate reconciliation currency".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Encode new validated input in deterministic currency order.
pub(super) fn encode(check: &str, variance: &ReconciliationVariance) -> Result<Value, RepoError> {
    validate(check, variance)?;
    let stored = match variance {
        ReconciliationVariance::Money { by_currency } => {
            let mut money: Vec<&PostedMoney> = by_currency.iter().collect();
            money.sort_by_key(|m| m.currency().code());
            StoredVariance::Money {
                by_currency: money
                    .into_iter()
                    .map(|m| StoredMoney {
                        amount: encode_amount(m),
                        currency: m.currency().code().to_owned(),
                        currency_scale: i16::from(m.currency().scale()),
                    })
                    .collect(),
            }
        }
        ReconciliationVariance::MissingInvoices { count } => {
            StoredVariance::MissingInvoices { count: *count }
        }
    };
    serde_json::to_value(stored).map_err(|e| RepoError::Db(format!("encode variance: {e}")))
}

/// Strictly restore canonical evidence; never normalize stored money or ordering.
pub(super) fn decode(check: &str, text: &str) -> Result<ReconciliationVariance, RepoError> {
    let stored: StoredVariance = serde_json::from_str(text)
        .map_err(|e| RepoError::InvalidStoredMoney(format!("reconciliation variance: {e}")))?;
    let variance = match stored {
        StoredVariance::Money { by_currency } => {
            let mut money = Vec::with_capacity(by_currency.len());
            let mut previous: Option<String> = None;
            for item in by_currency {
                if previous
                    .as_deref()
                    .is_some_and(|p| p >= item.currency.as_str())
                {
                    return Err(RepoError::InvalidStoredMoney(
                        "noncanonical reconciliation currency order or duplicate".into(),
                    ));
                }
                money.push(decode_money(
                    &item.amount,
                    &item.currency,
                    item.currency_scale,
                )?);
                previous = Some(item.currency);
            }
            ReconciliationVariance::Money { by_currency: money }
        }
        StoredVariance::MissingInvoices { count } => {
            ReconciliationVariance::MissingInvoices { count }
        }
    };
    validate(check, &variance).map_err(|e| RepoError::InvalidStoredMoney(e.to_string()))?;
    Ok(variance)
}

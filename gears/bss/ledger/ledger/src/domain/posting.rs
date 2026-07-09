//! Pure, backend-agnostic posting invariants — the same checks the P1
//! `bss.check_entry_balanced` trigger enforces, reproduced in app code so
//! the engine errors deterministically on both backends and before COMMIT.

use bss_ledger_sdk::Side;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Minimal per-line facts the balance check needs.
#[domain_model]
#[derive(Clone, Debug)]
pub struct LineFacts {
    pub side: Side,
    pub amount_minor: i64,
    pub currency: String,
    pub currency_scale: u8,
    pub payer_tenant_id: Uuid,
    pub functional_amount_minor: Option<i64>,
}

impl LineFacts {
    /// A functional-only line carries no transaction-currency amount.
    fn is_functional_only(&self) -> bool {
        self.amount_minor == 0 && self.functional_amount_minor.is_some()
    }
}

/// A structural posting-invariant breach. Projected into a [`DomainError`]
/// (the gear's single canonical-mapping vocabulary) by the `From` impl below.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PostingViolation {
    #[error("entry has no lines")]
    Empty,
    #[error("entry does not net to zero per currency")]
    Unbalanced,
    #[error("entry spans more than one payer tenant")]
    MixedPayer,
    #[error("line currency does not match the entry currency")]
    CurrencyMismatch,
    #[error("lines in the same currency carry different scales")]
    InconsistentScale,
    #[error("line amount must be positive, or zero with a functional amount")]
    AmountOutOfRange,
    #[error("entry mixes functional and non-functional lines")]
    FunctionalPartial,
    #[error("entry does not net to zero in the functional currency")]
    FunctionalUnbalanced,
}

impl From<PostingViolation> for DomainError {
    // Explicit per-variant detail literals (not `v.to_string()`): the source
    // error type must not be flattened into a string here (DE1302), and the
    // literals keep the domain detail clean (no category-prefix doubling).
    fn from(v: PostingViolation) -> Self {
        match v {
            PostingViolation::Empty => Self::Empty("entry has no lines".to_owned()),
            PostingViolation::MixedPayer => {
                Self::MixedPayer("entry spans more than one payer tenant".to_owned())
            }
            // A wrong per-line scale changes the implied magnitude of the
            // amount — surfaced as out-of-range (wire `AMOUNT_OUT_OF_RANGE`)
            // rather than a balance fault, preserving the prior contract.
            PostingViolation::InconsistentScale => Self::InconsistentScale(
                "lines in the same currency carry different scales".to_owned(),
            ),
            PostingViolation::Unbalanced => {
                Self::Unbalanced("entry does not net to zero per currency".to_owned())
            }
            // CurrencyMismatch has no dedicated variant — a line in another
            // currency cannot net to zero, so it surfaces as unbalanced.
            PostingViolation::CurrencyMismatch => {
                Self::Unbalanced("line currency does not match the entry currency".to_owned())
            }
            PostingViolation::AmountOutOfRange => Self::AmountOutOfRange(
                "line amount must be positive, or zero with a functional amount".to_owned(),
            ),
            // FX dual-column (Slice 5): a partial-functional or functional-imbalance
            // entry is a balance-class fault (no dedicated canonical variant).
            PostingViolation::FunctionalPartial => {
                Self::Unbalanced("entry mixes functional and non-functional lines".to_owned())
            }
            PostingViolation::FunctionalUnbalanced => {
                Self::Unbalanced("entry does not net to zero in the functional currency".to_owned())
            }
        }
    }
}

/// Validate the structural invariants of a balanced entry.
///
/// # Errors
/// A [`PostingViolation`] when the entry is empty, spans payers, mixes
/// currencies, or does not net to zero per `(currency, scale)` group.
pub fn validate_balanced_entry(
    entry_currency: &str,
    lines: &[LineFacts],
) -> Result<(), PostingViolation> {
    if lines.is_empty() {
        return Err(PostingViolation::Empty);
    }
    // chk_journal_line_amount, reproduced before COMMIT: every amount must be
    // positive, or exactly zero with a POSITIVE functional amount (functional-only
    // line — the DR/CR side carries the sign, so the functional amount is > 0).
    if lines.iter().any(|l| {
        l.amount_minor < 0
            || (l.amount_minor == 0 && !matches!(l.functional_amount_minor, Some(f) if f > 0))
    }) {
        return Err(PostingViolation::AmountOutOfRange);
    }
    let first_payer = lines[0].payer_tenant_id;
    if lines.iter().any(|l| l.payer_tenant_id != first_payer) {
        return Err(PostingViolation::MixedPayer);
    }
    if lines
        .iter()
        .any(|l| l.currency != entry_currency && !l.is_functional_only())
    {
        return Err(PostingViolation::CurrencyMismatch);
    }
    // One scale per currency: the registry resolves a single scale per
    // currency and the currency-keyed balance caches hold one magnitude, so a
    // line whose scale disagrees would corrupt the cached magnitude even if the
    // entry nets to zero. `PostingService::post` is `pub` and trusts the
    // caller-supplied `currency_scale`, so this guards that surface too.
    let mut scale_by_ccy: std::collections::HashMap<&str, u8> = std::collections::HashMap::new();
    for l in lines {
        if let Some(&s) = scale_by_ccy.get(l.currency.as_str())
            && s != l.currency_scale
        {
            return Err(PostingViolation::InconsistentScale);
        }
        scale_by_ccy
            .entry(l.currency.as_str())
            .or_insert(l.currency_scale);
    }
    // Net per (currency, scale) group must be exactly zero.
    let mut groups: std::collections::HashMap<(&str, u8), i128> = std::collections::HashMap::new();
    for l in lines {
        let signed = match l.side {
            Side::Debit => i128::from(l.amount_minor),
            Side::Credit => -i128::from(l.amount_minor),
        };
        *groups
            .entry((l.currency.as_str(), l.currency_scale))
            .or_insert(0) += signed;
    }
    if groups.values().any(|net| *net != 0) {
        return Err(PostingViolation::Unbalanced);
    }
    // FX dual-column functional balance (NULL-aware; mirrors bss.check_entry_balanced).
    // f = count(functional Some): f=0 → single-currency, skip; f=len → enforce
    // SUM(DR.functional) == SUM(CR.functional); 0<f<len → partial-functional bug.
    let func_count = lines
        .iter()
        .filter(|l| l.functional_amount_minor.is_some())
        .count();
    if func_count > 0 && func_count < lines.len() {
        return Err(PostingViolation::FunctionalPartial);
    }
    if func_count == lines.len() {
        let mut func_net: i128 = 0;
        for l in lines {
            let f = i128::from(l.functional_amount_minor.unwrap_or(0));
            func_net += match l.side {
                Side::Debit => f,
                Side::Credit => -f,
            };
        }
        if func_net != 0 {
            return Err(PostingViolation::FunctionalUnbalanced);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "posting_tests.rs"]
mod tests;

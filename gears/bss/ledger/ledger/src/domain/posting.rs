//! Pure, backend-agnostic posting invariants — the same checks the P1
//! `bss.check_entry_balanced` trigger enforces, reproduced in app code so
//! the engine errors deterministically on both backends and before COMMIT.

use crate::domain::exact_money::ExactAmount;
use bss_ledger_sdk::{PostedMoney, Side};
use rust_decimal::Decimal;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Minimal per-line facts the balance check needs.
#[domain_model]
#[derive(Clone, Debug)]
pub struct LineFacts {
    pub side: Side,
    pub money: PostedMoney,
    pub payer_tenant_id: Uuid,
    pub functional_money: Option<PostedMoney>,
}

impl LineFacts {
    /// A functional-only line carries no transaction-currency amount.
    fn is_functional_only(&self) -> bool {
        is_functional_only(&self.money, self.functional_money.as_ref())
    }
}

/// The engine's functional-only admission rule: a line with a zero transaction
/// amount and a functional value carries no transaction value, so it may be in
/// another currency than the entry. The one predicate the engine and the
/// reversal builder (`domain::invoice::reversal`) share.
#[must_use]
pub(crate) fn is_functional_only(
    money: &PostedMoney,
    functional_money: Option<&PostedMoney>,
) -> bool {
    money.amount().is_zero() && functional_money.is_some()
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
            // Preserve the named scale mismatch at the API boundary.
            PostingViolation::InconsistentScale => Self::InconsistentScale(
                "lines in the same currency carry different scales".to_owned(),
            ),
            PostingViolation::Unbalanced => {
                Self::Unbalanced("entry does not net to zero per currency".to_owned())
            }
            // Currency mismatch is distinct from an unequal monetary sum.
            PostingViolation::CurrencyMismatch => {
                Self::CurrencyMismatch("line currency does not match the entry currency".to_owned())
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
    // Preserve the existing positive transaction / positive functional-only rule.
    if lines.iter().any(|line| {
        line.money.amount() < Decimal::ZERO
            || (line.money.amount().is_zero()
                && line
                    .functional_money
                    .as_ref()
                    .is_none_or(|f| f.amount() <= Decimal::ZERO))
    }) {
        return Err(PostingViolation::AmountOutOfRange);
    }
    let first_payer = lines[0].payer_tenant_id;
    if lines.iter().any(|line| line.payer_tenant_id != first_payer) {
        return Err(PostingViolation::MixedPayer);
    }
    if lines
        .iter()
        .any(|line| line.money.currency().code() != entry_currency && !line.is_functional_only())
    {
        return Err(PostingViolation::CurrencyMismatch);
    }
    let mut groups = std::collections::HashMap::new();
    for line in lines {
        let spec = line.money.currency();
        let (scale, net) = groups
            .entry(spec.code())
            .or_insert_with(|| (spec.scale(), ExactAmount::from_decimal(Decimal::ZERO)));
        if *scale != spec.scale() {
            return Err(PostingViolation::InconsistentScale);
        }
        *net = add_signed(net, &line.money, line.side)?;
    }
    let zero = ExactAmount::from_decimal(Decimal::ZERO);
    if groups.values().any(|(_, net)| *net != zero) {
        return Err(PostingViolation::Unbalanced);
    }
    let func_count = lines
        .iter()
        .filter(|line| line.functional_money.is_some())
        .count();
    if func_count > 0 && func_count < lines.len() {
        return Err(PostingViolation::FunctionalPartial);
    }
    if let Some(first) = lines[0].functional_money.as_ref() {
        let mut net = zero.clone();
        for line in lines {
            let Some(money) = line.functional_money.as_ref() else {
                return Err(PostingViolation::FunctionalPartial);
            };
            if money.currency().code() != first.currency().code() {
                return Err(PostingViolation::CurrencyMismatch);
            }
            if money.currency().scale() != first.currency().scale() {
                return Err(PostingViolation::InconsistentScale);
            }
            net = add_signed(&net, money, line.side)?;
        }
        if net != zero {
            return Err(PostingViolation::FunctionalUnbalanced);
        }
    }
    Ok(())
}

/// Add a journal leg without narrowing intermediate totals to a posted amount.
fn add_signed(
    net: &ExactAmount,
    money: &PostedMoney,
    side: Side,
) -> Result<ExactAmount, PostingViolation> {
    let value = ExactAmount::from_decimal(money.amount());
    match side {
        Side::Debit => net.checked_add(&value),
        Side::Credit => net.checked_sub(&value),
    }
    .map_err(|_| PostingViolation::AmountOutOfRange)
}

#[cfg(test)]
#[path = "posting_tests.rs"]
mod tests;

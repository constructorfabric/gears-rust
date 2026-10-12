//! Exact arithmetic from `bss-money`, plus the ledger's mapping of its errors
//! onto the domain error vocabulary.

use bss_ledger_sdk::money::{CurrencySpec, MoneyError, PostedMoney};
pub use bss_money::exact::{ExactAmount, ExactError, ScaledSum, sum_posted};
use rust_decimal::Decimal;

/// Preserve named SDK numeric errors at pure domain boundaries.
pub(crate) fn map_money_error(error: MoneyError) -> crate::domain::error::DomainError {
    let detail = error.to_string();
    money_error_with_detail(error, detail)
}

/// The one `MoneyError` to domain-error table; every boundary (REST field parse,
/// stored reversal, repository, exact arithmetic) maps through it, so the wire
/// code for a money violation does not depend on the endpoint.
pub(crate) fn money_error_with_detail(
    error: MoneyError,
    detail: String,
) -> crate::domain::error::DomainError {
    use crate::domain::error::DomainError as D;
    match error {
        MoneyError::InvalidDecimal | MoneyError::InvalidCurrency => D::InvalidRequest(detail),
        MoneyError::ScaleOutOfRange => D::ScaleOutOfRange(detail),
        MoneyError::AmountOutOfRange => D::AmountOutOfRange(detail),
        MoneyError::InvalidPostingIncrement => D::InvalidPostingIncrement(detail),
        MoneyError::CurrencyMismatch => D::CurrencyMismatch(detail),
        MoneyError::ScaleMismatch => D::InconsistentScale(detail),
    }
}

/// Preserve numeric failures; structural exact-operation defects remain invariant failures.
pub(crate) fn map_exact_error(error: ExactError) -> crate::domain::error::DomainError {
    use crate::domain::error::DomainError as D;
    match error {
        ExactError::Money(error) => map_money_error(error),
        ExactError::ArithmeticLimit => D::AmountOutOfRange(error.to_string()),
        ExactError::TooManyTerms => D::ScheduleTooLong(error.to_string()),
        ExactError::DivisionByZero | ExactError::EmptyWeights | ExactError::NegativeWeight => {
            D::Internal(error.to_string())
        }
    }
}

/// Check both metadata dimensions (code and stored scale) of two currency specs,
/// for a caller that holds a [`CurrencySpec`] rather than a posting.
pub(crate) fn matching_currency(
    left: &CurrencySpec,
    right: &CurrencySpec,
) -> Result<(), crate::domain::error::DomainError> {
    left.ensure_same(right).map_err(map_money_error)
}

/// Check both metadata dimensions before comparing or calculating money, including zero.
pub(crate) fn matching_spec(
    left: &PostedMoney,
    right: &PostedMoney,
) -> Result<(), crate::domain::error::DomainError> {
    matching_currency(left.currency(), right.currency())
}

/// Exact difference narrowed only to the final posting with the validated stored spec.
pub(crate) fn subtract_posted(
    left: &PostedMoney,
    right: &PostedMoney,
) -> Result<PostedMoney, crate::domain::error::DomainError> {
    matching_spec(left, right)?;
    ExactAmount::from_decimal(left.amount())
        .checked_sub(&ExactAmount::from_decimal(right.amount()))
        .and_then(|value| value.into_posted_exact(left.currency().clone()))
        .map_err(map_exact_error)
}

/// Explicit-spec zero for a validated monetary context.
pub(crate) fn zero_posted(
    basis: &PostedMoney,
) -> Result<PostedMoney, crate::domain::error::DomainError> {
    PostedMoney::try_new(Decimal::ZERO, basis.currency().clone()).map_err(map_money_error)
}

#[cfg(test)]
#[path = "exact_money_tests.rs"]
mod exact_money_tests;

#[cfg(test)]
#[path = "exact_money_refs_tests.rs"]
mod exact_money_refs_tests;

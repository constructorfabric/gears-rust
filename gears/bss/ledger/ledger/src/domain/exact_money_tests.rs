//! The ledger mapping of exact-arithmetic errors onto the domain vocabulary.

use super::{ExactError, map_exact_error, map_money_error};
use bss_ledger_sdk::money::MoneyError;

#[test]
fn pure_domain_mapping_preserves_named_numeric_categories() {
    use crate::domain::error::DomainError as D;
    for (error, expected) in [
        (
            MoneyError::InvalidDecimal,
            D::InvalidRequest("invalid decimal".to_owned()),
        ),
        (
            MoneyError::InvalidCurrency,
            D::InvalidRequest("invalid currency".to_owned()),
        ),
        (
            MoneyError::ScaleOutOfRange,
            D::ScaleOutOfRange("scale".to_owned()),
        ),
        (
            MoneyError::AmountOutOfRange,
            D::AmountOutOfRange("amount".to_owned()),
        ),
        (
            MoneyError::InvalidPostingIncrement,
            D::InvalidPostingIncrement("grid".to_owned()),
        ),
        (
            MoneyError::CurrencyMismatch,
            D::CurrencyMismatch("code".to_owned()),
        ),
        (
            MoneyError::ScaleMismatch,
            D::InconsistentScale("scale".to_owned()),
        ),
    ] {
        assert_eq!(
            std::mem::discriminant(&map_money_error(error.clone())),
            std::mem::discriminant(&expected)
        );
        assert_eq!(
            std::mem::discriminant(&map_exact_error(ExactError::Money(error))),
            std::mem::discriminant(&expected)
        );
    }
    assert!(matches!(
        map_exact_error(ExactError::ArithmeticLimit),
        D::AmountOutOfRange(_)
    ));
    assert!(matches!(
        map_exact_error(ExactError::TooManyTerms),
        D::ScheduleTooLong(_)
    ));
    for error in [
        ExactError::DivisionByZero,
        ExactError::EmptyWeights,
        ExactError::NegativeWeight,
    ] {
        assert!(matches!(map_exact_error(error), D::Internal(_)));
    }
}

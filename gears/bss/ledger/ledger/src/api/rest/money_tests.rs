//! Portable money codec contract checks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::money::MoneyDto;
use crate::domain::model::RepoError;
use crate::infra::storage::money_text::{decode_amount, encode_amount};
use bss_ledger_sdk::{CurrencySpec, MoneyError, PostedMoney, parse_decimal};

#[test]
fn wire_amount_must_be_a_string() {
    let number = r#"{"amount":12.34,"currency":"EUR","currency_scale":2}"#;
    assert!(serde_json::from_str::<MoneyDto>(number).is_err());
    let text = r#"{"amount":"12.34","currency":"EUR","currency_scale":2}"#;
    let dto: MoneyDto = serde_json::from_str(text).unwrap();
    assert_eq!(
        PostedMoney::try_from(dto).unwrap().amount(),
        parse_decimal("12.34").unwrap()
    );
}

#[test]
fn wire_output_is_canonical_and_keeps_metadata() {
    let dto: MoneyDto = serde_json::from_value(serde_json::json!({
        "amount": "10.000", "currency": "EUR", "currency_scale": 2
    }))
    .unwrap();
    let value = PostedMoney::try_from(dto).unwrap();
    assert_eq!(
        serde_json::to_value(MoneyDto::from(&value)).unwrap(),
        serde_json::json!({
            "amount": "10", "currency": "EUR", "currency_scale": 2
        })
    );
}

#[test]
fn wire_rejects_malformed_and_out_of_contract_money() {
    for text in [
        "",
        " ",
        " 1",
        "1 ",
        "+1",
        "01",
        "-01",
        ".1",
        "1.",
        "1e2",
        "1E2",
        "1,2",
        "1_000",
        "NaN",
        "Infinity",
        "-0",
        "-0.00",
        "--1",
        "1.2.3",
        "١",
        "0.047",
        "10000000000000000000000000000",
        "0.00000000000000000000000000001",
    ] {
        let dto = MoneyDto {
            amount: text.to_owned(),
            currency: "EUR".to_owned(),
            currency_scale: 2,
        };
        assert!(PostedMoney::try_from(dto).is_err(), "{text:?}");
    }
    assert!(
        PostedMoney::try_from(MoneyDto {
            amount: format!("1.{}", "0".repeat(63)),
            currency: "EUR".to_owned(),
            currency_scale: 2
        })
        .is_err()
    );
    for (currency, scale) in [("", 2), ("eur", 2), ("EUR", 29)] {
        assert!(
            PostedMoney::try_from(MoneyDto {
                amount: "1".to_owned(),
                currency: currency.to_owned(),
                currency_scale: scale
            })
            .is_err()
        );
    }
}

#[test]
fn wire_requires_all_metadata() {
    for json in [
        r#"{"amount":"1","currency":"EUR"}"#,
        r#"{"amount":"1","currency_scale":2}"#,
        r#"{"currency":"EUR","currency_scale":2}"#,
        r#"{"amount":"1","currency":null,"currency_scale":2}"#,
    ] {
        assert!(serde_json::from_str::<MoneyDto>(json).is_err(), "{json}");
    }
}

#[test]
fn scale_28_boundary_round_trips_on_wire_and_storage() {
    let text = "0.0000000000000000000000000001";
    let dto = MoneyDto {
        amount: text.to_owned(),
        currency: "PRECISE".to_owned(),
        currency_scale: 28,
    };
    let value = PostedMoney::try_from(dto).unwrap();
    assert_eq!(MoneyDto::from(&value).amount, text);
    assert_eq!(encode_amount(&value), text);
    assert_eq!(
        decode_amount(text, value.currency().clone()).unwrap(),
        value
    );
}

#[test]
fn storage_rejects_corrupt_noncanonical_and_invalid_increment_text() {
    for text in [
        "",
        "1e2",
        " 1",
        "+1",
        "01",
        "-0",
        "10.000",
        "0.00",
        "1.20",
        "0.047",
        "10000000000000000000000000000",
    ] {
        let error =
            decode_amount(text, CurrencySpec::try_new("EUR".to_owned(), 2).unwrap()).unwrap_err();
        assert!(
            matches!(error, RepoError::InvalidStoredMoney(_)),
            "{text:?}: {error:?}"
        );
    }
}

#[test]
fn storage_round_trips_canonical_zero_negative_and_maximum() {
    for text in ["0", "-12.34", "9999999999999999999999999999"] {
        let value =
            decode_amount(text, CurrencySpec::try_new("EUR".to_owned(), 2).unwrap()).unwrap();
        assert_eq!(encode_amount(&value), text);
        assert_eq!(value.currency().code(), "EUR");
        assert_eq!(value.currency().scale(), 2);
    }
}

#[test]
fn money_input_errors_use_invalid_argument() {
    use toolkit::api::canonical_prelude::{CanonicalError, Problem};
    for error in [
        MoneyError::InvalidDecimal,
        MoneyError::InvalidCurrency,
        MoneyError::ScaleOutOfRange,
        MoneyError::AmountOutOfRange,
        MoneyError::InvalidPostingIncrement,
        MoneyError::CurrencyMismatch,
        MoneyError::ScaleMismatch,
    ] {
        let canonical: CanonicalError = super::error::money_error_to_canonical(error);
        assert_eq!(canonical.status_code(), 400);
        assert!(matches!(canonical, CanonicalError::InvalidArgument { .. }));
        assert!(
            serde_json::to_string(&Problem::from(canonical))
                .unwrap()
                .contains("invalid_argument")
        );
    }
}

#[test]
fn money_schema_has_string_amount_limit_and_example() {
    fn both_traits<
        T: toolkit::api::api_dto::RequestApiDto + toolkit::api::api_dto::ResponseApiDto,
    >() {
    }
    let schema = serde_json::to_value(<MoneyDto as utoipa::PartialSchema>::schema()).unwrap();
    assert_eq!(schema["properties"]["amount"]["type"], "string");
    assert_eq!(schema["properties"]["amount"]["maxLength"], 64);
    assert_eq!(schema["properties"]["amount"]["example"], "12.34");
    both_traits::<MoneyDto>();
}

#[test]
fn stored_corruption_uses_existing_internal_error_path_and_redacts_wire() {
    use crate::infra::posting::service::{decode_business_error, repo_to_db};
    use toolkit::api::canonical_prelude::{CanonicalError, Problem};
    for text in ["10.000", "chk_reusable_credit_subbalance_no_negative"] {
        let error =
            decode_amount(text, CurrencySpec::try_new("EUR".to_owned(), 2).unwrap()).unwrap_err();
        let diagnostic = error.to_string();
        let domain = decode_business_error(&repo_to_db(error));
        assert!(
            matches!(domain, crate::domain::error::DomainError::Internal(_)),
            "{text} maps to Internal"
        );
        let canonical = CanonicalError::from(domain);
        assert_eq!(canonical.status_code(), 500);
        assert!(canonical.diagnostic().unwrap().contains(&diagnostic));
        let body = serde_json::to_string(&Problem::from(canonical)).unwrap();
        assert!(!body.contains(&diagnostic));
        assert!(!body.contains(text));
    }
}

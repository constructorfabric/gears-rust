//! Canonical decimal text adapters.
#![allow(clippy::unwrap_used)]

use rust_decimal::Decimal;

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Line {
    #[serde(with = "crate::serde_text")]
    amount: Decimal,
    #[serde(with = "crate::serde_text::option")]
    cap: Option<Decimal>,
}

#[test]
fn canonical_text_round_trips() {
    let line = Line {
        amount: Decimal::from_str_exact("12.340").unwrap(),
        cap: None,
    };
    let json = serde_json::to_string(&line).unwrap();
    assert_eq!(json, r#"{"amount":"12.34","cap":null}"#);
    assert_eq!(serde_json::from_str::<Line>(&json).unwrap(), line);
    let with_cap = r#"{"amount":"-0.5","cap":"100"}"#;
    let parsed: Line = serde_json::from_str(with_cap).unwrap();
    assert_eq!(parsed.cap, Some(Decimal::from(100)));
    assert_eq!(serde_json::to_string(&parsed).unwrap(), with_cap);
}

#[test]
fn noncanonical_and_invalid_text_are_refused() {
    for text in ["12.340", "+1", "1e2", "007", "-0", "1.", ".5", "abc"] {
        let json = format!(r#"{{"amount":"{text}","cap":null}}"#);
        assert!(serde_json::from_str::<Line>(&json).is_err(), "{text}");
    }
    assert!(serde_json::from_str::<Line>(r#"{"amount":12.34,"cap":null}"#).is_err());
    assert!(serde_json::from_str::<Line>(r#"{"amount":"1","cap":"1.50"}"#).is_err());
}

#[test]
fn a_value_the_reader_would_refuse_is_not_written() {
    let line = Line {
        amount: Decimal::from_str_exact("10000000000000000000000000000").unwrap(),
        cap: None,
    };
    assert!(serde_json::to_string(&line).is_err());
}

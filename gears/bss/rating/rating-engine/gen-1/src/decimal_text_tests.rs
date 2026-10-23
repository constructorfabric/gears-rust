#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Price {
    #[serde(with = "super")]
    rate: Decimal,
    #[serde(with = "super::option")]
    fee: Option<Decimal>,
}

fn read(json: &str) -> Result<Price, serde_json::Error> {
    serde_json::from_str(json)
}

#[test]
fn v47_trailing_zeros_are_dropped_as_for_quantities() {
    let padded = read(r#"{"rate":"0.0470","fee":"30.00"}"#).unwrap();
    let canonical = read(r#"{"rate":"0.047","fee":"30"}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&padded).unwrap(),
        serde_json::to_string(&canonical).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&padded).unwrap(),
        r#"{"rate":"0.047","fee":"30"}"#
    );
}

#[test]
fn v47_other_spellings_and_numbers_are_refused() {
    for text in ["1e1", "+1", "01", "-0", "-0.00", "", ".5", "1."] {
        let json = format!(r#"{{"rate":"{text}","fee":null}}"#);
        assert!(read(&json).is_err(), "{text:?}");
    }
    let number = read(r#"{"rate":0.047,"fee":null}"#).unwrap_err();
    assert!(number.to_string().contains(REQUIRES_STRING), "{number}");
}

#[test]
fn a_decimal_beyond_28_digits_has_no_text() {
    let price = Price {
        rate: Decimal::MAX,
        fee: None,
    };
    assert!(serde_json::to_string(&price).is_err());
    assert!(read(r#"{"rate":"79228162514264337593543950335","fee":null}"#).is_err());
}

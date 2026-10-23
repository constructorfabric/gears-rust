#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::exact::REQUIRES_STRING;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Price {
    #[serde(with = "super")]
    rate: Decimal,
    #[serde(default, with = "super::option")]
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
fn an_absent_optional_decimal_reads_as_none() {
    assert_eq!(
        read(r#"{"rate":"1"}"#).unwrap(),
        read(r#"{"rate":"1","fee":null}"#).unwrap()
    );
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

proptest::proptest! {
    /// Any decimal of at most 28 digits writes canonical text that reads back as the same value
    /// and the same text, with or without trailing zeros on input.
    #[test]
    fn canonical_text_round_trips(mantissa in proptest::num::i64::ANY, scale in 0_u32..=18) {
        let value = Decimal::new(mantissa, scale);
        let price = Price { rate: value, fee: Some(value) };
        let text = serde_json::to_string(&price).unwrap();
        let reread = read(&text).unwrap();
        proptest::prop_assert_eq!(reread.rate, value);
        proptest::prop_assert_eq!(serde_json::to_string(&reread).unwrap(), text);
        let canonical = value.normalize().to_string();
        let zeros = if canonical.contains('.') { "000" } else { ".000" };
        let padded = serde_json::json!({"rate": format!("{canonical}{zeros}"), "fee": null});
        proptest::prop_assert_eq!(read(&padded.to_string()).unwrap().rate, value);
    }

    /// No text makes the parser panic.
    #[test]
    fn any_text_is_read_or_refused(text in ".{0,64}") {
        let json = serde_json::json!({"rate": text, "fee": null}).to_string();
        drop(read(&json));
    }
}

use std::str::FromStr;

use bigdecimal::BigDecimal;
use chrono::TimeZone;
use rust_decimal::Decimal;

use toolkit_odata::filter::ODataValue;

use super::{SqlBind, bind_one, odata_value_to_bind};

#[test]
fn datetime64_micros_uses_explicit_epoch_conversion() {
    let bind = SqlBind::DateTime64Micros(1_786_359_600_000_000);

    assert_eq!(bind.placeholder(), "fromUnixTimestamp64Micro(?)");
}

#[test]
fn non_datetime_bind_uses_plain_placeholder() {
    let bind = SqlBind::Str("value".to_owned());

    assert_eq!(bind.placeholder(), "?");
}

fn dt_val() -> ODataValue {
    ODataValue::DateTime(chrono::Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap())
}

#[test]
fn number_converts_to_decimal_bind() {
    let v =
        odata_value_to_bind(&ODataValue::Number(BigDecimal::from_str("42.5").unwrap())).unwrap();
    assert!(matches!(v, SqlBind::Decimal(d) if d.to_string() == "42.5"));
}

#[test]
fn datetime_converts_to_epoch_micros_bind() {
    let v = odata_value_to_bind(&dt_val()).unwrap();
    assert!(matches!(v, SqlBind::DateTime64Micros(_)));
}

#[test]
fn string_converts_to_str_bind() {
    let v = odata_value_to_bind(&ODataValue::String("active".to_owned())).unwrap();
    assert!(matches!(v, SqlBind::Str(s) if s == "active"));
}

#[test]
fn null_and_date_and_time_values_are_rejected() {
    assert!(odata_value_to_bind(&ODataValue::Null).is_err());
    assert!(
        odata_value_to_bind(&ODataValue::Date(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap()
        ))
        .is_err()
    );
    assert!(
        odata_value_to_bind(&ODataValue::Time(
            chrono::NaiveTime::from_hms_opt(1, 2, 3).unwrap()
        ))
        .is_err()
    );
}

#[test]
fn bool_and_uuid_values_convert_to_their_binds() {
    assert!(matches!(
        odata_value_to_bind(&ODataValue::Bool(true)).unwrap(),
        SqlBind::Bool(true)
    ));
    let u = uuid::Uuid::from_u128(0x1234);
    assert!(matches!(
        odata_value_to_bind(&ODataValue::Uuid(u)).unwrap(),
        SqlBind::Uuid(got) if got == u
    ));
}

#[test]
fn numeric_out_of_decimal_range_is_rejected() {
    // 40-digit integer: well past rust_decimal::Decimal's 96-bit mantissa, so
    // the `BigDecimal` -> `Decimal` conversion must surface an error rather than
    // silently truncate.
    let huge = BigDecimal::from_str("1000000000000000000000000000000000000000").unwrap();
    assert!(odata_value_to_bind(&ODataValue::Number(huge)).is_err());
}

#[test]
fn numeric_with_precision_beyond_scale_9_is_rejected() {
    // Ten fractional digits, the last non-zero: no stored Decimal128(9) value
    // can equal it, so the literal is refused here instead of being coerced
    // server-side.
    let fine = BigDecimal::from_str("1.1234567891").unwrap();
    let err = odata_value_to_bind(&ODataValue::Number(fine)).unwrap_err();
    assert!(
        err.contains("Decimal128(9)"),
        "rejection must name the column's scale, got: {err}"
    );
}

#[test]
fn numeric_with_trailing_zeros_beyond_scale_9_is_accepted() {
    // 1.1000000000 is exactly 1.1: nothing is lost at scale 9.
    let v = odata_value_to_bind(&ODataValue::Number(
        BigDecimal::from_str("1.1000000000").unwrap(),
    ))
    .unwrap();
    assert!(matches!(v, SqlBind::Decimal(d) if d == Decimal::new(11, 1)));
}

#[test]
fn numeric_at_exactly_scale_9_is_accepted() {
    let v = odata_value_to_bind(&ODataValue::Number(
        BigDecimal::from_str("0.123456789").unwrap(),
    ))
    .unwrap();
    assert!(matches!(v, SqlBind::Decimal(d) if d.to_string() == "0.123456789"));
}

#[test]
fn bind_one_accepts_every_sql_bind_variant() {
    // Construction-only: no network I/O. Exercises each `bind_one` match arm so
    // llvm-cov counts the dialect wiring.
    let client = clickhouse::Client::default();
    let u = uuid::Uuid::from_u128(0xabcd);
    let d = Decimal::from_str("1.25").unwrap();

    let mut q = client.query("SELECT ?, ?, ?, ?, ?, ?, ?");
    for bind in [
        SqlBind::Uuid(u),
        SqlBind::Str("x".to_owned()),
        SqlBind::Decimal(d),
        SqlBind::DateTime64Micros(1),
        SqlBind::I64(-7),
        SqlBind::Bool(false),
        SqlBind::U64(9),
    ] {
        q = bind_one(q, &bind);
    }
    // Binding every arm without panicking is the assertion; the fully bound
    // query is dropped unsent.
    let _bound = q;
}

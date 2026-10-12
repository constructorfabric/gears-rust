//! Domain fixtures, isolated from production.
use rust_decimal::Decimal;
use std::str::FromStr;
use time::Date;
pub fn date(s: &str) -> Date {
    Date::parse(s, &time::format_description::well_known::Iso8601::DATE).unwrap()
}
pub fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

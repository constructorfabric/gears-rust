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
/// The currency spec of `code` at `scale`.
pub fn spec(code: &str, scale: u8) -> bss_money::CurrencySpec {
    bss_money::CurrencySpec::try_new(code.to_owned(), scale).unwrap()
}

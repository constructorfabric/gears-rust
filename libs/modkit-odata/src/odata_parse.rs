use crate::odata_filters::{CompareOperator, Expr, ParseError, Value};
use bigdecimal::BigDecimal;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, Utc};
use std::str::FromStr;
use uuid::Uuid;

/// Parses an `OData` v4 `$filter` expression string into an `Expr` AST.
///
/// A result containing the parsed `Expr` on success, or an `Error` on failure.
///
/// # Errors
/// Returns `ParseError::Parsing` if the filter string is malformed.
///
/// ```rust,ignore
/// use modkit_odata::odata_filters::parse_str;
///
/// let filter = "name eq 'John' and isActive eq true";
/// let result = parse_str(filter).expect("valid filter tree");
/// ```
pub fn parse_str(query: impl AsRef<str>) -> Result<Expr, ParseError> {
    match odata_filter::parse_str(query.as_ref().trim()) {
        Ok(expr) => expr,
        Err(error) => Err(ParseError::Parsing(error.to_string())),
    }
}

enum AfterValueExpr {
    Compare(CompareOperator, Box<Expr>),
    In(Vec<Expr>),
    End,
}

peg::parser! {
    /// Parses `OData` v4 `$filter` expressions.
    grammar odata_filter() for str {
        use super::{Expr, CompareOperator, Value, ParseError};

        /// Entry point for parsing a filter expression string.
        pub(super) rule parse_str() -> Result<Expr, ParseError>
            = filter()

        /// Parses a filter expression (alias for `or_expr`).
        rule filter() -> Result<Expr, ParseError>
            = or_expr()

        /// Parses a left-associative chain of `and_expr` separated by "or".
        rule or_expr() -> Result<Expr, ParseError>
            = l:and_expr() rest:(_ "or" _ r:and_expr() { r })* {
                rest.into_iter().fold(l, |acc, r| Ok(Expr::Or(Box::new(acc?), Box::new(r?))))
            }

        /// Parses a left-associative chain of `unary_expr` separated by "and".
        rule and_expr() -> Result<Expr, ParseError>
            = l:unary_expr() rest:(_ "and" _ r:unary_expr() { r })* {
                rest.into_iter().fold(l, |acc, r| Ok(Expr::And(Box::new(acc?), Box::new(r?))))
            }

        /// Parses "not" prefix or falls through to a primary expression.
        rule unary_expr() -> Result<Expr, ParseError>
            = "not" _ e:unary_expr() { Ok(Expr::Not(Box::new(e?))) }
            / any_expr()

        /// Parses any expression, including grouped expressions and value expressions.
        rule any_expr() -> Result<Expr, ParseError>
            = "(" _ e:filter() _ ")" { e }
            / l:value_expr() _ r:after_value_expr() { Ok(match r? {
                AfterValueExpr::Compare(op, r) => Expr::Compare(Box::new(l?), op, r),
                AfterValueExpr::In(r) => Expr::In(Box::new(l?), r),
                AfterValueExpr::End => l?,
            }) }

        /// Parses an expression that comes after a value.
        rule after_value_expr() -> Result<AfterValueExpr, ParseError>
            = op:comparison_op() _ r:value_expr() { Ok(AfterValueExpr::Compare(op, Box::new(r?))) }
            / "in" _ "(" _ r:filter_list() _ ")" { Ok(AfterValueExpr::In(r?)) }
            / { Ok(AfterValueExpr::End) }

        /// Parses a value expression, which can be a function call, a value, or an identifier.
        rule value_expr() -> Result<Expr, ParseError>
            = function_call()
            / v:value() { Ok(Expr::Value(v?)) }
            / i:identifier() { Ok(Expr::Identifier(i)) }

        /// Parses a comparison operator.
        rule comparison_op() -> CompareOperator
            = "eq" { CompareOperator::Equal }
            / "ne" { CompareOperator::NotEqual }
            / "gt" { CompareOperator::GreaterThan }
            / "ge" { CompareOperator::GreaterOrEqual }
            / "lt" { CompareOperator::LessThan }
            / "le" { CompareOperator::LessOrEqual }

        /// Parses a function call with a name and arguments.
        rule function_call() -> Result<Expr, ParseError>
            = f:identifier() _ "(" _ l:filter_list() _ ")" { Ok(Expr::Function(f, l?)) }

        /// Parses an identifier.
        rule identifier() -> String
            = s:$(['a'..='z'|'A'..='Z'|'_']['a'..='z'|'A'..='Z'|'_'|'0'..='9']*) { s.to_owned() }

        /// Parses a value, which can be a string, datetime, date, time, number, boolean, or null.
        rule value() -> Result<Value, ParseError>
            = string_value()
            / datetime_value()
            / date_value()
            / time_value()
            / uuid_value()
            / number_value()
            / v:bool_value() { Ok(v) }
            / v:null_value() { Ok(v) }

        /// Parses a boolean value.
        rule bool_value() -> Value
            = ['t'|'T']['r'|'R']['u'|'U']['e'|'E'] { Value::Bool(true) }
            / ['f'|'F']['a'|'A']['l'|'L']['s'|'S']['e'|'E'] { Value::Bool(false) }

        /// Parses a numeric value.
        rule number_value() -> Result<Value, ParseError>
            = n:$(['+' | '-']? ['0'..='9']+ ("." ['0'..='9']*)?) { Ok(Value::Number(BigDecimal::from_str(n).map_err(|_| ParseError::ParsingNumber)?)) }

        /// Parses a uuid value.
        rule uuid_value() -> Result<Value, ParseError>
            = id:$(hex()*<8> "-" hex()*<4> "-" hex()*<4> "-" hex()*<4> "-" hex()*<12> ) { Ok(Value::Uuid(Uuid::parse_str(id).map_err(|_| ParseError::ParsingUuid)?)) }

        /// Parses a single hexadecimal digit.
        rule hex() -> char
            = ['0'..='9'|'a'..='f'|'A'..='F']

        /// Parses a time value in the format `HH:MM:SS` or `HH:MM`.
        rule time() -> Result<NaiveTime, ParseError>
            = hm:$($(['0'..='9']*<1,2>) ":" $(['0'..='9']*<2>)) s:$(":" $(['0'..='9']*<2>))? ms:$("." $(['0'..='9']*<1,9>))? {
                match (s, ms) {
                    (Some(s), Some(ms)) => NaiveTime::parse_from_str(&format!("{hm}{s}{ms}"), "%H:%M:%S%.f"),
                    (Some(s), None) => NaiveTime::parse_from_str(&format!("{hm}{s}"), "%H:%M:%S"),
                    (None, _) => NaiveTime::parse_from_str(hm, "%H:%M"),
                }.map_err(|_| ParseError::ParsingTime)
            }

        /// Parses a time value.
        rule time_value() -> Result<Value, ParseError>
            = t:time() { Ok(Value::Time(t?)) }

        /// Parses a date value in the format `YYYY-MM-DD`.
        rule date() -> Result<NaiveDate, ParseError>
            = d:$($(['0'..='9']*<4>) "-" $(['0'..='9']*<2>) "-" $(['0'..='9']*<2>)) { NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| ParseError::ParsingDate) }

        /// Parses a date value.
        rule date_value() -> Result<Value, ParseError>
            = d:date() { Ok(Value::Date(d?)) }

        /// Parses a named timezone.
        rule timezone_name() -> Result<chrono_tz::Tz, ParseError>
            = z:$(['a'..='z'|'A'..='Z'|'-'|'_'|'/'|'+']['a'..='z'|'A'..='Z'|'-'|'_'|'/'|'+'|'0'..='9']+) { z.parse::<chrono_tz::Tz>().map_err(|_| ParseError::ParsingTimeZoneNamed) }

        /// Parses a timezone offset.
        rule timezone_offset() -> Result<FixedOffset, ParseError>
            = "Z" { "+0000".parse().map_err(|_| ParseError::ParsingTimeZone) }
            / z:$($(['-'|'+']) $(['0'..='9']*<2>) ":"? $(['0'..='9']*<2>)) { z.parse().map_err(|_| ParseError::ParsingTimeZone) }
            / z:$($(['-'|'+']) $(['0'..='9']*<2>)) { format!("{z}00").parse().map_err(|_| ParseError::ParsingTimeZone) }

        /// Parses a datetime value in the format `YYYY-MM-DDTHH:MM:SSZ` or `YYYY-MM-DDTHH:MM:SS+01:00`.
        rule datetime() -> Result<DateTime<Utc>, ParseError>
            = d:date() "T" t:time() z:timezone_offset() { Ok(d?.and_time(t?).and_local_timezone(z?).earliest().ok_or(ParseError::ParsingDateTime)?.to_utc()) }
            / d:date() "T" t:time() z:timezone_name() { Ok(d?.and_time(t?).and_local_timezone(z?).earliest().ok_or(ParseError::ParsingDateTime)?.to_utc()) }

        /// Parses a datetime value.
        rule datetime_value() -> Result<Value, ParseError>
            = dt:datetime() { Ok(Value::DateTime(dt?)) }

        /// Parses a string value enclosed in single quotes.
        rule string_value() -> Result<Value, ParseError>
            = "'" s:quote_escaped_string_content()* "'" { Ok(Value::String(s.into_iter().collect::<Result<Vec<_>, _>>()?.into_iter().collect())) }

        rule quote_escaped_string_content() -> Result<char, ParseError>
            = "''" { Ok('\'') }
            / c:[^'\''] { Ok(c) }

        /// Parses a null value.
        rule null_value() -> Value
            = ['n'|'N']['u'|'U']['l'|'L']['l'|'L'] { Value::Null }

        /// Parses a list of value expressions separated by commas.
        rule value_list() -> Result<Vec<Expr>, ParseError>
            = v:value_expr() ** ( _ "," _ ) { v.into_iter().collect() }

        /// Parses a list of filter expressions separated by commas.
        rule filter_list() -> Result<Vec<Expr>, ParseError>
            = v:filter() ** ( _ "," _ ) { v.into_iter().collect() }

        /// Matches zero or more whitespace characters.
        rule _()
            = [' '|'\t'|'\n'|'\r']*
    }
}

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use thiserror::Error;
use uuid::Uuid;

pub use crate::odata_parse::parse_str;

/// Represents various errors that can occur during parsing.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// Error during general parsing.
    #[error("Error during general parsing: {0}")]
    Parsing(String),

    /// Error parsing a UUID.
    #[error("Error parsing a UUID.")]
    ParsingUuid,

    /// Error parsing a number.
    #[error("Error parsing a number.")]
    ParsingNumber,

    /// Error parsing a date.
    #[error("Error parsing a date.")]
    ParsingDate,

    /// Error parsing a time.
    #[error("Error parsing a time.")]
    ParsingTime,

    /// Error parsing a datetime.
    #[error("Error parsing a date and time.")]
    ParsingDateTime,

    /// Error parsing a time zone offset.
    #[error("Error parsing a time zone offset.")]
    ParsingTimeZone,

    /// Error parsing a named time zone.
    #[error("Error parsing a named time zone.")]
    ParsingTimeZoneNamed,
}

/// Represents the different types of expressions in the AST.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// Logical OR between two expressions.
    Or(Box<Expr>, Box<Expr>),

    /// Logical AND between two expressions.
    And(Box<Expr>, Box<Expr>),

    /// Logical NOT to invert an expression.
    Not(Box<Expr>),

    /// Comparison between two expressions.
    Compare(Box<Expr>, CompareOperator, Box<Expr>),

    /// In operator to check if a value is within a list of values.
    In(Box<Expr>, Vec<Expr>),

    /// Function call with a name and a list of arguments.
    Function(String, Vec<Expr>),

    /// An identifier.
    Identifier(String),

    /// A constant value.
    Value(Value),
}

/// Represents the various comparison operators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareOperator {
    /// Equal to.
    Equal,

    /// Not equal to.
    NotEqual,

    /// Greater than.
    GreaterThan,

    /// Greater than or equal to.
    GreaterOrEqual,

    /// Less than.
    LessThan,

    /// Less than or equal to.
    LessOrEqual,
}

/// Represents the various value types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// Null value.
    Null,

    /// Boolean value.
    Bool(bool),

    /// Numeric value.
    Number(BigDecimal),

    /// Unique ID sometimes referred to as GUIDs.
    Uuid(Uuid),

    /// Date and time with time zone value.
    DateTime(DateTime<Utc>),

    /// Date value.
    Date(NaiveDate),

    /// Time value.
    Time(NaiveTime),

    /// String value.
    String(String),
}

//! The one error type the AST and the DDL builders share.
//!
//! Its own module so that `ast` and `ddl` stay independent of each other: both
//! need this type, neither needs the other.

/// Something the AST refuses to render.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PgqError {
    /// A pattern with no `COLUMNS` produces no output columns, so the enclosing
    /// query has nothing to select.
    #[error("a GRAPH_TABLE needs at least one projected column")]
    NoColumns,
    /// An identifier that cannot name anything.
    #[error("{what} must not be empty")]
    EmptyIdentifier {
        /// Which identifier was empty.
        what: &'static str,
    },
    /// Two elements of one pattern share a variable, so a predicate on that
    /// variable would be ambiguous.
    #[error("element variable `{0}` is used twice in one pattern")]
    DuplicateVariable(String),
}

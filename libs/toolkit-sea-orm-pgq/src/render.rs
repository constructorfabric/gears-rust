//! Rendering the AST into SQL.
//!
//! Two invariants live here, and both are pinned by tests:
//!
//! * every identifier goes through `sea_query`'s own quoting, never `format!`;
//! * every value travels as a bound parameter, never as text.
//!
//! The construct is written into one [`SqlWriterValues`], so placeholders are
//! numbered continuously across all the element predicates rather than each
//! condition restarting at `$1` and needing to be renumbered afterwards.

// Writing into an in-memory buffer cannot fail, so every `write_str` here
// returns an `Ok` nobody can act on. `SqlWriterValues` offers only
// `std::fmt::Write`, so the `Result` cannot be avoided at the call site — and
// `unwrap` on it would be noise claiming a failure mode that does not exist.
#![allow(clippy::let_underscore_must_use)]

use std::collections::BTreeSet;
use std::fmt::Write as _;

use sea_orm::sea_query::{
    Alias, Expr, Func, IntoIden, PostgresQueryBuilder, QueryBuilder, QuotedBuilder,
    SqlWriterValues, TableRef,
};
use sea_orm::{Condition, Value};

use crate::ast::{Direction, Element, GraphTable, ProjectedColumn};
use crate::error::PgqError;

/// The construct's name. Rendered raw rather than as an identifier, because it
/// is a keyword: quoting it would make the statement a syntax error.
const GRAPH_TABLE: &str = "GRAPH_TABLE";

/// Write `name` as one quoted, escaped identifier.
///
/// Goes through `QuotedBuilder::prepare_iden`, which is the same path every
/// identifier in a `sea_query` statement takes. `Alias::new` on a runtime string
/// always takes the escaping branch, so a name containing the quote character
/// comes out with it doubled rather than closing the identifier early.
fn write_ident(out: &mut impl std::fmt::Write, name: &str) {
    let mut buffer = String::new();
    PostgresQueryBuilder.prepare_iden(&Alias::new(name).into_iden(), &mut buffer);
    // `prepare_iden` writes into a `SqlWriter`; `String` is one, and it cannot
    // fail, so the intermediate buffer is only about satisfying that bound.
    let _ = out.write_str(&buffer);
}

/// Write a condition into the shared writer, so its values join the same list.
fn write_condition(writer: &mut SqlWriterValues, condition: &Condition) {
    PostgresQueryBuilder.prepare_condition_where(condition, writer);
}

/// Refuse an identifier that cannot name anything.
fn require_named(value: &str, what: &'static str) -> Result<(), PgqError> {
    if value.trim().is_empty() {
        return Err(PgqError::EmptyIdentifier { what });
    }
    Ok(())
}

/// Write one element: `(var IS label WHERE …)` or `[var IS label WHERE …]`.
fn write_element(
    writer: &mut SqlWriterValues,
    element: &Element,
    brackets: (char, char),
) -> Result<(), PgqError> {
    require_named(element.variable(), "an element variable")?;
    require_named(element.label(), "an element label")?;

    let _ = writer.write_char(brackets.0);
    write_ident(writer, element.variable());
    let _ = writer.write_str(" IS ");
    write_ident(writer, element.label());
    if let Some(filter) = element.filter() {
        let _ = writer.write_str(" WHERE ");
        write_condition(writer, filter);
    }
    let _ = writer.write_char(brackets.1);
    Ok(())
}

/// Write the `COLUMNS` clause.
fn write_columns(
    writer: &mut SqlWriterValues,
    columns: &[ProjectedColumn],
) -> Result<(), PgqError> {
    if columns.is_empty() {
        return Err(PgqError::NoColumns);
    }
    let _ = writer.write_str(" COLUMNS (");
    for (index, column) in columns.iter().enumerate() {
        require_named(&column.variable, "a projected variable")?;
        require_named(&column.property, "a projected property")?;
        require_named(&column.alias, "a projected column alias")?;
        if index > 0 {
            let _ = writer.write_str(", ");
        }
        write_ident(writer, &column.variable);
        let _ = writer.write_char('.');
        write_ident(writer, &column.property);
        let _ = writer.write_str(" AS ");
        write_ident(writer, &column.alias);
    }
    let _ = writer.write_char(')');
    Ok(())
}

/// Reject a pattern that binds one variable twice.
///
/// A predicate on such a variable would be ambiguous, and for `toolkit-db` it
/// would also make "scope is attached per element" untrue: two elements would
/// share one qualifier.
fn require_distinct_variables(table: &GraphTable) -> Result<(), PgqError> {
    let mut seen = BTreeSet::new();
    for element in table.pattern.elements() {
        if !seen.insert(element.variable()) {
            return Err(PgqError::DuplicateVariable(element.variable().to_owned()));
        }
    }
    Ok(())
}

/// Render the inside of `GRAPH_TABLE ( … )` and the values it binds.
pub fn body(table: &GraphTable) -> Result<(String, Vec<Value>), PgqError> {
    require_named(&table.graph, "a property graph name")?;
    require_distinct_variables(table)?;

    // `$` numbered: the same placeholder style `PostgresQueryBuilder` produces,
    // and the numbering continues across every element predicate because they
    // all write into this one writer.
    let mut writer = SqlWriterValues::new("$", true);

    write_ident(&mut writer, &table.graph);
    let _ = writer.write_str(" MATCH ");
    write_element(&mut writer, &table.pattern.head, ('(', ')'))?;

    for hop in &table.pattern.hops {
        // The arrow is always explicit. There is no undirected form to fall
        // back to, by design — see the crate documentation.
        let (before, after) = match hop.direction {
            Direction::Outgoing => ("-", "->"),
            Direction::Incoming => ("<-", "-"),
        };
        let _ = writer.write_str(before);
        write_element(&mut writer, &hop.edge, ('[', ']'))?;
        let _ = writer.write_str(after);
        write_element(&mut writer, &hop.target, ('(', ')'))?;
    }

    write_columns(&mut writer, &table.columns)?;

    let (sql, values) = writer.into_parts();
    Ok((sql, values.into_iter().collect()))
}

/// Render the construct as an aliased `FROM` item.
pub fn table_ref(table: &GraphTable, alias: &str) -> Result<TableRef, PgqError> {
    require_named(alias, "a GRAPH_TABLE alias")?;
    let (sql, values) = body(table)?;

    // `Func::cust` renders its name raw, which is what a keyword needs, and the
    // single argument becomes the parenthesised body. The values ride along as
    // bound parameters: `cust_with_values` substitutes each `$n` with a
    // placeholder rather than with the value's text.
    let call = Func::cust(Alias::new(GRAPH_TABLE)).arg(Expr::cust_with_values(sql, values));
    Ok(TableRef::FunctionCall(call, Alias::new(alias).into_iden()))
}

impl GraphTable {
    /// Render as a `FROM` item aliased `alias`.
    ///
    /// # Errors
    /// Returns [`PgqError`] for a construct that cannot name anything: no
    /// projected columns, an empty identifier, or a variable used twice.
    pub fn into_table_ref(self, alias: &str) -> Result<TableRef, PgqError> {
        table_ref(&self, alias)
    }

    /// Render the construct's SQL and bound values, without executing it.
    ///
    /// Exposed for tests: assertions belong on rendered SQL, because a predicate
    /// that never reaches the database would satisfy a `Debug`-form assertion
    /// and still leak.
    ///
    /// # Errors
    /// As [`Self::into_table_ref`].
    pub fn render_for_test(&self) -> Result<(String, Vec<Value>), PgqError> {
        body(self)
    }
}

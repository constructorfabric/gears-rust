//! The `archived` term of a list's `$filter` (products P-D-263, pricing D-522).
//!
//! An archive mark is a nullable column (`archived_at`), and a list hides the archived rows unless
//! it is asked for them. The toolkit's pager maps one filter field to one column, so it cannot say
//! "the mark is set" for a boolean field. A list therefore takes its top-level `archived` terms out
//! of the `$filter` with [`take_archived`] before the pager reads the rest, and applies the answer
//! as its own condition on the mark.

use toolkit_odata::ast::{CompareOperator, Expr, Value};

/// The filter field every BSS list publishes for the archive mark.
pub const ARCHIVED: &str = "archived";

/// Why an `archived` term is refused: 400 on every list that takes the field.
pub const ARCHIVED_FILTER_REFUSED: &str = "an `archived` comparison is `eq` or `ne` with true or \
     false, joined only by top-level `and`; an `archived` term under `or` or `not`, or any other \
     use of `archived`, is refused";

/// Which rows a list keeps by their archive mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archived {
    /// The rows that are not archived: the default, and `archived eq false`.
    Hidden,
    /// Only the archived rows: `archived eq true`.
    Only,
    /// No row: two top-level terms that disagree (`archived eq true and archived eq false`).
    Neither,
}

/// Take the top-level `archived` comparisons out of `expr`: `eq` or `ne` against `true` or
/// `false`, alone or joined to the rest by `and`. The answer is the rest of the filter (`None`
/// when nothing is left) and the rows the terms keep; no term keeps [`Archived::Hidden`].
///
/// # Errors
/// [`ARCHIVED_FILTER_REFUSED`] for an `archived` term under `or` or `not`, inside a function or an
/// `in`, compared with an operator other than `eq` and `ne`, or with a value that is not a
/// boolean.
pub fn take_archived(expr: Expr) -> Result<(Option<Expr>, Archived), String> {
    let mut wanted: Vec<bool> = Vec::new();
    let rest = take(expr, &mut wanted)?;
    let archived = match (wanted.contains(&true), wanted.contains(&false)) {
        (true, true) => Archived::Neither,
        (true, false) => Archived::Only,
        (false, _) => Archived::Hidden,
    };
    Ok((rest, archived))
}

/// [`take_archived`] over an optional filter: no filter keeps the default.
///
/// # Errors
/// As [`take_archived`].
pub fn take_archived_opt(expr: Option<Expr>) -> Result<(Option<Expr>, Archived), String> {
    match expr {
        Some(expr) => take_archived(expr),
        None => Ok((None, Archived::Hidden)),
    }
}

fn take(expr: Expr, wanted: &mut Vec<bool>) -> Result<Option<Expr>, String> {
    match expr {
        Expr::And(left, right) => {
            let left = take(*left, wanted)?;
            let right = take(*right, wanted)?;
            Ok(match (left, right) {
                (Some(left), Some(right)) => Some(Expr::And(Box::new(left), Box::new(right))),
                (Some(one), None) | (None, Some(one)) => Some(one),
                (None, None) => None,
            })
        }
        Expr::Compare(left, op, right) if is_archived(&left) => {
            let Expr::Value(Value::Bool(value)) = *right else {
                return Err(ARCHIVED_FILTER_REFUSED.to_owned());
            };
            match op {
                CompareOperator::Eq => wanted.push(value),
                CompareOperator::Ne => wanted.push(!value),
                _ => return Err(ARCHIVED_FILTER_REFUSED.to_owned()),
            }
            Ok(None)
        }
        other if names_archived(&other) => Err(ARCHIVED_FILTER_REFUSED.to_owned()),
        other => Ok(Some(other)),
    }
}

fn is_archived(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(name) if name == ARCHIVED)
}

fn names_archived(expr: &Expr) -> bool {
    match expr {
        Expr::Identifier(name) => name == ARCHIVED,
        Expr::Value(_) => false,
        Expr::And(a, b) | Expr::Or(a, b) | Expr::Compare(a, _, b) => {
            names_archived(a) || names_archived(b)
        }
        Expr::Not(inner) => names_archived(inner),
        Expr::In(inner, list) => names_archived(inner) || list.iter().any(names_archived),
        Expr::Function(_, args) => args.iter().any(names_archived),
    }
}

#[cfg(test)]
#[path = "archived_tests.rs"]
mod archived_tests;

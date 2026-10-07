// Created: 2026-09-29 by Virtuozzo International GmbH
//! The `$filter` grammar the settings surfaces share.
//!
//! Browse and search take the same expression — `category_id eq`, `key eq`,
//! `key in (...)` and `needs_review eq true`, joined by `and` — and read it the
//! same way, from one place: two readings of one grammar would drift, and a
//! filter accepted by one list and refused by the other is the defect a shared
//! Filters control cannot work around.

use settings_service_sdk::odata::SettingFilterField;
use toolkit_odata::ast::{CompareOperator, Expr, Value as ODataValue};
use toolkit_odata::filter::convert_expr_to_filter_node;

use crate::domain::error::DomainError;
use crate::field;

/// What a settings filter asks for, once interpreted.
///
/// Browse reads `needs_review` as a switch — the page lists flagged rows
/// instead of resolving — and search as a narrowing: only settings with a
/// flagged override in the corpus. The remainder selects declarations on
/// either.
#[derive(Debug, Default)]
pub(crate) struct SettingFilter {
    /// `needs_review eq true`.
    pub(crate) needs_review: bool,
    /// `key in (…)` or `key eq …`: the named key set, for per-key outcomes.
    pub(crate) keys: Option<Vec<String>>,
    /// The remainder, which selects declarations: `category_id`, `key`.
    pub(crate) declarations: Option<Expr>,
}

pub(crate) fn unsupported(message: impl Into<String>) -> DomainError {
    DomainError::Validation {
        field: "$filter".to_owned(),
        code: field::ODATA_QUERY,
        message: message.into(),
    }
}

/// Interpret a settings filter.
///
/// Every field and operator is checked against the declared surface first, so
/// an unmapped field or an unsupported operator is refused rather than
/// ignored; then the expression is split into what selects declarations and
/// the `needs_review` switch.
pub(crate) fn interpret(filter: Option<&Expr>) -> Result<SettingFilter, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-5
    let Some(expr) = filter else {
        return Ok(SettingFilter::default());
    };
    convert_expr_to_filter_node::<SettingFilterField>(expr)
        .map_err(|e| unsupported(e.to_string()))?;
    let mut out = SettingFilter::default();
    out.declarations = split(expr, &mut out)?;
    Ok(out)
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-5
}

/// Split one conjunction: returns the declaration-selecting remainder.
fn split(expr: &Expr, out: &mut SettingFilter) -> Result<Option<Expr>, DomainError> {
    match expr {
        Expr::And(left, right) => {
            let left = split(left, out)?;
            let right = split(right, out)?;
            Ok(match (left, right) {
                (Some(l), Some(r)) => Some(Expr::And(Box::new(l), Box::new(r))),
                (Some(one), None) | (None, Some(one)) => Some(one),
                (None, None) => None,
            })
        }
        Expr::Compare(left, CompareOperator::Eq, right) => match (&**left, &**right) {
            (Expr::Identifier(name), ODataValueExpr(ODataValue::Bool(flag)))
                if name.eq_ignore_ascii_case("needs_review") =>
            {
                if !flag {
                    return Err(unsupported(
                        "`needs_review eq false` is not a listing; omit the filter to browse",
                    ));
                }
                out.needs_review = true;
                Ok(None)
            }
            (Expr::Identifier(name), ODataValueExpr(ODataValue::String(key)))
                if name.eq_ignore_ascii_case("key") =>
            {
                out.keys = Some(vec![key.clone()]);
                Ok(Some(expr.clone()))
            }
            (Expr::Identifier(name), ODataValueExpr(ODataValue::Uuid(_)))
                if name.eq_ignore_ascii_case("category_id") =>
            {
                Ok(Some(expr.clone()))
            }
            _ => Err(unsupported(
                "only `category_id eq`, `key eq`, `key in (...)` and `needs_review eq true` \
                 are supported, joined by `and`",
            )),
        },
        Expr::In(left, values) => match &**left {
            Expr::Identifier(name) if name.eq_ignore_ascii_case("key") => {
                let keys = values
                    .iter()
                    .filter_map(|v| match v {
                        ODataValueExpr(ODataValue::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                out.keys = Some(keys);
                Ok(Some(expr.clone()))
            }
            _ => Err(unsupported("`in` is supported on `key` only")),
        },
        _ => Err(unsupported(
            "only `category_id eq`, `key eq`, `key in (...)` and `needs_review eq true` \
             are supported, joined by `and`",
        )),
    }
}

use Expr::Value as ODataValueExpr;

#[cfg(test)]
#[path = "setting_filter_tests.rs"]
mod setting_filter_tests;

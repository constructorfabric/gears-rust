//! RG filter normalization. GTS type predicates are matched through the local
//! identity dictionary in SQL.

use sea_orm::sea_query::Expr as SqlExpr;
use sea_orm::{Condition, EntityTrait, ExprTrait};
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, filter_node_to_condition};
use toolkit_db::secure::{Scoped, SecureSelect};
use toolkit_odata::ODataQuery;
use toolkit_odata::ast::{Expr, Value};
use toolkit_odata::filter::{
    FieldKind, FilterField, FilterNode, FilterOp, convert_expr_to_filter_node,
};
use uuid::Uuid;

use super::gts_type_dict;
use crate::domain::error::DomainError;

/// Apply the resolved filter once, preserving the caller's scope and original
/// filter hash for pagination, and clear the AST to prevent a second application.
pub(super) fn prepare_filter<F, M, E>(
    mut select: SecureSelect<E, Scoped>,
    query: &ODataQuery,
    type_field: F,
    bind_budget: usize,
) -> Result<(SecureSelect<E, Scoped>, ODataQuery), DomainError>
where
    F: FilterField,
    M: FieldToColumn<F>,
    E: EntityTrait,
{
    if let Some(ast) = query.filter.as_deref() {
        let validated = validate_filter::<F>(ast)?;
        // Reject an oversized filter before allocating its SQL subqueries.
        if filter_bind_count(&validated) > bind_budget {
            return Err(DomainError::validation(format!(
                "filter exceeds the {bind_budget} bind parameter budget"
            )));
        }
        let condition = filter_to_condition::<F, M>(&validated, type_field)?;
        select = select.filter(condition);
    }
    let mut query = query.clone();
    query.filter = None;
    Ok((select, query))
}

fn filter_bind_count<F>(node: &FilterNode<F>) -> usize
where
    F: FilterField,
{
    match node {
        FilterNode::Binary { .. } => 1,
        FilterNode::InList { values, .. } => values.len(),
        FilterNode::Not(child) => filter_bind_count(child),
        FilterNode::Composite { children, .. } => children.iter().map(filter_bind_count).sum(),
    }
}

/// Accept quoted UUIDs only for UUID fields. Keep the caller's AST and filter
/// hash intact so cursor validation continues to describe the public query.
pub(super) fn validate_filter<F: FilterField>(ast: &Expr) -> Result<FilterNode<F>, DomainError> {
    let mut normalized = ast.clone();
    normalize_uuids::<F>(&mut normalized)?;
    convert_expr_to_filter_node::<F>(&normalized)
        .map_err(|e| DomainError::validation(format!("invalid $filter: {e}")))
}

fn normalize_uuids<F: FilterField>(expr: &mut Expr) -> Result<(), DomainError> {
    match expr {
        Expr::And(left, right) | Expr::Or(left, right) => {
            normalize_uuids::<F>(left)?;
            normalize_uuids::<F>(right)?;
        }
        Expr::Not(inner) => normalize_uuids::<F>(inner)?,
        Expr::Compare(left, _, right) => normalize_uuid_literal::<F>(left, right)?,
        Expr::In(left, values) => {
            for value in values {
                normalize_uuid_literal::<F>(left, value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_uuid_literal<F: FilterField>(
    left: &Expr,
    right: &mut Expr,
) -> Result<(), DomainError> {
    if let Expr::Identifier(name) = left
        && let Some(field) = F::from_name(name)
        && field.kind() == FieldKind::Uuid
        && let Expr::Value(Value::String(raw)) = right
    {
        let uuid = Uuid::parse_str(raw).map_err(|_| {
            DomainError::validation(format!(
                "invalid $filter: field {} expects a valid UUID",
                field.name()
            ))
        })?;
        *right = Expr::Value(Value::Uuid(uuid));
    }
    Ok(())
}

/// Lower GTS type filters (`eq`, `ne` and `in` on strings) through a dictionary
/// subquery. Unknown identifiers contribute no keys, so `eq` and `in` match
/// nothing for them and `ne` matches every row. Non-null keys preserve this
/// behavior in compound predicates. Other fields use the shared lowering.
fn filter_to_condition<F, M>(node: &FilterNode<F>, type_field: F) -> Result<Condition, DomainError>
where
    F: FilterField,
    M: FieldToColumn<F>,
{
    match node {
        FilterNode::Binary { field, op, value } if *field == type_field => {
            let keys = gts_type_dict::keys_subquery([type_path(value, field.name())?.to_owned()]);
            let column = SqlExpr::col(M::map_field(*field));
            match op {
                FilterOp::Eq => Ok(Condition::all().add(column.in_subquery(keys))),
                FilterOp::Ne => Ok(Condition::all().add(column.not_in_subquery(keys))),
                _ => Err(DomainError::validation(format!(
                    "invalid $filter: field {} supports only eq, ne, and in",
                    field.name()
                ))),
            }
        }
        FilterNode::InList { field, values } if *field == type_field => {
            if values.is_empty() {
                return Err(DomainError::validation(
                    "invalid $filter: IN list must not be empty",
                ));
            }
            let paths = values
                .iter()
                .map(|v| type_path(v, field.name()).map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?;
            let column = SqlExpr::col(M::map_field(*field));
            Ok(Condition::all().add(column.in_subquery(gts_type_dict::keys_subquery(paths))))
        }
        FilterNode::Composite { op, children } => {
            let base = match op {
                FilterOp::And => Condition::all(),
                FilterOp::Or => Condition::any(),
                _ => {
                    return Err(DomainError::validation(format!(
                        "invalid $filter: invalid composite operator {op:?}"
                    )));
                }
            };
            children.iter().try_fold(base, |acc, child| {
                Ok(acc.add(filter_to_condition::<F, M>(child, type_field)?))
            })
        }
        FilterNode::Not(inner) => Ok(Condition::all()
            .add(filter_to_condition::<F, M>(inner, type_field)?)
            .not()),
        other => filter_node_to_condition::<F, M>(other)
            .map_err(|e| DomainError::validation(format!("invalid $filter: {e}"))),
    }
}

fn type_path<'a>(value: &'a Value, field: &str) -> Result<&'a str, DomainError> {
    let Value::String(raw) = value else {
        return Err(DomainError::validation(format!(
            "invalid $filter: field {field} expects a GTS type path string"
        )));
    };
    Ok(raw)
}

#[cfg(test)]
#[path = "odata_filter_tests.rs"]
mod tests;

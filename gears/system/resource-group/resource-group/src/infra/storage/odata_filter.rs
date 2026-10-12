//! RG filter normalization and batched GTS resolution.

use std::collections::HashMap;

use resource_group_sdk::GtsTypePath;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, filter_node_to_condition};
use toolkit_db::secure::{DBRunner, ScopeError, Scoped, SecureEntityExt, SecureSelect};
use toolkit_odata::ODataQuery;
use toolkit_odata::ast::{Expr, Value};
use toolkit_odata::filter::{
    FieldKind, FilterField, FilterNode, FilterOp, convert_expr_to_filter_node,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::entity::gts_type::{Column as TypeColumn, Entity as TypeEntity};
use crate::domain::error::DomainError;

/// Apply the resolved filter once, preserving the caller's scope and original
/// filter hash for pagination, and clear the AST to prevent a second application.
pub(super) async fn prepare_filter<F, M, E>(
    db: &impl DBRunner,
    mut select: SecureSelect<E, Scoped>,
    query: &ODataQuery,
    type_field: F,
) -> Result<(SecureSelect<E, Scoped>, ODataQuery), DomainError>
where
    F: FilterField,
    M: FieldToColumn<F>,
    E: EntityTrait,
{
    if let Some(ast) = query.filter.as_deref() {
        let validated = validate_filter::<F>(ast)?;
        let resolved = resolve_type_filter(db, &validated, type_field).await?;
        let condition = filter_node_to_condition::<F, M>(&resolved)
            .map_err(|e| DomainError::validation(format!("invalid $filter: {e}")))?;
        select = select.filter(condition);
    }
    let mut query = query.clone();
    query.filter = None;
    Ok((select, query))
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

/// Resolve the public GTS string field to its numeric storage ID. Ordered and
/// string operations do not have the same meaning on surrogate IDs.
pub(super) async fn resolve_type_filter<F: FilterField>(
    db: &impl DBRunner,
    node: &FilterNode<F>,
    type_field: F,
) -> Result<FilterNode<F>, DomainError> {
    let mut paths = Vec::new();
    collect_type_paths(node, type_field, &mut paths)?;
    if paths.is_empty() {
        return Ok(node.clone());
    }
    paths.sort_unstable();
    paths.dedup();

    // Preserve the existing group-listing bound on query count and bind count.
    // A client can reference many types through IN or nested predicates.
    let scope = AccessScope::allow_all();
    let mut ids = HashMap::new();
    for chunk in paths.chunks(toolkit_db::secure::max_bind_params_for(db)) {
        let rows = TypeEntity::find()
            .filter(TypeColumn::SchemaId.is_in(chunk.to_vec()))
            .secure()
            .scope_with(&scope)
            .all(db)
            .await
            .map_err(|e| match e {
                ScopeError::Db(db) => DomainError::Database(db),
                other => DomainError::database(other.to_string()),
            })?;
        ids.extend(rows.into_iter().map(|row| (row.schema_id, row.id)));
    }
    map_type_values(node, type_field, &mut |value| {
        let raw = type_path(value, type_field.name())?;
        // Registered identifiers are authoritative: type creation historically
        // accepted codes outside the stricter GTS grammar. Keep those records
        // queryable by their exact stored identifier.
        if let Some(id) = ids.get(raw) {
            return Ok(Value::Number((*id).into()));
        }
        if let Err(e) = GtsTypePath::new(raw) {
            return Err(DomainError::validation(format!(
                "invalid $filter: field {}: {e}",
                type_field.name()
            )));
        }
        Err(DomainError::validation(format!(
            "invalid $filter: Unknown type in filter: {raw} (field {})",
            type_field.name()
        )))
    })
}

/// Collect borrowed identifiers and reject unsupported GTS operators before lookup.
fn collect_type_paths<'a, F: FilterField>(
    node: &'a FilterNode<F>,
    type_field: F,
    paths: &mut Vec<&'a str>,
) -> Result<(), DomainError> {
    match node {
        FilterNode::Binary { field, op, value } if *field == type_field => {
            if !matches!(op, FilterOp::Eq | FilterOp::Ne) {
                return Err(DomainError::validation(format!(
                    "invalid $filter: field {} supports only eq, ne, and in",
                    field.name()
                )));
            }
            paths.push(type_path(value, field.name())?);
        }
        FilterNode::InList { field, values } if *field == type_field => {
            for value in values {
                paths.push(type_path(value, field.name())?);
            }
        }
        FilterNode::Composite { children, .. } => {
            for child in children {
                collect_type_paths(child, type_field, paths)?;
            }
        }
        FilterNode::Not(inner) => collect_type_paths(inner, type_field, paths)?,
        _ => {}
    }
    Ok(())
}

/// Visit only GTS predicates, preserving the logical structure and other fields.
fn map_type_values<F: FilterField>(
    node: &FilterNode<F>,
    type_field: F,
    map_value: &mut impl FnMut(&Value) -> Result<Value, DomainError>,
) -> Result<FilterNode<F>, DomainError> {
    Ok(match node {
        FilterNode::Binary { field, op, value } if *field == type_field => FilterNode::Binary {
            field: *field,
            op: *op,
            value: map_value(value)?,
        },
        FilterNode::InList { field, values } if *field == type_field => FilterNode::InList {
            field: *field,
            values: values
                .iter()
                .map(&mut *map_value)
                .collect::<Result<_, _>>()?,
        },
        FilterNode::Composite { op, children } => FilterNode::Composite {
            op: *op,
            children: children
                .iter()
                .map(|child| map_type_values(child, type_field, map_value))
                .collect::<Result<_, _>>()?,
        },
        FilterNode::Not(inner) => {
            FilterNode::Not(Box::new(map_type_values(inner, type_field, map_value)?))
        }
        other => other.clone(),
    })
}

fn type_path<'a>(value: &'a Value, field: &str) -> Result<&'a str, DomainError> {
    let Value::String(raw) = value else {
        return Err(DomainError::validation(format!(
            "invalid $filter: field {field} expects a GTS type path string"
        )));
    };
    // Preserve the original spelling for lookup. Syntax validation happens only
    // if no registered identifier matches it.
    Ok(raw)
}

#[cfg(test)]
#[path = "odata_filter_tests.rs"]
mod tests;

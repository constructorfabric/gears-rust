//! `OData` field mapping of `GET /grants`.

use license_enforcement_sdk::dto::GrantFilterField;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};
use toolkit_odata::ast::Value as ODataValue;
use toolkit_odata::filter::FilterOp;

use super::entity::grant::{Column, Entity, Model};
use crate::domain::types::type_key;

pub struct GrantODataMapper;

impl FieldToColumn<GrantFilterField> for GrantODataMapper {
    type Column = Column;

    fn map_field(field: GrantFilterField) -> Column {
        match field {
            GrantFilterField::Id => Column::Id,
            GrantFilterField::ResourceType => Column::ResourceTypeKey,
            GrantFilterField::State => Column::State,
            GrantFilterField::BindingSubjectType => Column::BindingSubjectTypeKey,
            GrantFilterField::BindingSubjectId => Column::BindingSubjectId,
            GrantFilterField::PackId => Column::PackId,
            GrantFilterField::ParentGrantId => Column::ParentGrantId,
        }
    }

    /// Type filters compare without minor versions, as the check does. Only
    /// `eq`, `ne`, and `in`: a substring of a key has no meaning.
    fn map_value(
        field: GrantFilterField,
        op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        match (field, value) {
            // PostgreSQL does not take a NUL in a text parameter.
            (_, ODataValue::String(s)) if s.contains('\0') => {
                Err("a string value must not contain NUL".to_owned())
            }
            (
                GrantFilterField::ResourceType | GrantFilterField::BindingSubjectType,
                ODataValue::String(s),
            ) => match op {
                FilterOp::Eq | FilterOp::Ne | FilterOp::In => Ok(ODataValue::String(type_key(s))),
                other => Err(format!(
                    "operator {other:?} is not supported on a type; use `eq`, `ne`, or `in`"
                )),
            },
            _ => Ok(value.clone()),
        }
    }

    /// Only the primary key: an order on another column sorts every grant
    /// of the scope on each page.
    fn is_orderable(field: GrantFilterField) -> bool {
        field == GrantFilterField::Id
    }
}

impl ODataFieldMapping<GrantFilterField> for GrantODataMapper {
    type Entity = Entity;

    fn extract_cursor_value(model: &Model, field: GrantFilterField) -> sea_orm::Value {
        match field {
            GrantFilterField::Id => sea_orm::Value::Uuid(Some(model.id)),
            GrantFilterField::ResourceType => {
                sea_orm::Value::String(Some(model.resource_type_key.clone()))
            }
            GrantFilterField::State => sea_orm::Value::String(Some(model.state.clone())),
            GrantFilterField::BindingSubjectType => {
                sea_orm::Value::String(model.binding_subject_type_key.clone())
            }
            GrantFilterField::BindingSubjectId => {
                sea_orm::Value::String(model.binding_subject_id.clone())
            }
            GrantFilterField::PackId => sea_orm::Value::Uuid(model.pack_id),
            GrantFilterField::ParentGrantId => sea_orm::Value::Uuid(model.parent_grant_id),
        }
    }
}

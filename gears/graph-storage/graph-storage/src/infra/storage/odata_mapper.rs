//! The `OData` binding of the node projection.
//!
//! Filtering, ordering and pagination are the platform's: the filter fields
//! are declared once on the row DTO, mapped to columns here, and
//! `paginate_odata` composes them with the `CursorV1` continuation token. The
//! gear invents no second dialect, so `$filter` can never reach a column this
//! mapping does not name.

use sea_orm::Value;
use toolkit_db::odata::{FieldToColumn, ODataFieldMapping};

use crate::infra::storage::entity::node;
use graph_storage_sdk::models::NodeFilterField as Field;

pub struct NodeODataMapper;

impl FieldToColumn<Field> for NodeODataMapper {
    type Column = node::Column;

    fn map_field(field: Field) -> node::Column {
        match field {
            Field::NodeKey => node::Column::NodeKey,
            Field::Name => node::Column::Name,
            Field::CreatedAt => node::Column::CreatedAt,
            Field::UpdatedAt => node::Column::UpdatedAt,
        }
    }
}

impl ODataFieldMapping<Field> for NodeODataMapper {
    type Entity = node::Entity;

    fn extract_cursor_value(model: &node::Model, field: Field) -> Value {
        match field {
            Field::NodeKey => Value::String(Some(model.node_key.clone().into())),
            Field::Name => Value::String(Some(model.name.clone().into())),
            Field::CreatedAt => Value::TimeDateTimeWithTimeZone(Some(model.created_at.into())),
            Field::UpdatedAt => Value::TimeDateTimeWithTimeZone(Some(model.updated_at.into())),
        }
    }
}

//! `OData` field mapping of the bundle listing: the SDK's paging schema
//! ([`BundleFilterField`]) onto the `policy_engine__bundle` columns, plus the
//! cursor values of each field.

use policy_engine_sdk::management::odata::BundleFilterField;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

use super::entity::bundle::{Column, Entity, Model};

/// Maps [`BundleFilterField`] to `policy_engine__bundle`.
pub struct BundleODataMapper;

impl FieldToColumn<BundleFilterField> for BundleODataMapper {
    type Column = Column;

    fn map_field(field: BundleFilterField) -> Column {
        match field {
            BundleFilterField::Id => Column::Id,
        }
    }
}

impl ODataFieldMapping<BundleFilterField> for BundleODataMapper {
    type Entity = Entity;

    fn extract_cursor_value(model: &Model, field: BundleFilterField) -> sea_orm::Value {
        match field {
            BundleFilterField::Id => sea_orm::Value::Uuid(Some(model.id)),
        }
    }
}

//! `OData` field mappings of the listings: the SDK's paging schemas
//! ([`BundleFilterField`], [`VersionFilterField`]) onto the
//! `policy_engine__bundle` and `policy_engine__bundle_version` columns, plus
//! the cursor values of each field.

use policy_engine_sdk::management::odata::{BundleFilterField, VersionFilterField};
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

use super::entity::bundle::{Column, Entity, Model};
use super::entity::bundle_version;

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

/// Maps [`VersionFilterField`] to `policy_engine__bundle_version`.
pub struct VersionODataMapper;

impl FieldToColumn<VersionFilterField> for VersionODataMapper {
    type Column = bundle_version::Column;

    fn map_field(field: VersionFilterField) -> bundle_version::Column {
        match field {
            VersionFilterField::Ordinal => bundle_version::Column::Ordinal,
        }
    }
}

impl ODataFieldMapping<VersionFilterField> for VersionODataMapper {
    type Entity = bundle_version::Entity;

    fn extract_cursor_value(
        model: &bundle_version::Model,
        field: VersionFilterField,
    ) -> sea_orm::Value {
        match field {
            VersionFilterField::Ordinal => sea_orm::Value::BigInt(Some(i64::from(model.ordinal))),
        }
    }
}

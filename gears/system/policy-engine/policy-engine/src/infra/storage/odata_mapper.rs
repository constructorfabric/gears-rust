//! `OData` field mappings of the listings: the SDK's paging schemas
//! ([`BundleFilterField`], [`VersionFilterField`], [`AssignmentFilterField`])
//! onto the `policy_engine__bundle`, `policy_engine__bundle_version` and
//! `policy_engine__assignment` columns, plus the cursor values of each field.

use policy_engine_sdk::management::odata::{
    AssignmentFilterField, BundleFilterField, VersionFilterField,
};
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

use super::entity::bundle::{Column, Entity, Model};
use super::entity::{assignment, bundle_version};

/// Maps [`BundleFilterField`] to `policy_engine__bundle`.
pub struct BundleODataMapper;

impl FieldToColumn<BundleFilterField> for BundleODataMapper {
    type Column = Column;

    fn map_field(field: BundleFilterField) -> Column {
        match field {
            BundleFilterField::Id => Column::Id,
            BundleFilterField::Name => Column::Name,
            BundleFilterField::OwnerTenantId => Column::OwnerTenantId,
        }
    }
}

impl ODataFieldMapping<BundleFilterField> for BundleODataMapper {
    type Entity = Entity;

    fn extract_cursor_value(model: &Model, field: BundleFilterField) -> sea_orm::Value {
        match field {
            BundleFilterField::Id => sea_orm::Value::Uuid(Some(model.id)),
            BundleFilterField::Name => sea_orm::Value::String(Some(model.name.clone())),
            BundleFilterField::OwnerTenantId => sea_orm::Value::Uuid(Some(model.owner_tenant_id)),
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

/// Maps [`AssignmentFilterField`] to `policy_engine__assignment`.
pub struct AssignmentODataMapper;

impl FieldToColumn<AssignmentFilterField> for AssignmentODataMapper {
    type Column = assignment::Column;

    fn map_field(field: AssignmentFilterField) -> assignment::Column {
        match field {
            AssignmentFilterField::Id => assignment::Column::Id,
            AssignmentFilterField::BundleId => assignment::Column::BundleId,
            AssignmentFilterField::TenantId => assignment::Column::TenantId,
        }
    }
}

impl ODataFieldMapping<AssignmentFilterField> for AssignmentODataMapper {
    type Entity = assignment::Entity;

    fn extract_cursor_value(
        model: &assignment::Model,
        field: AssignmentFilterField,
    ) -> sea_orm::Value {
        sea_orm::Value::Uuid(Some(match field {
            AssignmentFilterField::Id => model.id,
            AssignmentFilterField::BundleId => model.bundle_id,
            AssignmentFilterField::TenantId => model.tenant_id,
        }))
    }
}

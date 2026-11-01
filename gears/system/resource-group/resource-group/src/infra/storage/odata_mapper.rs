// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-29 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
//! Infrastructure layer mapping from type-safe `FilterNode` to `SeaORM` Conditions.
//!
//! This gear maps from DTO-level filter fields to `SeaORM` Column types.

use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

use crate::infra::storage::entity::resource_group::{
    Column as GroupColumn, Entity as GroupEntity, Model as GroupModel,
};
use crate::infra::storage::entity::resource_group_membership::{
    Column as MembershipColumn, Entity as MembershipEntity, Model as MembershipModel,
};
use resource_group_sdk::odata::{GroupFilterField, HierarchyFilterField, MembershipFilterField};

/// `OData` mapper for resource groups.
pub struct GroupODataMapper;

impl FieldToColumn<GroupFilterField> for GroupODataMapper {
    type Column = GroupColumn;

    fn map_field(field: GroupFilterField) -> GroupColumn {
        match field {
            GroupFilterField::Type => GroupColumn::GtsTypeId,
            GroupFilterField::HierarchyParentId => GroupColumn::ParentId,
            GroupFilterField::TenantId => GroupColumn::TenantId,
            GroupFilterField::Id => GroupColumn::Id,
            GroupFilterField::Name => GroupColumn::Name,
        }
    }

    /// `type` is a GTS path on the wire but a Registry Reference (UUID) in
    /// storage; neither order is meaningful, so it is not orderable.
    fn is_orderable(field: GroupFilterField) -> bool {
        !matches!(field, GroupFilterField::Type)
    }
}

impl ODataFieldMapping<GroupFilterField> for GroupODataMapper {
    type Entity = GroupEntity;

    fn extract_cursor_value(model: &GroupModel, field: GroupFilterField) -> sea_orm::Value {
        match field {
            GroupFilterField::Id => sea_orm::Value::Uuid(Some(model.id)),
            GroupFilterField::Name => sea_orm::Value::String(Some(model.name.clone())),
            GroupFilterField::HierarchyParentId => match model.parent_id {
                Some(pid) => sea_orm::Value::Uuid(Some(pid)),
                None => sea_orm::Value::Uuid(None),
            },
            GroupFilterField::TenantId => sea_orm::Value::Uuid(Some(model.tenant_id)),
            GroupFilterField::Type => sea_orm::Value::Uuid(Some(model.gts_type_id)),
        }
    }
}

/// `OData` mapper for hierarchy queries (not used for `paginate_odata`; hierarchy
/// queries are handled manually). Included for completeness.
pub struct HierarchyODataMapper;

impl FieldToColumn<HierarchyFilterField> for HierarchyODataMapper {
    type Column = GroupColumn;

    fn map_field(field: HierarchyFilterField) -> GroupColumn {
        match field {
            HierarchyFilterField::HierarchyDepth => GroupColumn::Id, // placeholder
            HierarchyFilterField::Type => GroupColumn::GtsTypeId,
        }
    }

    fn is_orderable(field: HierarchyFilterField) -> bool {
        !matches!(field, HierarchyFilterField::Type)
    }
}

impl ODataFieldMapping<HierarchyFilterField> for HierarchyODataMapper {
    type Entity = GroupEntity;

    fn extract_cursor_value(model: &GroupModel, field: HierarchyFilterField) -> sea_orm::Value {
        match field {
            HierarchyFilterField::HierarchyDepth => sea_orm::Value::Int(None),
            HierarchyFilterField::Type => sea_orm::Value::Uuid(Some(model.gts_type_id)),
        }
    }
}

/// `OData` mapper for memberships.
pub struct MembershipODataMapper;

impl FieldToColumn<MembershipFilterField> for MembershipODataMapper {
    type Column = MembershipColumn;

    fn map_field(field: MembershipFilterField) -> MembershipColumn {
        match field {
            MembershipFilterField::GroupId => MembershipColumn::GroupId,
            MembershipFilterField::ResourceType => MembershipColumn::GtsTypeId,
            MembershipFilterField::ResourceId => MembershipColumn::ResourceId,
        }
    }

    fn is_orderable(field: MembershipFilterField) -> bool {
        !matches!(field, MembershipFilterField::ResourceType)
    }
}

impl ODataFieldMapping<MembershipFilterField> for MembershipODataMapper {
    type Entity = MembershipEntity;

    fn extract_cursor_value(
        model: &MembershipModel,
        field: MembershipFilterField,
    ) -> sea_orm::Value {
        match field {
            MembershipFilterField::GroupId => sea_orm::Value::Uuid(Some(model.group_id)),
            MembershipFilterField::ResourceType => sea_orm::Value::Uuid(Some(model.gts_type_id)),
            MembershipFilterField::ResourceId => {
                sea_orm::Value::String(Some(model.resource_id.clone()))
            }
        }
    }
}

#[cfg(test)]
#[path = "odata_mapper_tests.rs"]
mod tests;

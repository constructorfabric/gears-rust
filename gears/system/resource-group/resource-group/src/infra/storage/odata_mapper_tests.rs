// Created: 2026-04-16 by Constructor Tech
use super::*;

// TC-ODATA-07: GroupODataMapper field -> column
#[test]
fn group_odata_mapper_field_to_column() {
    assert!(
        matches!(
            GroupODataMapper::map_field(GroupFilterField::Type),
            GroupColumn::GtsTypeId
        ),
        "Type -> GtsTypeId"
    );
    assert!(
        matches!(
            GroupODataMapper::map_field(GroupFilterField::HierarchyParentId),
            GroupColumn::ParentId
        ),
        "HierarchyParentId -> ParentId"
    );
    assert!(
        matches!(
            GroupODataMapper::map_field(GroupFilterField::Id),
            GroupColumn::Id
        ),
        "Id -> Id"
    );
    assert!(
        matches!(
            GroupODataMapper::map_field(GroupFilterField::Name),
            GroupColumn::Name
        ),
        "Name -> Name"
    );
}

// TC-ODATA-08a: HierarchyODataMapper field -> column
#[test]
fn hierarchy_odata_mapper_field_to_column() {
    assert!(
        matches!(
            HierarchyODataMapper::map_field(HierarchyFilterField::HierarchyDepth),
            GroupColumn::Id
        ),
        "HierarchyDepth -> Id (placeholder)"
    );
    assert!(
        matches!(
            HierarchyODataMapper::map_field(HierarchyFilterField::Type),
            GroupColumn::GtsTypeId
        ),
        "Type -> GtsTypeId"
    );
}

// TC-ODATA-08: MembershipODataMapper field -> column
#[test]
fn membership_odata_mapper_field_to_column() {
    assert!(
        matches!(
            MembershipODataMapper::map_field(MembershipFilterField::GroupId),
            MembershipColumn::GroupId
        ),
        "GroupId -> GroupId"
    );
    assert!(
        matches!(
            MembershipODataMapper::map_field(MembershipFilterField::ResourceType),
            MembershipColumn::GtsTypeId
        ),
        "ResourceType -> GtsTypeId"
    );
    assert!(
        matches!(
            MembershipODataMapper::map_field(MembershipFilterField::ResourceId),
            MembershipColumn::ResourceId
        ),
        "ResourceId -> ResourceId"
    );
}

// `type` / `resource_type` are Registry References (UUIDs) in storage: no
// meaningful order, so `$orderby` on them must be rejected.
#[test]
fn type_fields_are_not_orderable() {
    assert!(!GroupODataMapper::is_orderable(GroupFilterField::Type));
    assert!(GroupODataMapper::is_orderable(GroupFilterField::Name));
    assert!(!HierarchyODataMapper::is_orderable(
        HierarchyFilterField::Type
    ));
    assert!(!MembershipODataMapper::is_orderable(
        MembershipFilterField::ResourceType
    ));
    assert!(MembershipODataMapper::is_orderable(
        MembershipFilterField::ResourceId
    ));
}

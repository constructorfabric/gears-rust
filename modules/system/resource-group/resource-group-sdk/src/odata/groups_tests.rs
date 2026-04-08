// Created: 2026-04-16 by Constructor Tech
use super::*;

// TC-ODATA-01: GroupFilterField names
#[test]
fn group_filter_field_names_correct() {
    assert_eq!(GroupFilterField::Type.name(), "type");
    assert_eq!(
        GroupFilterField::HierarchyParentId.name(),
        "hierarchy/parent_id"
    );
    assert_eq!(GroupFilterField::Id.name(), "id");
    assert_eq!(GroupFilterField::Name.name(), "name");
}

// TC-ODATA-02: GroupFilterField kinds
#[test]
fn group_filter_field_kinds_correct() {
    assert_eq!(GroupFilterField::Type.kind(), FieldKind::String);
    assert_eq!(GroupFilterField::HierarchyParentId.kind(), FieldKind::Uuid);
    assert_eq!(GroupFilterField::Id.kind(), FieldKind::Uuid);
    assert_eq!(GroupFilterField::Name.kind(), FieldKind::String);
}

// TC-ODATA-03: FIELDS completeness
#[test]
fn group_filter_field_completeness() {
    assert_eq!(GroupFilterField::FIELDS.len(), 4);
}

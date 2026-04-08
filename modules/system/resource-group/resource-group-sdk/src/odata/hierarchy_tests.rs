// Created: 2026-04-16 by Constructor Tech
use super::*;

// TC-ODATA-04: HierarchyFilterField names + kinds
#[test]
fn hierarchy_filter_field_names_and_kinds() {
    assert_eq!(
        HierarchyFilterField::HierarchyDepth.name(),
        "hierarchy/depth"
    );
    assert_eq!(HierarchyFilterField::HierarchyDepth.kind(), FieldKind::I64);

    assert_eq!(HierarchyFilterField::Type.name(), "type");
    assert_eq!(HierarchyFilterField::Type.kind(), FieldKind::String);

    assert_eq!(HierarchyFilterField::FIELDS.len(), 2);
}

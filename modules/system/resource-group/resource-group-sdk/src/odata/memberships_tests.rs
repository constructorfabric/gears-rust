// Created: 2026-04-16 by Constructor Tech
use super::*;

// TC-ODATA-05: MembershipFilterField names + kinds
#[test]
fn membership_filter_field_names_and_kinds() {
    assert_eq!(MembershipFilterField::GroupId.name(), "group_id");
    assert_eq!(MembershipFilterField::GroupId.kind(), FieldKind::Uuid);

    assert_eq!(MembershipFilterField::ResourceType.name(), "resource_type");
    assert_eq!(
        MembershipFilterField::ResourceType.kind(),
        FieldKind::String
    );

    assert_eq!(MembershipFilterField::ResourceId.name(), "resource_id");
    assert_eq!(MembershipFilterField::ResourceId.kind(), FieldKind::String);

    assert_eq!(MembershipFilterField::FIELDS.len(), 3);
}

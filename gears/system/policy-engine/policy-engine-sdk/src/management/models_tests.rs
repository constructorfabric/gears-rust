use super::*;

#[test]
fn version_state_labels() {
    assert_eq!(VersionState::Draft.as_str(), "draft");
    assert_eq!(VersionState::Active.as_str(), "active");
    assert_eq!(VersionState::Superseded.as_str(), "superseded");
}

#[test]
fn assignments_enforce_by_default_and_can_be_shadowed() {
    let spec = AssignmentSpec::new(Uuid::from_u128(1), Uuid::from_u128(2));
    assert!(spec.enforce);
    assert!(!spec.shadow().enforce);
}

#[test]
fn new_bundle_builders() {
    let bundle = NewBundle::new("b")
        .with_owner_tenant(Uuid::from_u128(3))
        .with_description("d");
    assert_eq!(bundle.owner_tenant_id, Some(Uuid::from_u128(3)));
    assert_eq!(bundle.description, "d");
}

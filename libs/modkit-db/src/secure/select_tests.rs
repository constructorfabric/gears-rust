use super::*;
use modkit_security::pep_properties;

// Note: Full integration tests with real SeaORM entities should be written
// in application code where actual entities are available.
// The typestate pattern is enforced at compile time.
//
// See USAGE_EXAMPLE.md for complete usage patterns.

#[test]
fn test_typestate_markers_exist() {
    // This test verifies the typestate markers compile
    // The actual enforcement happens at compile time
    let unscoped = Unscoped;
    assert!(std::mem::size_of_val(&unscoped) == 0); // Unscoped is zero-sized

    // Scoped now requires an AccessScope
    let scope = AccessScope::default();
    let scoped = Scoped {
        scope: Arc::new(scope),
    };
    assert!(!scoped.scope.has_property(pep_properties::OWNER_TENANT_ID)); // default scope has no tenants
}

#[test]
fn test_scoped_state_holds_scope() {
    let tenant_id = uuid::Uuid::new_v4();
    let scope = AccessScope::for_tenants(vec![tenant_id]);
    let scoped = Scoped {
        scope: Arc::new(scope),
    };

    // Verify the scope is accessible
    assert!(scoped.scope.has_property(pep_properties::OWNER_TENANT_ID));
    assert_eq!(
        scoped
            .scope
            .all_values_for(pep_properties::OWNER_TENANT_ID)
            .len(),
        1
    );
    assert!(
        scoped
            .scope
            .all_uuid_values_for(pep_properties::OWNER_TENANT_ID)
            .contains(&tenant_id)
    );
}

#[test]
fn test_scoped_state_is_cloneable() {
    let scope = AccessScope::for_tenants(vec![uuid::Uuid::new_v4()]);
    let scoped = Scoped {
        scope: Arc::new(scope),
    };

    // Cloning should share the Arc
    let cloned = scoped.clone();
    assert!(Arc::ptr_eq(&scoped.scope, &cloned.scope));
}

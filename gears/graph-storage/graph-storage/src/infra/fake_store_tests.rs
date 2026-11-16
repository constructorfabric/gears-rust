use super::scope_admits;
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

#[test]
fn empty_scope_cannot_become_unrestricted_in_the_fake() {
    let tenant = Uuid::from_u128(1);
    let dead = ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
        ScopeFilter::in_uuids(pep_properties::RESOURCE_ID, vec![]),
    ]);
    for live in [false, true] {
        let mut constraints = vec![dead.clone()];
        if live {
            constraints.push(ScopeConstraint::new(vec![ScopeFilter::eq(
                pep_properties::OWNER_TENANT_ID,
                tenant,
            )]));
        }
        assert_eq!(
            scope_admits(&AccessScope::from_constraints(constraints), tenant),
            live
        );
    }
    assert!(scope_admits(&AccessScope::allow_all(), tenant));
}

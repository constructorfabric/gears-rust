#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use policy_engine_sdk::gts::permissions::{Capability, RESOURCE_TYPE};
use uuid::Uuid;

use super::*;
use crate::domain::management::test_support::{FakePdp, PdpMode, ctx};

const ROOT: Uuid = Uuid::from_u128(1);
const CHILD: Uuid = Uuid::from_u128(2);

struct Fixture {
    authz: ManagementAuthorizer,
    pdp: Arc<FakePdp>,
}

fn fixture(pdp: FakePdp) -> Fixture {
    let pdp = Arc::new(pdp);
    let authz = ManagementAuthorizer::new(PolicyEnforcer::new(pdp.clone()));
    Fixture { authz, pdp }
}

#[tokio::test]
async fn exactly_three_capabilities_each_asking_for_its_catalog_action() {
    assert_eq!(Capability::ALL.len(), 3);
    let f = fixture(FakePdp::allow_all());
    let caller = ctx(Uuid::new_v4(), CHILD);
    for capability in Capability::ALL {
        f.authz
            .authorize(&caller, capability, AccessTarget::owned_by(CHILD))
            .await
            .unwrap();
        let seen = f.pdp.seen.lock().last().cloned().unwrap();
        assert_eq!(seen.action, capability.action(), "{capability:?}");
        assert_eq!(seen.resource_type, RESOURCE_TYPE, "{capability:?}");
        assert_eq!(seen.owner, Some(CHILD));
    }
    // The named read entry point asks for the read capability.
    f.authz
        .read(&caller, AccessTarget::owned_by(CHILD))
        .await
        .unwrap();
    assert_eq!(f.pdp.seen.lock().last().unwrap().action, "read");
}

#[tokio::test]
async fn capabilities_are_separate() {
    for granted in Capability::ALL {
        let f = fixture(FakePdp::allow_only(&[granted.action()]));
        let caller = ctx(Uuid::new_v4(), CHILD);
        let owned = AccessTarget::owned_by(CHILD);
        assert!(f.authz.authorize(&caller, granted, owned).await.is_ok());
        for other in Capability::ALL.into_iter().filter(|c| *c != granted) {
            assert_eq!(
                f.authz.authorize(&caller, other, owned).await.unwrap_err(),
                AuthzError::Denied,
                "{other:?} must not follow from {granted:?}"
            );
        }
    }
}

#[tokio::test]
async fn enforcer_outcomes_map_to_denied_and_unavailable() {
    let caller = ctx(Uuid::new_v4(), CHILD);
    let owned = AccessTarget::owned_by(CHILD).resource(Uuid::new_v4());

    let f = fixture(FakePdp::new(PdpMode::Deny));
    assert_eq!(
        f.authz
            .authorize(&caller, Capability::Author, owned)
            .await
            .unwrap_err(),
        AuthzError::Denied
    );

    let f = fixture(FakePdp::new(PdpMode::Fail));
    assert!(matches!(
        f.authz
            .authorize(&caller, Capability::Author, owned)
            .await
            .unwrap_err(),
        AuthzError::Unavailable(_)
    ));

    // A prefetch check of another tenant's content is a denial.
    let f = fixture(FakePdp::allow_all());
    assert_eq!(
        f.authz
            .read(&caller, AccessTarget::owned_by(ROOT).prefetched())
            .await
            .unwrap_err(),
        AuthzError::Denied
    );
    // A constrained grant is confined to the caller's tenant.
    let auth = f
        .authz
        .authorize(&caller, Capability::Author, owned)
        .await
        .unwrap();
    assert!(!auth.scope.is_unconstrained());
}

#[test]
fn a_scope_covers_a_row_only_through_filters_it_can_check() {
    use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};

    let (owner, other, id) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let covers = |scope: AccessScope| Authorization { scope }.covers(owner, id);

    assert!(covers(AccessScope::allow_all()));
    assert!(!covers(AccessScope::deny_all()));
    assert!(covers(AccessScope::for_tenant(owner)));
    assert!(!covers(AccessScope::for_tenant(other)));
    assert!(covers(AccessScope::for_resource(id)));
    // Every filter of a constraint must admit the row.
    assert!(!covers(AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, owner),
        ScopeFilter::eq(pep_properties::RESOURCE_ID, other),
    ]))));
    // Any one constraint suffices.
    assert!(covers(AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![ScopeFilter::eq(
            pep_properties::OWNER_TENANT_ID,
            other
        )]),
        ScopeConstraint::new(vec![ScopeFilter::eq(
            pep_properties::OWNER_TENANT_ID,
            owner
        )]),
    ])));
    // A property the row does not carry, or a filter only the database can
    // resolve, does not admit it.
    assert!(!covers(AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq("owner_id", owner),
    ]))));
    assert!(!covers(AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_tenant_subtree(pep_properties::OWNER_TENANT_ID, owner, true, Vec::new()),
    ]))));
}

#[test]
fn a_scope_covers_a_whole_tenant_only_through_an_owner_filter_alone() {
    use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};

    let (owner, other) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let covers = |scope: AccessScope| Authorization { scope }.covers_tenant(owner);

    assert!(covers(AccessScope::allow_all()));
    assert!(covers(AccessScope::for_tenant(owner)));
    assert!(!covers(AccessScope::for_tenant(other)));
    // A constraint that also narrows by resource identity does not admit
    // every row of the tenant.
    assert!(!covers(AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, owner),
        ScopeFilter::eq(pep_properties::RESOURCE_ID, Uuid::from_u128(3)),
    ]))));
    // Any one qualifying constraint suffices.
    assert!(covers(AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![ScopeFilter::eq(
            pep_properties::OWNER_TENANT_ID,
            other
        )]),
        ScopeConstraint::new(vec![ScopeFilter::eq(
            pep_properties::OWNER_TENANT_ID,
            owner
        )]),
    ])));
}

#[test]
fn an_in_constraint_listing_the_tenant_covers_it() {
    use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};

    let (owner, other) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let scope = |listed: Vec<Uuid>| {
        AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::OWNER_TENANT_ID,
            listed,
        )]))
    };
    assert!(
        Authorization {
            scope: scope(vec![other, owner])
        }
        .covers_tenant(owner)
    );
    assert!(
        !Authorization {
            scope: scope(vec![other])
        }
        .covers_tenant(owner)
    );
}

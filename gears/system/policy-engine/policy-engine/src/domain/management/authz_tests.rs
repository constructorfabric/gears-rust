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

//! The decisions behind the PEP resources the broker asks about: which
//! event-type selectors can match an event type, and which ids are refused
//! before the PEP is asked at all.

use authz_resolver_sdk::test_support::EmptyScopePdp;
use gts::{GtsIdPattern, GtsTypeId};

use super::{check_event_type_selector, event_type_resource};
use crate::domain::error::{DomainError, ErrorCode};

const EVENT_BASE: &str = "gts.cf.core.events.event.v1~";

fn pattern(raw: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(raw).expect("test pattern must be a valid GTS pattern")
}

/// Whether a selector passes the check, or the refusal code.
fn selector_check(raw: &str) -> Result<(), ErrorCode> {
    check_event_type_selector(&pattern(raw)).map_err(|err| match err {
        DomainError::Validation { code, .. } => code,
        other => panic!("unexpected error kind: {other:?}"),
    })
}

#[test]
fn selectors_that_can_match_an_event_type_are_accepted() {
    // An exact type, and the base type a family grant covers.
    assert_eq!(
        selector_check("gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"),
        Ok(())
    );
    assert_eq!(selector_check(EVENT_BASE), Ok(()));
    // A wildcard narrower than the event family, and one broader than it,
    // which still matches every event type.
    assert_eq!(selector_check("gts.cf.core.events.event.v1~x.eb.*"), Ok(()));
    assert_eq!(selector_check("gts.cf.core.events.*"), Ok(()));
}

#[test]
fn interests_that_cannot_match_an_event_type_are_refused_before_the_pep() {
    // Another gear's type, exact or as a family: no event can match, and
    // asking the PEP would turn it into a question about that gear's resource.
    assert_eq!(
        selector_check("gts.cf.irm.core.resource.v1~"),
        Err(ErrorCode::BadTypePattern)
    );
    assert_eq!(
        selector_check("gts.cf.irm.*"),
        Err(ErrorCode::BadTypePattern)
    );
}

#[test]
fn publishing_a_type_that_is_not_an_event_type_is_refused_before_the_pep() {
    let err = event_type_resource(&GtsTypeId::new("gts.cf.irm.core.resource.v1~"))
        .expect_err("a non-event type must not reach the PEP");
    assert!(
        matches!(
            err,
            DomainError::NotFound {
                code: ErrorCode::EventTypeNotFound,
                ..
            }
        ),
        "got {err:?}"
    );
    assert!(
        event_type_resource(&GtsTypeId::new(
            "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"
        ))
        .is_ok()
    );
}

#[test]
fn event_and_topic_resources_advertise_the_target_tenant_property() {
    use toolkit_security::pep_properties::OWNER_TENANT_ID;
    let topic =
        toolkit_gts::GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1")
            .unwrap();
    for resource in [
        super::topic_resource(&topic),
        event_type_resource(&GtsTypeId::new(EVENT_BASE)).unwrap(),
    ] {
        assert!(resource.supported_properties().contains(&OWNER_TENANT_ID));
    }
}

use uuid::Uuid;

#[tokio::test]
async fn empty_scope_cannot_authorize_target_tenant() {
    let tenant = Uuid::from_u128(1);
    let ctx = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(2))
        .subject_tenant_id(tenant)
        .build()
        .unwrap();
    for live in [false, true] {
        let pep =
            authz_resolver_sdk::PolicyEnforcer::new(std::sync::Arc::new(EmptyScopePdp { live }));
        let result = super::tenant_authorized(&ctx, &pep, "produce", tenant).await;
        assert_eq!(result.is_ok(), live);
        if !live {
            assert!(matches!(
                result,
                Err(DomainError::Forbidden {
                    code: ErrorCode::TenantIdNotAuthorized,
                    ..
                })
            ));
        }
    }
}

//! The decisions behind the PEP resources the broker asks about: which
//! interests are checked on the exact type and which on the event base type,
//! and which ids are refused before the PEP is asked at all.

use gts::{GtsIdPattern, GtsTypeId};

use super::{event_type_interest_resource, event_type_resource};
use crate::domain::error::{DomainError, ErrorCode};

const EVENT_BASE: &str = "gts.cf.core.events.event.v1~";

fn pattern(raw: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(raw).expect("test pattern must be a valid GTS pattern")
}

/// The PEP resource type an interest is checked on, or the refusal code.
fn interest_check(raw: &str) -> Result<String, ErrorCode> {
    event_type_interest_resource(&pattern(raw))
        .map(|resource| resource.name().to_owned())
        .map_err(|err| match err {
            DomainError::Validation { code, .. } => code,
            other => panic!("unexpected error kind: {other:?}"),
        })
}

#[test]
fn exact_interests_are_checked_on_the_type_they_name() {
    let exact = "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~";
    assert_eq!(interest_check(exact).as_deref(), Ok(exact));
    // The base type is wildcard-free too, and is what a family grant covers.
    assert_eq!(interest_check(EVENT_BASE).as_deref(), Ok(EVENT_BASE));
}

#[test]
fn wildcard_interests_are_checked_on_the_event_base_type() {
    // A wildcard pattern is not a GTS identifier, so it never becomes the
    // resource type - whether it is narrower than the event family...
    assert_eq!(
        interest_check("gts.cf.core.events.event.v1~x.eb.*").as_deref(),
        Ok(EVENT_BASE)
    );
    // ...or broader than it, which still matches every event type.
    assert_eq!(
        interest_check("gts.cf.core.events.*").as_deref(),
        Ok(EVENT_BASE)
    );
}

#[test]
fn interests_that_cannot_match_an_event_type_are_refused_before_the_pep() {
    // Another gear's type, exact or as a family: no event can match, and
    // asking the PEP would turn it into a question about that gear's resource.
    assert_eq!(
        interest_check("gts.cf.irm.core.resource.v1~"),
        Err(ErrorCode::BadTypePattern)
    );
    assert_eq!(
        interest_check("gts.cf.irm.*"),
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

/// Declaring the property lets the SDK compile a PDP's tenant constraint
/// instead of failing closed; [`super::authorize_in_tenant`] then checks the
/// compiled scope against the concrete tenant (see the tests below).
#[test]
fn event_and_topic_resources_advertise_the_target_tenant_property() {
    use toolkit_security::pep_properties::OWNER_TENANT_ID;
    let topic =
        toolkit_gts::GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1")
            .unwrap();
    for resource in [
        super::topic_resource(&topic),
        event_type_resource(&GtsTypeId::new(EVENT_BASE)).unwrap(),
        event_type_interest_resource(&pattern(EVENT_BASE)).unwrap(),
        event_type_interest_resource(&pattern("gts.cf.core.events.*")).unwrap(),
    ] {
        assert!(resource.supported_properties().contains(&OWNER_TENANT_ID));
    }
}

fn caller() -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_tenant_id(uuid::Uuid::new_v4())
        .subject_id(uuid::Uuid::new_v4())
        .build()
        .expect("valid caller")
}

/// A PDP that allows only within some tenants, by returning an
/// `In(owner_tenant_id, ..)` constraint, must not authorize another tenant:
/// the broker never applies the scope as a query predicate, so it decides the
/// point itself.
#[tokio::test]
async fn a_constrained_allow_covers_only_the_tenants_it_names() {
    use crate::test_support::TenantScopedAuthZ;
    use authz_resolver_sdk::{AccessRequest, EnforcerError, PolicyEnforcer};
    use std::sync::Arc;

    let granted = uuid::Uuid::new_v4();
    let foreign = uuid::Uuid::new_v4();
    let policy = PolicyEnforcer::new(Arc::new(TenantScopedAuthZ {
        tenants: vec![granted],
    }));
    let topic =
        toolkit_gts::GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.orders.acme.v1")
            .unwrap();
    let ctx = caller();
    for resource in [
        super::topic_resource(&topic),
        event_type_resource(&GtsTypeId::new(EVENT_BASE)).unwrap(),
        event_type_interest_resource(&pattern("gts.cf.core.events.*")).unwrap(),
        super::TENANT_SCOPE_RESOURCE,
    ] {
        let name = resource.name().to_owned();
        super::authorize_in_tenant(
            &ctx,
            &policy,
            &resource,
            "consume",
            AccessRequest::new(),
            granted,
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: the named tenant is covered: {e}"));
        let denial = super::authorize_in_tenant(
            &ctx,
            &policy,
            &resource,
            "consume",
            AccessRequest::new(),
            foreign,
        )
        .await;
        assert!(
            matches!(
                &denial,
                Err(EnforcerError::Denied { deny_reason: Some(reason) })
                    if reason.error_code == super::TENANT_OUTSIDE_SCOPE
            ),
            "{name}: another tenant must be denied, got {denial:?}"
        );
    }
}

/// The tenant-scope gate maps a foreign tenant to its own denial code.
#[tokio::test]
async fn the_tenant_gate_denies_a_tenant_outside_a_constrained_allow() {
    use crate::test_support::TenantScopedAuthZ;
    use authz_resolver_sdk::PolicyEnforcer;
    use std::sync::Arc;

    let granted = uuid::Uuid::new_v4();
    let policy = PolicyEnforcer::new(Arc::new(TenantScopedAuthZ {
        tenants: vec![granted],
    }));
    let ctx = caller();
    super::tenant_authorized(&ctx, &policy, "produce", granted)
        .await
        .expect("the named tenant is covered");
    assert!(matches!(
        super::tenant_authorized(&ctx, &policy, "produce", uuid::Uuid::new_v4()).await,
        Err(DomainError::Forbidden {
            code: ErrorCode::TenantIdNotAuthorized,
            ..
        })
    ));
}

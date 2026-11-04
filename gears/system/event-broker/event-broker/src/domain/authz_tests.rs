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

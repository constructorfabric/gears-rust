// Created: 2026-08-12 by Constructor Tech
//! Tests for the reader degradation contract and its typed projection.
//!
//! Contract source: FEATURE `gear-foundation.md` §5 *Reader Degradation Contract
//! and Typed Projection*, and the platform ADR on SDK error surfaces.
//!
//! `CanonicalError` is `Debug + Clone` but **not** `PartialEq`, so assertions
//! match on variant shape rather than comparing values.

use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

use super::{SettingsError, not_found_resource};
use crate::gts::{CATEGORY_SCHEMA, DECLARATION_SCHEMA, VALUE_SCHEMA};
use crate::precondition::SETTING_RETIRED;

#[resource_error("gts.cf.toolkit.settings.declaration.v1~")]
struct DeclarationScope;

#[resource_error("gts.cf.toolkit.settings.value.v1~")]
struct ValueScope;

#[resource_error("gts.cf.toolkit.settings.category.v1~")]
struct CategoryScope;

fn problem_json(err: CanonicalError) -> serde_json::Value {
    serde_json::to_value(Problem::from(err)).expect("Problem serializes")
}

#[test]
fn service_unavailable_projects_to_unavailable() {
    let err = CanonicalError::service_unavailable()
        .with_detail("database unreachable")
        .create();
    match SettingsError::from(err) {
        SettingsError::Unavailable { detail, .. } => {
            assert!(detail.contains("database unreachable"));
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn a_retry_hint_survives_the_projection() {
    // This is the field a backing-off consumer reads. Dropped silently, every
    // caller falls back to its own guess about how long to wait.
    let err = CanonicalError::service_unavailable()
        .with_detail("database unreachable")
        .with_retry_after_seconds(30)
        .create();
    match SettingsError::from(err) {
        SettingsError::Unavailable {
            retry_after_secs, ..
        } => {
            assert_eq!(retry_after_secs, Some(30));
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn an_absent_retry_hint_stays_absent() {
    // No hint must not become a fabricated one — `None` says "the service did
    // not say", which is different from "retry immediately".
    let err = CanonicalError::service_unavailable().create();
    match SettingsError::from(err) {
        SettingsError::Unavailable {
            retry_after_secs, ..
        } => {
            assert_eq!(retry_after_secs, None);
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn a_declaration_not_found_projects_to_not_found() {
    let err = DeclarationScope::not_found("no declaration for key")
        .with_resource("some.key.v1")
        .create();
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::NotFound { .. }
    ));
}

#[test]
fn a_value_not_found_projects_to_secret_not_configured() {
    // Same canonical category as the case above; only the resource type differs.
    // Conflating the two is what makes a consumer hand a placeholder to its
    // backend believing it to be a credential.
    let err = ValueScope::not_found("no credential at any scope")
        .with_resource("some.key.v1")
        .create();
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::SecretNotConfigured { .. }
    ));
}

#[test]
fn not_found_and_secret_not_configured_are_different_variants() {
    let declaration =
        SettingsError::from(DeclarationScope::not_found("d").with_resource("k").create());
    let credential = SettingsError::from(ValueScope::not_found("c").with_resource("k").create());
    assert!(matches!(declaration, SettingsError::NotFound { .. }));
    assert!(matches!(
        credential,
        SettingsError::SecretNotConfigured { .. }
    ));
}

#[test]
fn a_retired_precondition_projects_to_retired() {
    // `Retired` has no canonical category, so it travels as a precondition
    // violation type and is fanned back out here.
    let err = DeclarationScope::failed_precondition()
        .with_precondition_violation("setting", "declaration was retired", SETTING_RETIRED)
        .create();
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::Retired { .. }
    ));
}

#[test]
fn retired_is_not_reported_as_not_found() {
    // A retired declaration is a positive fact: the row exists. A consumer told
    // "not found" would wait for it to appear, which will never happen.
    let err = DeclarationScope::failed_precondition()
        .with_precondition_violation("setting", "declaration was retired", SETTING_RETIRED)
        .create();
    assert!(!matches!(
        SettingsError::from(err),
        SettingsError::NotFound { .. }
    ));
}

#[test]
fn a_non_retirement_precondition_reaches_the_catch_all() {
    // The guard on the `Retired` arm exists precisely for this case. Without
    // this test, widening it to match any failed precondition — or dropping the
    // `if` entirely — leaves every other test green while every precondition
    // failure starts claiming the setting was withdrawn.
    let err = DeclarationScope::failed_precondition()
        .with_precondition_violation("setting", "value is under review", "NEEDS_REVIEW")
        .create();
    match SettingsError::from(err) {
        SettingsError::Other { .. } => {}
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn permission_denied_projects_to_unauthorized() {
    let err = ValueScope::permission_denied()
        .with_reason("SETTING_NOT_ENTITLED")
        .create();
    assert!(matches!(
        SettingsError::from(err),
        SettingsError::Unauthorized { .. }
    ));
}

#[test]
fn an_unmodelled_category_reaches_the_catch_all_intact() {
    // Forward compatibility: a category the SDK does not model must arrive with
    // its canonical value preserved, not collapsed into a neighbouring variant.
    //
    // `Internal` deliberately keeps the caller's string in the in-process
    // diagnostic and returns a generic `detail`, so the wire never carries it.
    // Asserting on the diagnostic is what proves the canonical value survived
    // the projection rather than being rebuilt from the public text.
    let err = CanonicalError::internal("not built yet").create();
    match SettingsError::from(err) {
        SettingsError::Other { canonical } => {
            assert_eq!(canonical.diagnostic(), Some("not built yet"));
            assert!(
                !canonical.detail().contains("not built yet"),
                "the diagnostic must stay out of the public detail"
            );
        }
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn the_projection_is_infallible_for_every_modelled_category() {
    // No input may make the conversion panic or refuse — that is what lets a
    // consumer write `.map_err(SettingsError::from)?` without a fallback.
    let cases = vec![
        CanonicalError::service_unavailable().create(),
        DeclarationScope::not_found("d").with_resource("k").create(),
        ValueScope::not_found("c").with_resource("k").create(),
        ValueScope::permission_denied().with_reason("X").create(),
        CanonicalError::internal("i").create(),
    ];
    for err in cases {
        let _projected = SettingsError::from(err);
    }
}

#[test]
fn setting_retired_constant_round_trips_to_the_violation_type() {
    let err = DeclarationScope::failed_precondition()
        .with_precondition_violation("setting", "retired", SETTING_RETIRED)
        .create();
    let json = problem_json(err);
    assert_eq!(
        json["context"]["violations"][0]["type"], SETTING_RETIRED,
        "the retired marker must reach failed_precondition.ctx.violations[].type"
    );
}

#[test]
fn resource_constants_round_trip_to_the_not_found_context() {
    for (err, wire) in [
        (
            DeclarationScope::not_found("d").with_resource("k").create(),
            DECLARATION_SCHEMA,
        ),
        (
            ValueScope::not_found("c").with_resource("k").create(),
            VALUE_SCHEMA,
        ),
        (
            CategoryScope::not_found("g").with_resource("k").create(),
            CATEGORY_SCHEMA,
        ),
    ] {
        let json = problem_json(err);
        assert_eq!(
            json["context"]["resource_type"], wire,
            "`{wire}` must reach not_found.ctx.resource_type"
        );
    }
}

#[test]
fn an_unattributed_not_found_falls_to_not_found_never_to_secret() {
    // A producer or proxy can strip `resource_type` on the wire, which is the
    // only discriminator separating "never declared" from "no credential".
    // Losing it must degrade to the weaker claim: `SecretNotConfigured` asserts
    // the declaration resolved, and an unattributed error does not support that.
    let mut problem = serde_json::to_value(Problem::from(
        ValueScope::not_found("c").with_resource("k").create(),
    ))
    .expect("serializes");
    problem["context"]
        .as_object_mut()
        .expect("context is an object")
        .remove("resource_type");

    let stripped: Problem = serde_json::from_value(problem).expect("still a Problem");
    let err = CanonicalError::try_from(stripped).expect("still canonical");

    assert!(
        not_found_resource(&err).is_none(),
        "the fixture must actually have lost its attribution"
    );
    assert!(
        matches!(SettingsError::from(err), SettingsError::NotFound { .. }),
        "an unattributed not-found must never be reported as a missing credential"
    );
}

#[test]
fn the_projection_carries_no_transport_fields() {
    // `instance` and `trace_id` belong to the Problem envelope. A projection
    // that copied them would let a caller log a stale correlation id.
    let rendered = format!(
        "{:?}",
        SettingsError::from(CanonicalError::service_unavailable().create())
    );
    assert!(!rendered.contains("trace_id"), "got `{rendered}`");
    assert!(!rendered.contains("instance"), "got `{rendered}`");
}

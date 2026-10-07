use toolkit_canonical_errors::CanonicalError;

use super::{AdmissionFailure, context, reason};
use crate::TYPE_RESOURCE_TYPE;

const KEY: &str = "gts.cf.test.pkg.thing.v1~";

#[test]
fn a_failure_with_every_context_field_round_trips() {
    let failure = AdmissionFailure::new(reason::BLOCKED_BY_DEPENDENCY, "the base is not admitted")
        .with_context(context::DEPENDENCY_ID, "gts.cf.test.pkg.base.v1~")
        .with_context(context::DEPENDENCY_KIND, "derivation_base")
        .with_context(context::DIAGNOSTIC_CODE, "x1")
        .with_context(context::STORED_VERSION, "0.3.0")
        .with_context(context::OFFERED_VERSION, "0.2.0")
        .with_context(context::STORED_PUBLISHER, "gear-a")
        .with_context(context::OFFERED_PUBLISHER, "gear-b");

    let error = failure.clone().into_canonical(KEY);

    assert_eq!(AdmissionFailure::from_canonical(&error), Some(failure));
}

#[test]
fn unknown_reasons_and_context_names_survive() {
    let failure = AdmissionFailure::new("future_reason", "from a newer registry")
        .with_context("future_field", "value");
    let error = failure.clone().into_canonical(KEY);
    assert_eq!(AdmissionFailure::from_canonical(&error), Some(failure));
}

#[test]
fn the_encoding_names_the_item_and_the_entity_resource_type() {
    let error = AdmissionFailure::new(reason::PRECONDITION_FAILED, "stale").into_canonical(KEY);

    assert!(matches!(error, CanonicalError::FailedPrecondition { .. }));
    assert_eq!(error.resource_type(), Some(TYPE_RESOURCE_TYPE));
    assert_eq!(error.resource_name(), Some(KEY));
}

#[test]
fn a_reason_spelled_like_a_context_entry_is_still_the_primary_reason() {
    let failure = AdmissionFailure::new("context.trick", "odd but legal");
    let error = failure.clone().into_canonical(KEY);
    assert_eq!(AdmissionFailure::from_canonical(&error), Some(failure));
}

#[test]
fn other_shapes_do_not_decode() {
    let not_found = crate::gts::TypeResource::not_found("absent")
        .with_resource(KEY)
        .create();
    assert_eq!(AdmissionFailure::from_canonical(&not_found), None);

    let foreign = crate::gts::TypeResource::failed_precondition()
        .with_precondition_violation(KEY, "", "x")
        .with_precondition_violation("v", "", "not_a_context_entry")
        .create();
    assert_eq!(AdmissionFailure::from_canonical(&foreign), None);
}

proptest::proptest! {
    /// Round-trip arbitrary reason/message/context, including empty strings and context-like names.
    #[test]
    fn any_failure_round_trips(
        reason in "(context\\.)?[a-z_.]{0,12}",
        message in ".{0,24}",
        context in proptest::collection::btree_map("(context\\.)?[a-z_.]{0,10}", ".{0,12}", 0..4),
    ) {
        let failure = AdmissionFailure { reason, message, context };
        let error = failure.clone().into_canonical(KEY);
        proptest::prop_assert_eq!(AdmissionFailure::from_canonical(&error), Some(failure));
    }
}

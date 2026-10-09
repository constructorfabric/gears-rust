// Malformed test fixtures and source guards must fail the test immediately.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use toolkit_canonical_errors::{CanonicalError, Problem};

use super::{AdmissionFailure, AdmissionFailureReason, DependencyKind, context, dependency_kind};
use crate::TYPE_RESOURCE_TYPE;
use crate::precondition::PARENT_NOT_REGISTERED;

const KEY: &str = "gts.cf.test.pkg.thing.v1~";

#[test]
fn a_failure_with_every_context_field_round_trips() {
    let failure = AdmissionFailure::new(
        AdmissionFailureReason::BlockedByDependency,
        "the base is not admitted",
    )
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
    let failure = AdmissionFailure::new(
        AdmissionFailureReason::from_wire("future_reason"),
        "from a newer registry",
    )
    .with_context("future_field", "value");
    let error = failure.clone().into_canonical(KEY);
    assert_eq!(AdmissionFailure::from_canonical(&error), Some(failure));
}

#[test]
fn the_encoding_names_the_item_and_the_entity_resource_type() {
    let error = AdmissionFailure::new(AdmissionFailureReason::PreconditionFailed, "stale")
        .into_canonical(KEY);

    assert!(matches!(error, CanonicalError::FailedPrecondition { .. }));
    assert_eq!(error.resource_type(), Some(TYPE_RESOURCE_TYPE));
    assert_eq!(error.resource_name(), Some(KEY));
}

#[test]
fn a_reason_spelled_like_a_context_entry_is_still_the_primary_reason() {
    let failure = AdmissionFailure::new(
        AdmissionFailureReason::from_wire("context.trick"),
        "odd but legal",
    );
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
        .with_resource(KEY)
        .with_precondition_violation(KEY, "", "x")
        .with_precondition_violation("v", "", "not_a_context_entry")
        .create();
    assert_eq!(AdmissionFailure::from_canonical(&foreign), None);
}

/// Each shape the decoder must refuse, so a near miss stays a plain `CanonicalError`.
#[test]
fn near_misses_of_the_encoding_do_not_decode() {
    let base = || crate::gts::TypeResource::failed_precondition().with_resource(KEY);
    let refused = [
        // The parent-not-registered refusal names the parent, not the dependent.
        base()
            .with_precondition_violation(
                "gts.cf.test.pkg.base.v1~",
                "absent",
                PARENT_NOT_REGISTERED,
            )
            .create(),
        // A registration-policy refusal names the region.
        base()
            .with_precondition_violation(
                "<default>",
                "vendor not allowed",
                format!(
                    "{}ALLOWED_VENDORS",
                    crate::precondition::REGISTRATION_POLICY_PREFIX
                ),
            )
            .create(),
        // Reserved codes are refused on their own, even when the subject is the key.
        base()
            .with_precondition_violation(KEY, "absent", PARENT_NOT_REGISTERED)
            .create(),
        base()
            .with_precondition_violation(
                KEY,
                "vendor not allowed",
                crate::precondition::REGISTRATION_POLICY_ALLOWED_VENDORS,
            )
            .create(),
        // A primary violation that does not name the resource.
        base()
            .with_precondition_violation("another.key.v1~", "m", "already_exists")
            .create(),
        // No resource name at all.
        crate::gts::TypeResource::failed_precondition()
            .with_precondition_violation(KEY, "m", "already_exists")
            .create(),
        // A context entry carrying a description.
        base()
            .with_precondition_violation(KEY, "m", "already_exists")
            .with_precondition_violation("v", "not empty", "context.dependency_id")
            .create(),
        // The same context name twice would silently lose one value.
        base()
            .with_precondition_violation(KEY, "m", "already_exists")
            .with_precondition_violation("a", "", "context.dependency_id")
            .with_precondition_violation("b", "", "context.dependency_id")
            .create(),
        // The operation resource type.
        crate::gts::OperationResource::failed_precondition()
            .with_resource(KEY)
            .with_precondition_violation(KEY, "m", "already_exists")
            .create(),
    ];
    for error in refused {
        assert_eq!(AdmissionFailure::from_canonical(&error), None, "{error:?}");
    }
}

proptest::proptest! {
    /// Round-trip arbitrary reason/message/context, including empty strings and context-like names.
    #[test]
    fn any_failure_round_trips(
        reason in "(context\\.)?[a-z_.]{0,12}",
        message in ".{0,24}",
        context in proptest::collection::btree_map("(context\\.)?[a-z_.]{0,10}", ".{0,12}", 0..4),
    ) {
        let failure = AdmissionFailure {
            reason: AdmissionFailureReason::from_wire(&reason),
            message,
            context,
        };
        let error = failure.clone().into_canonical(KEY);
        proptest::prop_assert_eq!(AdmissionFailure::from_canonical(&error), Some(failure));
    }
}

// ---- the reason vocabulary (P16 rule 3) ---------------------------------------
//
// Exhaustive production matches require codes and labels. These tests check
// uniqueness, round-trips, and bounded labels. [`known`] is checked against
// the enum source so an omitted variant cannot escape those assertions.

use super::AdmissionFailureReason as Reason;

/// Every known variant. `Unknown` is deliberately absent: it is the escape hatch
/// for a code this build does not know, not a member of the vocabulary.
fn known() -> Vec<Reason> {
    vec![
        Reason::ActivationWriteSetExceeded,
        Reason::AlreadyExists,
        Reason::BaselineUnresolvable,
        Reason::BlockedByDependency,
        Reason::BlockedByPredecessor,
        Reason::CompatibilityUndecidable,
        Reason::DependencyDeleted,
        Reason::DependencyNotFound,
        Reason::DependentInvalid,
        Reason::DialectChanged,
        Reason::EntityDeleted,
        Reason::FamilyKindConflict,
        Reason::HasRegisteredDependents,
        Reason::FamilyShapeConflict,
        Reason::IncompatibleWithBaseline,
        Reason::InstanceOfMajorZero,
        Reason::InvalidDocument,
        Reason::InvalidIdentifier,
        Reason::InvalidSchema,
        Reason::InvalidValue,
        Reason::MissingPredecessor,
        Reason::NotActive,
        Reason::PreconditionFailed,
        Reason::PublisherMismatch,
        Reason::ResolutionClosureExceeded,
        Reason::ResolvedDocumentTooLarge,
        Reason::RevalidationExhausted,
        Reason::StableDerivesFromMajorZero,
        Reason::StableRefsMajorZero,
        Reason::Superseded,
        Reason::SystemFailure,
        Reason::UnparsablePayload,
        Reason::UnreadableVersion,
        Reason::UnrecognizedPayload,
    ]
}

/// The count [`known`] must have. Bumped deliberately, which is the point: a
/// variant added without a thought about the dashboards reading it fails here.
const KNOWN_VARIANTS: usize = 34;

/// Read variant names from the enum source, failing on unexpected syntax
/// rather than returning an incomplete vocabulary.
fn variant_names_in_source() -> Vec<String> {
    const SOURCE: &str = include_str!("item_failure.rs");

    let (_, after) = SOURCE
        .split_once("pub enum AdmissionFailureReason {")
        .expect("the enum declaration must be found; has it been renamed?");
    let (body, _) = after
        .split_once("\n}")
        .expect("the enum body must be terminated by a closing brace at column 0");

    let names: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with("#["))
        .map(|line| {
            line.trim_end_matches(',')
                .split(['(', ' '])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .filter(|name| name.starts_with(|c: char| c.is_ascii_uppercase()))
        .collect();

    assert!(
        names.len() > 5,
        "the parse found only {names:?}; the enum's shape changed and this guard \
         would otherwise pass by finding nothing",
    );
    names
}

/// Check that [`known`] covers every declared reason exactly once.
#[test]
fn the_listed_vocabulary_matches_the_enum() {
    let mut declared = variant_names_in_source();
    declared.retain(|name| name != "Unknown");
    declared.sort_unstable();

    let mut listed: Vec<String> = known().iter().map(|r| variant_name(r).to_owned()).collect();
    listed.sort_unstable();

    // Sorted equality also rejects duplicates and the `Unknown` escape hatch.
    assert_eq!(
        listed, declared,
        "`known()` and the enum disagree: a variant was added or removed without \
         updating the list this file asserts over, was listed twice, or `Unknown` \
         was listed as a vocabulary member",
    );
    assert_eq!(
        declared.len(),
        KNOWN_VARIANTS,
        "the vocabulary changed size: update KNOWN_VARIANTS deliberately",
    );
}

/// Exhaustive naming; the source-based test separately checks list completeness.
fn variant_name(reason: &Reason) -> &'static str {
    {
        match reason {
            Reason::ActivationWriteSetExceeded => "ActivationWriteSetExceeded",
            Reason::AlreadyExists => "AlreadyExists",
            Reason::BaselineUnresolvable => "BaselineUnresolvable",
            Reason::BlockedByDependency => "BlockedByDependency",
            Reason::BlockedByPredecessor => "BlockedByPredecessor",
            Reason::CompatibilityUndecidable => "CompatibilityUndecidable",
            Reason::DependencyDeleted => "DependencyDeleted",
            Reason::DependencyNotFound => "DependencyNotFound",
            Reason::DependentInvalid => "DependentInvalid",
            Reason::DialectChanged => "DialectChanged",
            Reason::EntityDeleted => "EntityDeleted",
            Reason::FamilyKindConflict => "FamilyKindConflict",
            Reason::HasRegisteredDependents => "HasRegisteredDependents",
            Reason::FamilyShapeConflict => "FamilyShapeConflict",
            Reason::IncompatibleWithBaseline => "IncompatibleWithBaseline",
            Reason::InstanceOfMajorZero => "InstanceOfMajorZero",
            Reason::InvalidDocument => "InvalidDocument",
            Reason::InvalidIdentifier => "InvalidIdentifier",
            Reason::InvalidSchema => "InvalidSchema",
            Reason::InvalidValue => "InvalidValue",
            Reason::MissingPredecessor => "MissingPredecessor",
            Reason::NotActive => "NotActive",
            Reason::PreconditionFailed => "PreconditionFailed",
            Reason::PublisherMismatch => "PublisherMismatch",
            Reason::ResolutionClosureExceeded => "ResolutionClosureExceeded",
            Reason::ResolvedDocumentTooLarge => "ResolvedDocumentTooLarge",
            Reason::RevalidationExhausted => "RevalidationExhausted",
            Reason::StableDerivesFromMajorZero => "StableDerivesFromMajorZero",
            Reason::StableRefsMajorZero => "StableRefsMajorZero",
            Reason::Superseded => "Superseded",
            Reason::SystemFailure => "SystemFailure",
            Reason::UnparsablePayload => "UnparsablePayload",
            Reason::UnreadableVersion => "UnreadableVersion",
            Reason::UnrecognizedPayload => "UnrecognizedPayload",
            Reason::Unknown(_) => "Unknown",
        }
    }
}

/// Every stored code restores its own variant. A code that round-trips to the
/// *wrong* variant would relabel a refusal in the metrics without any read failing.
#[test]
fn every_code_round_trips_to_the_variant_that_wrote_it() {
    for reason in known() {
        let code = reason.as_wire().to_owned();
        assert_eq!(
            Reason::from_wire(&code),
            reason,
            "'{code}' did not restore the variant it came from",
        );
    }
}

/// Known reasons use the same code in stored errors and metric labels.
#[test]
fn a_known_reasons_stored_code_is_its_metric_label() {
    for reason in known() {
        assert_eq!(reason.as_wire(), reason.metric_label(), "{reason:?}");
    }
}

/// No two reasons share a code. Two refusals under one code are one number an
/// operator cannot act on, which is the failure P16 exists to prevent.
#[test]
fn no_two_reasons_share_a_code() {
    let mut codes: Vec<&str> = known().iter().map(Reason::metric_label).collect();
    let total = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), total, "duplicate code in the vocabulary");
}

/// Every code is a readable snake-case token, not a `Debug` rendering that would
/// change shape the day someone renames a variant.
#[test]
fn every_code_is_stable_snake_case() {
    for reason in known() {
        let code = reason.metric_label();
        assert!(!code.is_empty(), "{reason:?} has an empty code");
        assert!(
            code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "'{code}' is not snake-case, so it did not come from an explicit mapping",
        );
        assert!(
            !code.starts_with('_') && !code.ends_with('_') && !code.contains("__"),
            "'{code}' is malformed",
        );
    }
}

/// Preserve unfamiliar stored codes across versions; map their metric label
/// to `other` to bound cardinality.
#[test]
fn an_unfamiliar_code_is_preserved_in_storage_and_bounded_in_metrics() {
    let restored = Reason::from_wire("something_a_later_version_wrote");
    assert_eq!(
        restored,
        Reason::Unknown("something_a_later_version_wrote".to_owned()),
    );
    assert_eq!(
        restored.as_wire(),
        "something_a_later_version_wrote",
        "the row's own code survives the round trip verbatim",
    );
    assert_eq!(
        restored.metric_label(),
        "other",
        "and shares one bounded series rather than minting one of its own",
    );
}

/// `other` is reserved for the unknown bucket: no known reason may claim it, or a
/// real refusal would land in the bucket meant for codes this build cannot read.
#[test]
fn no_known_reason_claims_the_unknown_bucket() {
    assert!(
        known().iter().all(|r| r.metric_label() != "other"),
        "a known reason is hiding in the `other` series",
    );
}

/// Quarantine and dialect refusals must not use `invalid_schema`.
/// Whole-vocabulary tests already cover distinctness and round-trips.
#[test]
fn t18s_quarantine_and_dialect_reasons_are_four_distinct_codes() {
    let codes = [
        Reason::StableDerivesFromMajorZero.metric_label(),
        Reason::StableRefsMajorZero.metric_label(),
        Reason::InstanceOfMajorZero.metric_label(),
        Reason::DialectChanged.metric_label(),
    ];
    let mut unique = codes.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 4, "{codes:?}");
    assert!(
        !codes.contains(&Reason::InvalidSchema.metric_label()),
        "a rule refusal is wearing the malformed-document code",
    );
    for code in codes {
        assert_eq!(Reason::from_wire(code).metric_label(), code);
    }
}

// ---- dependency_kind ----------------------------------------------------------

const DEPENDENCY_KINDS: [(&str, DependencyKind); 3] = [
    (dependency_kind::BASE, DependencyKind::Base),
    (
        dependency_kind::CONFORMING_TYPE,
        DependencyKind::ConformingType,
    ),
    (dependency_kind::REF, DependencyKind::Ref),
];

#[test]
fn dependency_kind_values_are_the_designed_spellings() {
    assert_eq!(dependency_kind::BASE, "base");
    assert_eq!(dependency_kind::CONFORMING_TYPE, "conforming_type");
    assert_eq!(dependency_kind::REF, "ref");
}

#[test]
fn every_dependency_kind_round_trips_its_wire_value() {
    for (wire, kind) in DEPENDENCY_KINDS {
        assert_eq!(DependencyKind::from_wire(wire), kind);
        assert_eq!(kind.as_wire(), wire);
        assert_eq!(kind.to_string(), wire);
    }
    for unfamiliar in ["derivation_base", ""] {
        let kind = DependencyKind::from_wire(unfamiliar);
        assert_eq!(kind, DependencyKind::Unknown(unfamiliar.to_owned()));
        assert_eq!(kind.as_wire(), unfamiliar);
    }
}

#[test]
fn the_dependency_kind_accessor_is_none_only_when_absent() {
    let bare = AdmissionFailure::new(AdmissionFailureReason::DependencyNotFound, "absent");
    assert_eq!(bare.dependency_kind(), None);
    for (wire, kind) in DEPENDENCY_KINDS {
        let failure = bare.clone().with_context(context::DEPENDENCY_KIND, wire);
        assert_eq!(failure.dependency_kind(), Some(kind));
    }
    for unfamiliar in ["later", ""] {
        let failure = bare
            .clone()
            .with_context(context::DEPENDENCY_KIND, unfamiliar);
        assert_eq!(
            failure.dependency_kind(),
            Some(DependencyKind::Unknown(unfamiliar.to_owned()))
        );
    }
}

#[test]
fn every_dependency_kind_survives_problem_json() {
    for (wire, kind) in DEPENDENCY_KINDS {
        let error = AdmissionFailure::new(AdmissionFailureReason::DependencyDeleted, "tombstone")
            .with_context(context::DEPENDENCY_ID, "gts.cf.test.pkg.base.v1~")
            .with_context(context::DEPENDENCY_KIND, wire)
            .into_canonical(KEY);
        let json = serde_json::to_value(Problem::from(error)).expect("serialize");
        assert!(
            json["context"]["violations"]
                .as_array()
                .expect("violations")
                .iter()
                .any(|v| v["type"] == "context.dependency_kind" && v["subject"] == wire),
            "{wire} must ride in a context.dependency_kind violation: {json}"
        );
        let restored: Problem = serde_json::from_value(json).expect("deserialize");
        let canonical = CanonicalError::try_from(restored).expect("reconstruct");
        let failure = AdmissionFailure::from_canonical(&canonical).expect("an item failure");
        assert_eq!(failure.dependency_kind(), Some(kind));
    }
}

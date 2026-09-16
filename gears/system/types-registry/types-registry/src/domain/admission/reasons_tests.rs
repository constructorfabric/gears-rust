//! Admission reason vocabulary checks (P16 rule 3).
//!
//! Exhaustive production matches require codes and labels. These tests check
//! uniqueness, round-trips, and bounded labels. [`known`] is checked against
//! the enum source so an omitted variant cannot escape those assertions.
//! Using `sea_orm`'s `EnumIter` would add a storage dependency to the domain.

// Malformed test fixtures and source guards must fail the test immediately.
#![allow(clippy::expect_used, clippy::unwrap_used)]

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
        Reason::ResolutionClosureExceeded,
        Reason::ResolvedDocumentTooLarge,
        Reason::RevalidationExhausted,
        Reason::StableDerivesFromMajorZero,
        Reason::StableRefsMajorZero,
        Reason::UnparsablePayload,
        Reason::UnreadableVersion,
        Reason::UnrecognizedPayload,
    ]
}

/// The count [`known`] must have. Bumped deliberately, which is the point: a
/// variant added without a thought about the dashboards reading it fails here.
const KNOWN_VARIANTS: usize = 29;

/// Read variant names from the enum source, failing on unexpected syntax
/// rather than returning an incomplete vocabulary.
fn variant_names_in_source() -> Vec<String> {
    const SOURCE: &str = include_str!("reasons.rs");

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
            Reason::ResolutionClosureExceeded => "ResolutionClosureExceeded",
            Reason::ResolvedDocumentTooLarge => "ResolvedDocumentTooLarge",
            Reason::RevalidationExhausted => "RevalidationExhausted",
            Reason::StableDerivesFromMajorZero => "StableDerivesFromMajorZero",
            Reason::StableRefsMajorZero => "StableRefsMajorZero",
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
        let code = reason.as_str().to_owned();
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
        assert_eq!(reason.as_str(), reason.metric_label(), "{reason:?}");
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
        restored.as_str(),
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

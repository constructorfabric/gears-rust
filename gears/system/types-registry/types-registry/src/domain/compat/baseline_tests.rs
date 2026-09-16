//! Pure baseline selection and verdict tests. Worker coverage is in
//! `tests/compat_test.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{
    Baseline, Exemption, UnreadableVersion, backward_comparison, refusal, select_baseline,
};
use crate::domain::admission::{AdmissionFailureReason, Precondition};
use crate::domain::compat::{BaselineDoc, CandidateDoc};
use gts::{CompatibilityVerdict, GtsStore};
use serde_json::json;
use toolkit_gts::gts_id;

fn select(id: &str, precondition: Precondition) -> Result<Baseline, UnreadableVersion> {
    let parsed = gts::GtsId::try_new(id).unwrap_or_else(|e| panic!("{id}: {e}"));
    select_baseline(&parsed, precondition)
}

fn baseline(id: &str, precondition: Precondition) -> Baseline {
    select(id, precondition).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn creation(id: &str) -> Baseline {
    baseline(id, Precondition::MustNotExist)
}

fn revision(id: &str) -> Baseline {
    baseline(id, Precondition::Version(7))
}

/// An Instance is validated against a Type Schema revision. Comparing it against
/// its own previous value is a different relation, and ADR-0003 does not define one.
#[test]
fn an_instance_is_compared_against_nothing() {
    assert_eq!(
        creation(gts_id!("cf.core.example.thing.v1~cf.core.example.thing.v1")),
        Baseline::Exempt(Exemption::NotATypeSchema),
    );
    assert_eq!(
        revision(gts_id!("cf.core.example.thing.v1~cf.core.example.thing.v1")),
        Baseline::Exempt(Exemption::NotATypeSchema),
    );
}

/// ADR-0015: major 0 enforces no mode, so it has no baseline and gets no verdict —
/// on a creation and on a revision alike.
#[test]
fn a_major_zero_type_schema_gets_no_baseline_on_either_precondition() {
    assert_eq!(
        creation(gts_id!("cf.core.example.thing.v0~")),
        Baseline::Exempt(Exemption::MajorZero),
    );
    assert_eq!(
        revision(gts_id!("cf.core.example.thing.v0~")),
        Baseline::Exempt(Exemption::MajorZero),
    );
}

/// Major 0 wins over the minor arithmetic: `v0.1~` is exempt, not compared against
/// `v0.0~`. Quarantine is a property of the major, not of the minor.
#[test]
fn a_minor_bearing_major_zero_is_exempt_rather_than_cross_minor() {
    assert_eq!(
        creation(gts_id!("cf.core.example.thing.v0.1~")),
        Baseline::Exempt(Exemption::MajorZero),
    );
}

/// Nothing precedes a first admission, so there is no comparison — and the
/// provenance columns recorded beside it assert nothing about a verdict (ADR-0003).
#[test]
fn a_major_only_creation_has_nothing_to_compare_against() {
    assert_eq!(
        creation(gts_id!("cf.core.example.thing.v1~")),
        Baseline::Exempt(Exemption::FirstAdmission),
    );
}

/// The intra-entity edge: a revision of a major-only Type Schema is checked
/// against its own current revision, and nothing may waive it.
#[test]
fn a_major_only_revision_is_compared_against_its_own_current_revision() {
    let selected = revision(gts_id!("cf.core.example.thing.v1~"));
    assert_eq!(selected, Baseline::CurrentRevision);
    assert!(
        !selected.waivable(),
        "the intra-entity edge carries a floating $ref onto the new revision; \
         ADR-0003 lets nothing waive it",
    );
}

/// `vM.0~` opens its major, so contiguity gives it no predecessor to compare with.
#[test]
fn the_first_minor_of_a_major_has_no_baseline() {
    assert_eq!(
        creation(gts_id!("cf.core.example.thing.v1.0~")),
        Baseline::Exempt(Exemption::FirstMinor),
    );
}

/// The cross-minor edge: `vM.n~` names its own baseline through contiguity, and
/// this is the one check `force` may waive.
#[test]
fn a_later_minor_is_compared_against_its_preceding_minor() {
    let selected = creation(gts_id!("cf.core.example.thing.v2.3~"));
    assert_eq!(
        selected,
        Baseline::PrecedingMinor {
            gts_id: gts_id!("cf.core.example.thing.v2.2~").to_owned(),
        },
    );
    assert!(
        selected.waivable(),
        "ADR-0004 permits force across a minor boundary"
    );
}

/// Use `n - 1` in the same major and retain the Type Schema's trailing `~`.
#[test]
fn the_preceding_minor_stays_within_the_candidates_own_major() {
    assert_eq!(
        creation(gts_id!("cf.core.example.thing.v9.1~")),
        Baseline::PrecedingMinor {
            gts_id: gts_id!("cf.core.example.thing.v9.0~").to_owned(),
        },
    );
}

/// A preceding segment's version belongs to the base and must remain unchanged.
#[test]
fn a_minor_in_a_preceding_segment_is_not_the_one_that_decrements() {
    assert_eq!(
        creation(gts_id!(
            "cf.core.base.thing.v1.4~cf.core.example.derived.v3.2~"
        )),
        Baseline::PrecedingMinor {
            gts_id: gts_id!("cf.core.base.thing.v1.4~cf.core.example.derived.v3.1~").to_owned(),
        },
    );
}

/// Acceptance rejects minor-bearing revisions; selection must still use the strict edge.
#[test]
fn an_unreachable_minor_bearing_revision_gets_the_non_waivable_edge() {
    let selected = revision(gts_id!("cf.core.example.thing.v2.3~"));
    assert_eq!(selected, Baseline::CurrentRevision);
    assert!(!selected.waivable());
}

// ---------------------------------------------------------------------------
// What a verdict means for admission
// ---------------------------------------------------------------------------

/// The admissible verdict, forced or not: `force` waives a check, it does not
/// change what a passing one means.
#[test]
fn a_compatible_verdict_admits_whether_or_not_force_was_sent() {
    assert_eq!(refusal(CompatibilityVerdict::Compatible, false), None);
    assert_eq!(refusal(CompatibilityVerdict::Compatible, true), None);
}

/// Fail closed with distinct reasons for incompatible and undecidable verdicts.
#[test]
fn incompatible_and_unknown_refuse_under_distinct_reasons() {
    assert_eq!(
        refusal(CompatibilityVerdict::Incompatible, false),
        Some(AdmissionFailureReason::IncompatibleWithBaseline),
    );
    assert_eq!(
        refusal(CompatibilityVerdict::Unknown, false),
        Some(AdmissionFailureReason::CompatibilityUndecidable),
    );
    assert_ne!(
        refusal(CompatibilityVerdict::Incompatible, false),
        refusal(CompatibilityVerdict::Unknown, false),
    );
}

/// An authorized `force` waives either adverse cross-minor verdict (ADR-0004).
#[test]
fn force_waives_both_adverse_verdicts() {
    assert_eq!(refusal(CompatibilityVerdict::Incompatible, true), None);
    assert_eq!(refusal(CompatibilityVerdict::Unknown, true), None);
}

// ---------------------------------------------------------------------------
// The comparison entry point
// ---------------------------------------------------------------------------

/// Swapping document roles changes the verdict; distinct side types protect
/// this asymmetric comparison.
#[test]
fn the_verdict_depends_on_which_side_is_the_baseline() {
    let store = GtsStore::new();
    let closed = |props: serde_json::Value| json!({ "type": "object", "additionalProperties": false, "properties": props });
    let fewer = closed(json!({ "a": { "type": "string" } }));
    let more = closed(json!({ "a": { "type": "string" }, "b": { "type": "string" } }));

    let widening = backward_comparison(&store, BaselineDoc::new(&fewer), CandidateDoc::new(&more))
        .expect("self-contained documents resolve");
    assert_eq!(
        widening.backward_compatibility(),
        CompatibilityVerdict::Compatible,
        "adding an optional property to a closed level is backward compatible",
    );

    let narrowing = backward_comparison(&store, BaselineDoc::new(&more), CandidateDoc::new(&fewer))
        .expect("self-contained documents resolve");
    assert_eq!(
        narrowing.backward_compatibility(),
        CompatibilityVerdict::Incompatible,
        "dropping a property from a closed level is not backward compatible",
    );
}

/// Unresolvable references fail comparison (SPEC §7 prerequisite 6).
#[test]
fn a_reference_the_store_cannot_resolve_fails_the_comparison() {
    let store = GtsStore::new();
    let baseline = json!({ "$ref": "gts.cf.core.example.absent.v1~" });
    let candidate = json!({ "type": "object" });

    assert!(
        backward_comparison(
            &store,
            BaselineDoc::new(&baseline),
            CandidateDoc::new(&candidate)
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// Labels an operator reads
// ---------------------------------------------------------------------------

/// Every selection has a label, and the four exemptions stay distinguishable: on a
/// span "no baseline" is not an answer, "no baseline because major 0" is.
#[test]
fn every_baseline_selection_labels_itself_distinguishably() {
    let labels = [
        Baseline::Exempt(Exemption::NotATypeSchema).label(),
        Baseline::Exempt(Exemption::MajorZero).label(),
        Baseline::Exempt(Exemption::FirstAdmission).label(),
        Baseline::Exempt(Exemption::FirstMinor).label(),
        Baseline::CurrentRevision.label(),
        Baseline::PrecedingMinor {
            gts_id: gts_id!("cf.core.example.thing.v1.0~").to_owned(),
        }
        .label(),
    ];
    let mut unique = labels.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        labels.len(),
        "two selections share a label, so a span cannot tell them apart: {labels:?}",
    );
    for label in labels {
        assert!(
            !label.is_empty() && label.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{label} must be a stable snake-case token, not a Debug rendering",
        );
    }
}

/// The label does not depend on *which* preceding minor it is: an identifier is a
/// span field, never part of a label.
#[test]
fn the_preceding_minor_label_carries_no_identifier() {
    let label = Baseline::PrecedingMinor {
        gts_id: gts_id!("cf.core.example.thing.v4.7~").to_owned(),
    }
    .label();
    assert!(!label.contains("cf.core"), "got {label}");
    assert!(!label.contains('4') && !label.contains('7'), "got {label}");
}

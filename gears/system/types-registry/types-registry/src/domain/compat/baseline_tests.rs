//! Pure baseline selection and verdict tests. Worker coverage is in
//! `tests/compat_test.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{
    Baseline, ComparisonFailure, Exemption, TraitsDeclaration, UnreadableVersion,
    backward_comparison, evolution_comparison, refusal, select_baseline,
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

const PARENT: &str = "gts.cf.core.example.thing.v1~";

fn host(id: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    })
}

/// A trait whose value must be a type identifier matching `scope`.
fn scoped(scope: &str) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "scope": { "type": "string", "x-gts-type-ref": scope } },
    })
}

/// The traits half of [`evolution_comparison`] between two hosts.
fn traits_verdict(
    old_host: &serde_json::Value,
    new_host: &serde_json::Value,
    old: &serde_json::Value,
    new: &serde_json::Value,
) -> CompatibilityVerdict {
    declared_verdict(old_host, new_host, old, new, TraitsDeclaration::Kept)
}

fn declared_verdict(
    old_host: &serde_json::Value,
    new_host: &serde_json::Value,
    old: &serde_json::Value,
    new: &serde_json::Value,
    declaration: TraitsDeclaration,
) -> CompatibilityVerdict {
    evolution_comparison(
        &GtsStore::new(),
        BaselineDoc::new(old_host),
        CandidateDoc::new(new_host),
        BaselineDoc::new(old),
        CandidateDoc::new(new),
        declaration,
    )
    .expect("self-contained documents resolve")
    .verdict
}

/// With no declaration left in its chain a type admits no trait values, although
/// its materialized contract is the same dialect-only object an explicit `{}`
/// produces. Withdrawing an open declaration strands every value it admitted.
#[test]
fn withdrawing_an_open_traits_declaration_is_a_break() {
    let parent = host(PARENT);
    let dialect_only = json!({ "$schema": "http://json-schema.org/draft-07/schema#" });
    assert_eq!(
        declared_verdict(
            &parent,
            &parent,
            &dialect_only,
            &dialect_only,
            TraitsDeclaration::Withdrawn,
        ),
        CompatibilityVerdict::Incompatible,
    );
}

/// A declaration that admitted no trait value strands none when withdrawn.
#[test]
fn withdrawing_a_declaration_that_admitted_nothing_is_compatible() {
    let parent = host(PARENT);
    let dialect_only = json!({ "$schema": "http://json-schema.org/draft-07/schema#" });
    assert_eq!(
        declared_verdict(
            &parent,
            &parent,
            &json!(false),
            &dialect_only,
            TraitsDeclaration::Withdrawn,
        ),
        CompatibilityVerdict::Compatible,
    );
}

/// `/$id` in a trait binds to whichever type is validated: a descendant that
/// supplies or inherits `scope = PARENT` fails once the literal becomes `/$id`,
/// although under the host's own identity the two constraints coincide.
#[test]
fn a_literal_scope_becoming_self_relative_is_not_proven_compatible() {
    let parent = host(PARENT);
    assert_ne!(
        traits_verdict(&parent, &parent, &scoped(PARENT), &scoped("/$id")),
        CompatibilityVerdict::Compatible,
    );
}

/// A final baseline has no descendant whose values the change could strand,
/// and under the host's own identity the literal and `/$id` coincide.
#[test]
fn a_final_baseline_owes_no_descendant_reading() {
    let mut parent = host(PARENT);
    parent["x-gts-final"] = json!(true);
    assert_eq!(
        traits_verdict(&parent, &parent, &scoped(PARENT), &scoped("/$id")),
        CompatibilityVerdict::Compatible,
    );
}

/// The host itself validates trait values too: `{"scope": "<v1.0>"}` satisfies
/// `/$id` on v1.0 and not on v1.1, so the identical fragment is still a break.
#[test]
fn an_identical_self_relative_scope_breaks_across_minors() {
    let old = host("gts.cf.core.example.thing.v1.0~");
    let new = host("gts.cf.core.example.thing.v1.1~");
    assert_eq!(
        traits_verdict(&old, &new, &scoped("/$id"), &scoped("/$id")),
        CompatibilityVerdict::Incompatible,
    );
}

/// Trait values are objects, so constraining an open contract to objects
/// removes no value it accepted.
#[test]
fn trait_values_are_compared_as_objects() {
    let parent = host(PARENT);
    assert_eq!(
        traits_verdict(&parent, &parent, &json!({}), &json!({ "type": "object" })),
        CompatibilityVerdict::Compatible,
    );
}

/// GTS cannot prove an `additionalProperties` intersection included in itself,
/// yet a revision that leaves the traits contract untouched changes no trait
/// value's admissibility. Equivalent dialect spellings change nothing either.
#[test]
fn an_untouched_traits_contract_is_compatible_with_itself() {
    let overlapping = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [
            { "additionalProperties": false, "properties": { "a": { "type": "string" } } },
            { "properties": { "b": { "type": "string" } } },
        ],
    });
    let old = host(PARENT);
    let mut retitled = host(PARENT);
    retitled["title"] = json!("Renamed");
    assert_eq!(
        traits_verdict(&old, &retitled, &overlapping, &overlapping),
        CompatibilityVerdict::Compatible,
    );

    let mut respelled = host(PARENT);
    respelled["$schema"] = json!("https://json-schema.org/draft-07/schema");
    let mut respelled_traits = overlapping.clone();
    respelled_traits["$schema"] = json!("http://json-schema.org/draft-07/schema");
    assert_eq!(
        traits_verdict(&old, &respelled, &overlapping, &respelled_traits),
        CompatibilityVerdict::Compatible,
    );
}

/// The fast path compares dialect declarations by meaning and everything else
/// verbatim: a `$schema` key inside a `const` is data, and a different string
/// there admits a different value.
#[test]
fn a_dialect_spelling_inside_trait_data_is_not_normalized() {
    let tagged = |spelling: &str| {
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "tag": { "const": { "$schema": spelling } } },
        })
    };
    let parent = host(PARENT);
    assert_ne!(
        traits_verdict(
            &parent,
            &parent,
            &tagged("http://json-schema.org/draft-07/schema#"),
            &tagged("https://json-schema.org/draft-07/schema"),
        ),
        CompatibilityVerdict::Compatible,
    );
}

/// Both readings can report the same finding at the same place with different
/// prose; the refusal lists it once.
#[test]
fn a_finding_both_readings_report_is_listed_once() {
    let any_type = |scope: &str| {
        json!({
            "type": "object",
            "properties": {
                "scope": { "type": "string", "pattern": ".*", "x-gts-type-ref": scope },
            },
        })
    };
    let parent = host(PARENT);
    let diagnostics = evolution_comparison(
        &GtsStore::new(),
        BaselineDoc::new(&parent),
        CandidateDoc::new(&parent),
        BaselineDoc::new(&any_type("gts.*")),
        CandidateDoc::new(&any_type("/$id")),
        TraitsDeclaration::Kept,
    )
    .expect("self-contained documents resolve")
    .traits;
    let at_scope = diagnostics.iter().filter(|d| d.path == "$.scope").count();
    assert_eq!(at_scope, 1, "{diagnostics:?}");
}

/// One object level adding optional `b`: compatible closed, unprovable partial.
fn level(partial: bool, with_b: bool) -> serde_json::Value {
    let mut properties = json!({ "a": { "type": "string" } });
    if with_b {
        properties["b"] = json!({ "type": "string" });
    }
    let mut level = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if partial {
        level["patternProperties"] = json!({ "^b": { "type": "string" } });
    }
    level
}

/// A refuted traits contract refutes the revision even when the payload is
/// merely undecided: Incompatible dominates Unknown across the two contracts.
#[test]
fn an_undecided_payload_and_a_refuted_traits_contract_are_incompatible() {
    let with_host = |level: serde_json::Value| {
        let mut doc = host(PARENT);
        for (key, value) in level.as_object().unwrap() {
            doc[key] = value.clone();
        }
        doc
    };
    assert_eq!(
        traits_verdict(
            &with_host(level(true, false)),
            &with_host(level(true, true)),
            &scoped_enum(&["read", "write"]),
            &scoped_enum(&["read"]),
        ),
        CompatibilityVerdict::Incompatible,
    );
}

/// An undecided traits contract leaves a compatible payload undecided.
#[test]
fn a_compatible_payload_and_an_undecided_traits_contract_are_undecided() {
    let parent = host(PARENT);
    assert_eq!(
        traits_verdict(&parent, &parent, &level(true, false), &level(true, true)),
        CompatibilityVerdict::Unknown,
    );
}

/// A string trait `mode` limited to `modes`.
fn scoped_enum(modes: &[&str]) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "mode": { "type": "string", "enum": modes } },
    })
}

/// A traits contract that cannot be resolved names its side: the payload
/// compared cleanly, so "could not resolve" alone would send the operator to
/// the wrong document.
#[test]
fn an_unresolvable_traits_contract_fails_as_the_traits_side() {
    let store = GtsStore::new();
    let host = host(PARENT);
    let dangling = json!({ "$ref": "gts://gts.cf.core.example.absent.v1~" });
    let open = json!({});

    let error = evolution_comparison(
        &store,
        BaselineDoc::new(&host),
        CandidateDoc::new(&host),
        BaselineDoc::new(&dangling),
        CandidateDoc::new(&open),
        TraitsDeclaration::Kept,
    )
    .expect_err("the baseline traits contract has a dangling reference");
    assert!(
        matches!(error, ComparisonFailure::TraitsSchema(_)),
        "{error:?}"
    );
    assert!(
        error.to_string().starts_with("x-gts-traits-schema: "),
        "{error}"
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

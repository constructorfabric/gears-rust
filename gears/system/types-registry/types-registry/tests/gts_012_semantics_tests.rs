#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Pins the `gts-rust` 0.12.0 capabilities that DESIGN §4 requires and 0.11.0
//! did not provide (SPEC §7, prerequisites 1, 2, 3, 5 and 6).
//!
//! These are upgrade-guard tests, not compatibility-policy tests: they assert
//! that the tri-state verdict, the per-level content-model classification and
//! the document-level comparison entry point exist and behave as SPEC §7
//! describes. The admission policy built on them — `Unknown` rejected with a
//! reason distinct from `Incompatible`, `candidate_object_levels` surfaced in
//! Dry Run — arrives with the compatibility slice.
//!
//! Under 0.11.0 this file does not compile: `CompatibilityVerdict`,
//! `ContentModel` and `GtsStore::compare_documents` do not exist there.

use gts::schema_evolution::classify_object_levels;
use gts::{
    CompatibilityFinding, CompatibilityVerdict, ContentModel, GTS_IMPLEMENTATION_VERSION,
    GTS_SPECIFICATION_VERSION, GtsStore,
};
use serde_json::json;

/// Prerequisite 6: the entry point resolves both documents itself, so the
/// content model is read from the effective schema rather than the authored
/// one. An optional property added at a *closed* level keeps every instance
/// the old definition accepted.
#[test]
fn compare_documents_reports_backward_compatible_addition_at_a_closed_level() {
    let store = GtsStore::new();
    let old_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "a": { "type": "string" } },
    });
    let new_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "string" },
        },
    });

    let comparison = store
        .compare_documents(&old_schema, &new_schema)
        .expect("both documents are self-contained, so resolution cannot fail");

    assert_eq!(
        comparison.backward_compatibility(),
        CompatibilityVerdict::Compatible,
        "backward diagnostics: {:?}",
        comparison.backward_diagnostics,
    );
    assert!(comparison.backward_diagnostics.is_empty());
}

/// Prerequisite 4: the same addition at an *open* level is incompatible, and
/// the diagnostic names the offending level rather than the document root —
/// which is what ADR-0003's Dry Run has to report.
#[test]
fn compare_documents_names_the_level_that_prevents_admission() {
    let store = GtsStore::new();
    let old_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "payload": { "type": "object" },
        },
    });
    let new_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "payload": {
                "type": "object",
                "properties": { "b": { "type": "string" } },
            },
        },
    });

    let comparison = store
        .compare_documents(&old_schema, &new_schema)
        .expect("self-contained documents resolve");

    assert_eq!(
        comparison.backward_compatibility(),
        CompatibilityVerdict::Incompatible,
    );
    let diagnostic = comparison
        .backward_diagnostics
        .iter()
        .find(|d| d.path == "$.payload")
        .expect("the offending level must be named, not the document root");
    assert_eq!(diagnostic.finding, CompatibilityFinding::PropertyAdded);
}

/// Prerequisite 1, the reason the upgrade is not optional: an undecidable
/// relation reports `Unknown`, which is a different value from
/// `Incompatible`. P0 rejects it with its own reason, so the two must never
/// collapse into one.
#[test]
fn an_undecidable_relation_is_unknown_and_not_incompatible() {
    let store = GtsStore::new();
    let old_schema = json!({ "type": "string", "pattern": "^a+$" });
    let new_schema = json!({ "type": "string", "pattern": "^[ab]+$" });

    let comparison = store
        .compare_documents(&old_schema, &new_schema)
        .expect("self-contained documents resolve");

    let verdict = comparison.backward_compatibility();
    assert!(verdict.is_unknown(), "got {verdict}");
    assert!(!verdict.is_incompatible());
    assert_eq!(verdict.as_str(), "unknown");
    assert!(
        comparison
            .backward_diagnostics
            .iter()
            .all(|d| d.finding == CompatibilityFinding::NotProvable),
        "{:?}",
        comparison.backward_diagnostics,
    );
}

/// Prerequisites 2 and 3: every object level of the resolved candidate is
/// classified, a partially open level is reported as `Partial` rather than
/// guessed into `Open` or `Closed`, and only a closed level is evolvable in
/// place.
#[test]
fn candidate_object_levels_classify_every_level_including_partial() {
    let store = GtsStore::new();
    let candidate = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "payload": { "type": "object" },
            "labels": {
                "type": "object",
                "additionalProperties": { "type": "string" },
            },
        },
    });

    let comparison = store
        .compare_documents(&candidate, &candidate)
        .expect("self-contained documents resolve");

    let level = |path: &str| {
        comparison
            .candidate_object_levels
            .iter()
            .find(|l| l.path == path)
            .unwrap_or_else(|| {
                panic!(
                    "level {path} missing: {:?}",
                    comparison.candidate_object_levels
                )
            })
            .content_model
    };

    assert_eq!(level("$"), ContentModel::Closed);
    assert_eq!(level("$.payload"), ContentModel::Open);
    assert_eq!(level("$.labels"), ContentModel::Partial);

    // Evolvability is exactly closure: a partially open level is reported as
    // not evolvable rather than guessed.
    assert!(ContentModel::Closed.is_evolvable_in_place());
    assert!(!ContentModel::Open.is_evolvable_in_place());
    assert!(!ContentModel::Partial.is_evolvable_in_place());

    // `classify_object_levels` is the same classification, reachable without a
    // comparison — the Dry Run path uses it on the candidate alone.
    assert_eq!(
        classify_object_levels(&candidate).len(),
        comparison.candidate_object_levels.len(),
    );
}

/// Guards the one failure mode `[patch.crates-io]` has: an override that stops
/// matching the version requirement is recorded as `[[patch.unused]]` — a cargo
/// *warning* — and the workspace silently resolves the published 0.11.0 instead.
/// That happened once already (SPEC §7), and it was caught only because the
/// imports above stop compiling. This says it out loud, with a diagnosis.
#[test]
fn workspace_did_not_silently_fall_back_to_gts_0_11() {
    assert_eq!(GTS_SPECIFICATION_VERSION, "0.13");
    assert!(
        !GTS_IMPLEMENTATION_VERSION.starts_with("0.11."),
        "resolved gts {GTS_IMPLEMENTATION_VERSION}: the [patch.crates-io] override is not in \
         effect. Check `Cargo.lock` for [[patch.unused]] entries and that the workspace \
         requirements match the local checkout's version — see SPEC §7."
    );
}

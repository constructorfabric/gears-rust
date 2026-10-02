//! Pure quarantine and dialect-pin tests; worker coverage is in
//! `tests/quarantine_test.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use gts::{GtsId, GtsIdSegment};
use serde_json::{Value, json};
use toolkit_gts::gts_id;

use super::{DialectDrift, dialect_pin, quarantine};
use crate::domain::admission::AdmissionFailureReason as Reason;
use crate::domain::compat::{BaselineDoc, CandidateDoc};
use crate::domain::dependency::extract_edges;

/// [`dialect_pin`] with each side named, so a test reads `(baseline, candidate)`
/// without repeating the wrappers at every call.
fn pin(baseline: &Value, candidate: &Value) -> Result<(), DialectDrift> {
    dialect_pin(BaselineDoc::new(baseline), CandidateDoc::new(candidate))
}
use crate::domain::enums::DependencyKind;

const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
const DRAFT_2020: &str = "https://json-schema.org/draft/2020-12/schema";

const STABLE: &str = gts_id!("cf.core.quar.thing.v1~");
const UNSTABLE: &str = gts_id!("cf.core.quar.other.v0~");
const STABLE_TARGET: &str = gts_id!("cf.core.quar.other.v1~");

/// A Type Schema document with the given `$ref` targets under `properties`.
fn schema(gts_id: &str, refs: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    for (index, target) in refs.iter().enumerate() {
        properties.insert(
            format!("p{index}"),
            json!({ "$ref": format!("gts://{target}") }),
        );
    }
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": DRAFT_07,
        "type": "object",
        "properties": Value::Object(properties),
    })
}

/// Evaluate quarantine over the same extracted edges the worker uses.
fn verdict(gts_id: &str, content: &Value) -> Result<(), Reason> {
    let id = GtsId::try_new(gts_id).unwrap_or_else(|e| panic!("{gts_id}: {e}"));
    let edges = extract_edges(&id, content).expect("the fixtures' refs are well formed");
    quarantine(&id, &edges).map_err(|breach| breach.reason())
}

// ---------------------------------------------------------------------------
// The quarantine (ADR-0015)
// ---------------------------------------------------------------------------

/// The immediate base is a segment of the candidate's own identifier, so a stable
/// type deriving from an unstable one is refused with nothing loaded.
#[test]
fn a_stable_candidate_deriving_from_a_major_zero_base_is_refused() {
    let derived = gts_id!("cf.core.quar.other.v0~cf.core.quar.leaf.v1~");
    assert_eq!(
        verdict(derived, &schema(derived, &[])),
        Err(Reason::StableDerivesFromMajorZero),
    );
}

/// A floating `$ref` must not let an unstable target redefine a stable schema.
#[test]
fn a_stable_candidate_referencing_a_major_zero_target_is_refused() {
    assert_eq!(
        verdict(STABLE, &schema(STABLE, &[UNSTABLE])),
        Err(Reason::StableRefsMajorZero),
    );
}

/// The marker sits in a *preceding* segment here: the Instance's own last segment
/// is stable, and what is quarantined is the schema it conforms to.
#[test]
fn an_instance_of_a_major_zero_schema_is_refused() {
    let instance = gts_id!("cf.core.quar.other.v0~cf.core.quar.first.v1");
    assert_eq!(
        verdict(instance, &json!({ "a": 1 })),
        Err(Reason::InstanceOfMajorZero),
    );
}

/// Weaker on stronger is sound and is the normal case — a type under development
/// usually derives from a published base — so the relation is one-way.
#[test]
fn an_unstable_candidate_may_derive_from_and_reference_anything() {
    let derived = gts_id!("cf.core.quar.other.v0~cf.core.quar.leaf.v0~");
    assert_eq!(verdict(derived, &schema(derived, &[UNSTABLE])), Ok(()));
    assert_eq!(
        verdict(UNSTABLE, &schema(UNSTABLE, &[STABLE_TARGET])),
        Ok(())
    );
}

/// The ordinary case must not be refused by a rule that reads "major 0" out of
/// any segment it finds one in: `v1~` prefixes and `v1` tails are everywhere.
#[test]
fn a_stable_candidate_built_only_on_stable_targets_is_admitted() {
    let derived = gts_id!("cf.core.quar.other.v1~cf.core.quar.leaf.v1~");
    assert_eq!(verdict(derived, &schema(derived, &[STABLE_TARGET])), Ok(()));
    let instance = gts_id!("cf.core.quar.other.v1~cf.core.quar.first.v1");
    assert_eq!(verdict(instance, &json!({ "a": 1 })), Ok(()));
}

/// Only the immediate base is this candidate's edge. An intermediate stable
/// type must reject its own v0 base, so quarantine need not scan ancestors.
#[test]
fn the_quarantine_names_the_immediate_base_and_not_a_deeper_ancestor() {
    let id = gts_id!("cf.core.quar.other.v0~cf.core.quar.mid.v1~cf.core.quar.leaf.v1~");
    let parsed = GtsId::try_new(id).expect("a canonical three-segment chain");
    let chain = parsed.chain_ids();
    let edges = extract_edges(&parsed, &schema(id, &[])).expect("no refs");
    assert_eq!(quarantine(&parsed, &edges), Ok(()));
    assert_eq!(
        chain[chain.len() - 2],
        gts_id!("cf.core.quar.other.v0~cf.core.quar.mid.v1~"),
        "the immediate base is stable; the v0 entity is its base, not this one's",
    );
}

/// `x-gts-ref` constrains an instance value and resolves nothing, so it is
/// outside the rule — exactly, and through a pattern.
#[test]
fn an_x_gts_ref_naming_major_zero_is_outside_the_quarantine() {
    for target in [UNSTABLE.to_owned(), format!("{UNSTABLE}*")] {
        let mut content = schema(STABLE, &[]);
        content["properties"] = json!({ "role": { "type": "string", "x-gts-ref": target } });
        assert_eq!(
            verdict(STABLE, &content),
            Ok(()),
            "x-gts-ref '{target}' is an instance-value constraint, not a dependency",
        );
    }
}

/// Each edge kind must render the correct target, reason, and verb.
#[test]
fn each_edge_kind_refuses_under_its_own_reason_and_names_its_target() {
    let derived = gts_id!("cf.core.quar.other.v0~cf.core.quar.leaf.v1~");
    let cases = [
        (
            derived,
            schema(derived, &[]),
            DependencyKind::Derivation,
            UNSTABLE,
            Reason::StableDerivesFromMajorZero,
            "derive from",
        ),
        (
            STABLE,
            schema(STABLE, &[UNSTABLE]),
            DependencyKind::SchemaRef,
            UNSTABLE,
            Reason::StableRefsMajorZero,
            "$ref",
        ),
        (
            gts_id!("cf.core.quar.other.v0~cf.core.quar.first.v1"),
            json!({ "a": 1 }),
            DependencyKind::InstanceOf,
            UNSTABLE,
            Reason::InstanceOfMajorZero,
            "conform to",
        ),
    ];
    let mut codes = Vec::new();
    let mut verbs = Vec::new();
    for (gts_id, content, kind, target, reason, verb) in cases {
        let id = GtsId::try_new(gts_id).expect("canonical");
        let edges = extract_edges(&id, &content).expect("well-formed refs");
        let breach = quarantine(&id, &edges).expect_err("the target is major 0");
        assert_eq!(breach.gts_id, gts_id);
        assert_eq!(breach.kind, kind);
        assert_eq!(breach.target, target);
        assert_eq!(breach.reason(), reason);
        // Check the full sentence to detect swapped edge verbs.
        assert_eq!(
            breach.to_string(),
            format!("'{gts_id}' is stable, so it may not {verb} major-0 '{target}' (ADR-0015)"),
        );
        codes.push(breach.reason().metric_label());
        verbs.push(verb);
    }
    verbs.sort_unstable();
    verbs.dedup();
    assert_eq!(
        verbs.len(),
        3,
        "two edge kinds render the same verb, so the message cannot say which rule broke",
    );
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(
        codes.len(),
        3,
        "the three reasons must not collapse into one"
    );
}

/// Treat an unreadable candidate major as stable. Acceptance and baseline
/// selection reject this UUID-tail case before quarantine on the write path.
#[test]
fn a_candidate_with_no_readable_major_of_its_own_is_treated_as_stable() {
    const UUID_TAIL: &str = "7a1d2f34-5678-49ab-9012-abcdef123456";
    let id = GtsId::try_new(&format!("{UNSTABLE}{UUID_TAIL}"))
        .expect("an explicit UUID tail is a parseable identifier");
    assert_eq!(
        id.segments().last().and_then(GtsIdSegment::ver_major_opt),
        None,
        "the fixture only tests what it means to if the last segment has no major",
    );
    let edges = extract_edges(&id, &json!({ "a": 1 })).expect("an instance value has no refs");
    assert_eq!(
        quarantine(&id, &edges).map_err(|breach| breach.reason()),
        Err(Reason::InstanceOfMajorZero),
    );
}

// ---------------------------------------------------------------------------
// The dialect pin (ADR-0014)
// ---------------------------------------------------------------------------

fn document_declaring(dialect: &str) -> Value {
    json!({ "$id": format!("gts://{STABLE}"), "$schema": dialect, "type": "object" })
}

/// There is one Draft-07 meta-schema, so a trailing `#` and the `https` scheme
/// carry no semantic content and must not read as a dialect change.
#[test]
fn the_same_dialect_spelled_differently_is_not_a_change() {
    for spelling in [
        "http://json-schema.org/draft-07/schema",
        "https://json-schema.org/draft-07/schema#",
        "https://json-schema.org/draft-07/schema",
    ] {
        assert_eq!(
            pin(&document_declaring(DRAFT_07), &document_declaring(spelling)),
            Ok(()),
            "'{spelling}' is the pinned dialect under another spelling",
        );
    }
}

/// A successor must retain the major's dialect so comparison uses shared semantics.
#[test]
fn a_changed_dialect_is_refused_before_anything_is_compared() {
    let drift = pin(
        &document_declaring(DRAFT_07),
        &document_declaring(DRAFT_2020),
    )
    .expect_err("the pair declares two dialects");
    assert_eq!(
        drift,
        DialectDrift {
            pinned: Some(DRAFT_07.to_owned()),
            declared: Some(DRAFT_2020.to_owned()),
        },
    );
    assert_eq!(DialectDrift::REASON, Reason::DialectChanged);
}

/// Missing `$schema` pins nothing and matches nothing, including another absence
/// (GTS §11.1 Rule A).
#[test]
fn an_absent_dialect_pins_nothing_on_either_side() {
    let bare = json!({ "$id": format!("gts://{STABLE}"), "type": "object" });
    assert!(pin(&bare, &document_declaring(DRAFT_07)).is_err());
    assert!(pin(&document_declaring(DRAFT_07), &bare).is_err());
    assert!(pin(&bare, &bare).is_err());
}

//! Building a definition version (P-D-270, P-D-271) from stored rows alone: the digest covers
//! every pinned dependency, an equal record reuses its lineage's mapping and a changed one takes
//! the next version, a named definition takes its next version, and the version is unavailable
//! until qualification.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::domain::quota::review::Proposal;
use crate::quota_test_support::{
    ALLOCATED_VCPU, TEST_PLUGIN, VCPU_METRIC, proposal_of, submitted_row, vcpu_record,
};
use serde_json::json;

fn target() -> QuotaTarget {
    QuotaTarget {
        id: Uuid::from_u128(0x7a),
        tenant_id: Uuid::from_u128(0x7e),
        plugin_instance_id: TEST_PLUGIN.to_owned(),
        target_key: "region-1".to_owned(),
        name: "Region 1".to_owned(),
        created_by: Uuid::from_u128(0xa1),
        created_at: at(),
    }
}

fn at() -> OffsetDateTime {
    crate::infra::storage::stored_now()
}

const APPROVER: Uuid = Uuid::from_u128(0xa9);

fn candidate(record: serde_json::Value, proposal: &Proposal) -> QuotaCandidate {
    submitted_row(
        record,
        proposal,
        target().id,
        (Uuid::from_u128(0xa1), Uuid::from_u128(0xa2)),
    )
}

fn v(n: u64) -> QuotaVersion {
    QuotaVersion::new(n).unwrap()
}

#[test]
fn the_definition_digest_covers_the_pinned_dependencies() {
    let named = Uuid::new_v4();
    let a = candidate(
        vcpu_record(),
        &proposal_of(&vcpu_record(), Uuid::new_v4(), 3, Some(named)),
    );
    let lineage = build(&a, &target(), None, Some(v(1)), APPROVER, at())
        .unwrap()
        .mapping;
    let digest = |c: &QuotaCandidate| {
        build(c, &target(), Some(&lineage), Some(v(1)), APPROVER, at())
            .unwrap()
            .definition
            .digest
    };
    assert_eq!(
        digest(&a),
        digest(&a),
        "the same rows build the same digest"
    );
    let mut other_policy = a.clone();
    other_policy.evidence_json[ALLOCATED_VCPU]["digest"] = json!("f".repeat(64));
    assert_ne!(
        digest(&other_policy),
        digest(&a),
        "the accounting policy's evidence"
    );
    let mut other_text = a.clone();
    other_text.proposal_json.as_mut().unwrap()["profile_conditions"]["de"] =
        json!("Gestoppte VMs zaehlen.");
    assert_ne!(
        digest(&other_text),
        digest(&a),
        "a condition in another locale"
    );
    let built = build(&a, &target(), Some(&lineage), Some(v(1)), APPROVER, at()).unwrap();
    let content = &built.definition.content_json;
    assert_eq!(content["metric"]["ref"], VCPU_METRIC);
    assert_eq!(
        content["metric"]["content"]["display_name"]["en"],
        "Virtual CPUs"
    );
    assert_eq!(content["allowed_units"][0]["max"], "1024");
    assert_eq!(
        content["supported_bases"],
        json!(["fixed", "per_plan_unit"])
    );
    assert_eq!(content["sku_version"], "3");
    assert_eq!(
        bss_products_sdk::quota::digest::definition_digest(content),
        built.definition.digest
    );
}

#[test]
fn an_equal_record_reuses_the_lineages_mapping() {
    let proposal = proposal_of(&vcpu_record(), Uuid::new_v4(), 1, None);
    let first = build(
        &candidate(vcpu_record(), &proposal),
        &target(),
        None,
        None,
        APPROVER,
        at(),
    )
    .unwrap();
    assert!(!first.reuse_mapping);
    assert_eq!(
        (first.mapping.version, first.mapping.profile_version),
        (v(1), v(1))
    );
    // The mapping carries what drift compares: the record's digest and its evidence.
    let c = candidate(vcpu_record(), &proposal);
    assert_eq!(
        first.mapping.mapping_json["record_digest"],
        json!(c.record_digest)
    );
    assert_eq!(first.mapping.mapping_json["evidence"], c.evidence_json);
    let second = build(&c, &target(), Some(&first.mapping), None, APPROVER, at()).unwrap();
    assert!(second.reuse_mapping);
    assert_eq!(second.mapping, first.mapping);
    assert_eq!(second.definition.mapping_version, v(1));
}

#[test]
fn a_changed_mapping_key_is_the_next_mapping_version() {
    let proposal = proposal_of(&vcpu_record(), Uuid::new_v4(), 1, None);
    let first = build(
        &candidate(vcpu_record(), &proposal),
        &target(),
        None,
        None,
        APPROVER,
        at(),
    )
    .unwrap();
    let mut changed = vcpu_record();
    changed["mapping_key"] = json!("tenant-vcpu-mapping-4");
    let second = build(
        &candidate(changed, &proposal),
        &target(),
        Some(&first.mapping),
        None,
        APPROVER,
        at(),
    )
    .unwrap();
    assert!(!second.reuse_mapping);
    assert_eq!(
        (second.mapping.mapping_id, second.mapping.profile_id),
        (first.mapping.mapping_id, first.mapping.profile_id)
    );
    assert_eq!(
        (second.mapping.version, second.mapping.profile_version),
        (v(2), v(2))
    );
    assert_eq!(second.mapping.mapping_key, "tenant-vcpu-mapping-4");
    assert_eq!(
        (
            second.definition.mapping_id,
            second.definition.mapping_version
        ),
        (first.mapping.mapping_id, v(2))
    );
}

#[test]
fn a_named_definition_takes_its_next_version() {
    let named = Uuid::new_v4();
    let proposal = proposal_of(&vcpu_record(), Uuid::new_v4(), 1, Some(named));
    let built = build(
        &candidate(vcpu_record(), &proposal),
        &target(),
        None,
        Some(v(3)),
        APPROVER,
        at(),
    )
    .unwrap();
    assert_eq!(
        (built.definition.definition_id, built.definition.version),
        (named, v(4))
    );
    let fresh = proposal_of(&vcpu_record(), Uuid::new_v4(), 1, None);
    let built = build(
        &candidate(vcpu_record(), &fresh),
        &target(),
        None,
        None,
        APPROVER,
        at(),
    )
    .unwrap();
    assert_eq!(built.definition.version, v(1));
    assert_ne!(built.definition.definition_id, named);
}

#[test]
fn the_availability_is_disabled_pending_qualification() {
    let proposal = proposal_of(&vcpu_record(), Uuid::new_v4(), 1, None);
    let built = build(
        &candidate(vcpu_record(), &proposal),
        &target(),
        None,
        None,
        APPROVER,
        at(),
    )
    .unwrap();
    let a = &built.availability;
    assert_eq!(
        (a.definition_id, a.version, a.revision),
        (
            built.definition.definition_id,
            built.definition.version,
            v(1)
        )
    );
    assert_eq!(a.state, QuotaAvailabilityState::Disabled);
    assert_eq!(a.reason_code.as_deref(), Some(QUALIFICATION_PENDING));
    assert_eq!((a.qualification.as_ref(), a.decided_by), (None, APPROVER));
}

/// A candidate row that does not decode is a message naming its member, never a panic.
#[test]
fn a_stored_candidate_without_its_proposal_does_not_build() {
    let proposal = proposal_of(&vcpu_record(), Uuid::new_v4(), 1, None);
    let mut c = candidate(vcpu_record(), &proposal);
    c.proposal_evidence_json = None;
    let why = build(&c, &target(), None, None, APPROVER, at()).unwrap_err();
    assert!(why.contains("proposal_evidence_json"), "{why}");
}

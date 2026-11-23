//! Planning one intake against the target's approved mappings (P-D-268): drift, retired and
//! rejected notes, one candidate per capability key, and revisions ordered as numbers.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::domain::quota::evidence::Evidence;
use crate::domain::quota::record::{Resolutions, judge_record};
use crate::quota_test_support::{FakeQuotaRegistry, seed_catalog, vcpu_record_numbered};
use crate::test_support::authed_ctx;
use bss_products_sdk::quota::catalog::QuotaVersion;
use serde_json::json;
use std::cmp::Ordering;
use time::OffsetDateTime;

async fn judged(records: &[serde_json::Value]) -> Vec<JudgedRecord> {
    let registry = seed_catalog(FakeQuotaRegistry::default());
    let ctx = authed_ctx(Uuid::new_v4());
    let mut memo = Resolutions::default();
    let mut out = Vec::new();
    for raw in records {
        out.push(judge_record(&registry, &ctx, raw, &mut memo).await.unwrap());
    }
    out
}

/// The approved mapping of `judged`'s capability, carrying its record and evidence digests.
fn approved(judged: &JudgedRecord, version: u64) -> MappingVersion {
    MappingVersion {
        mapping_id: Uuid::new_v4(),
        version: QuotaVersion::new(version).unwrap(),
        target_id: Uuid::nil(),
        capability_key: judged.record.capability_key.clone(),
        plugin_instance_id: "plugin".to_owned(),
        target_key: "pool".to_owned(),
        mapping_key: judged.record.mapping_key.clone(),
        mapping_json: json!({
            "record_digest": judged.record_digest,
            "evidence": judged.evidence.to_json(),
        }),
        digest: "e".repeat(64),
        profile_id: Uuid::new_v4(),
        profile_version: QuotaVersion::new(1).unwrap(),
        profile_json: json!({}),
        profile_digest: "f".repeat(64),
        created_by: Uuid::nil(),
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn rejection(record: &serde_json::Value, code: &'static str) -> RejectedRecord {
    RejectedRecord {
        record: record["id"].as_str().unwrap().to_owned(),
        capability_key: record["capability_key"].as_str().map(str::to_owned),
        refusal: RecordRefusal {
            code,
            field: "metric_ref".to_owned(),
            detail: "x".to_owned(),
        },
    }
}

#[tokio::test]
async fn drift_is_a_changed_digest_of_an_approved_capability() {
    let records = [
        vcpu_record_numbered(1),
        vcpu_record_numbered(2),
        vcpu_record_numbered(3),
    ];
    let now = judged(&records).await;
    // Approved from an older record: another max.
    let mut older = records[0].clone();
    older["limits"]["max"] = json!("64");
    let then = judged(&[older]).await.remove(0);
    let record_moved = approved(&then, 2);
    // Approved from the same record, but the registry's metric content moved since.
    let mut evidence_moved = approved(&now[1], 1);
    let mut evidence = now[1].evidence.clone();
    let metric = evidence
        .0
        .values_mut()
        .find(|e| e.gts_id.contains("quota_metric"))
        .unwrap();
    *metric = Evidence {
        digest: "0".repeat(64),
        ..metric.clone()
    };
    evidence_moved.mapping_json["evidence"] = evidence.to_json();
    let unchanged = approved(&now[2], 1);
    let plan = plan_intake(
        now.iter().cloned().map(Ok).collect(),
        &[record_moved.clone(), evidence_moved.clone(), unchanged],
    );
    assert_eq!(
        plan.candidates.len(),
        3,
        "drift changes nothing published or staged"
    );
    assert_eq!(
        plan.drift,
        vec![
            DriftNote {
                capability_key: "tenant-vcpu-1".to_owned(),
                mapping_id: record_moved.mapping_id,
                mapping_version: record_moved.version,
            },
            DriftNote {
                capability_key: "tenant-vcpu-2".to_owned(),
                mapping_id: evidence_moved.mapping_id,
                mapping_version: evidence_moved.version,
            },
        ]
    );
    assert!(plan.retired.is_empty());
    let notes = review_json(&plan);
    assert_eq!(notes["drift"][0]["capability_key"], "tenant-vcpu-1");
    assert_eq!(notes["drift"][0]["mapping_version"], "2");
    assert_eq!(notes["rejected"], json!([]));
    assert_eq!(notes["retired"], json!([]));
}

#[tokio::test]
async fn retired_is_an_approved_capability_absent_from_the_snapshot() {
    let records = [vcpu_record_numbered(1), vcpu_record_numbered(2)];
    let all = judged(&records).await;
    let gone = approved(&all[1], 3);
    let kept = approved(&all[0], 1);
    let plan = plan_intake(vec![Ok(all[0].clone())], &[kept, gone.clone()]);
    assert_eq!(
        plan.retired,
        vec![RetiredNote {
            capability_key: "tenant-vcpu-2".to_owned(),
            mapping_id: gone.mapping_id,
            mapping_version: gone.version,
        }]
    );
    assert!(plan.drift.is_empty());
    let notes = review_json(&plan);
    assert_eq!(
        notes["retired"][0]["mapping_id"],
        json!(gone.mapping_id.to_string())
    );
}

#[tokio::test]
async fn a_rejected_record_is_neither_drift_nor_retired() {
    let records = [vcpu_record_numbered(1), vcpu_record_numbered(2)];
    let all = judged(&records).await;
    let mapping = approved(&all[1], 1);
    let plan = plan_intake(
        vec![
            Ok(all[0].clone()),
            Err(rejection(&records[1], "QUOTA_REFERENCE_UNKNOWN")),
        ],
        &[mapping],
    );
    assert_eq!(plan.candidates.len(), 1);
    assert!(plan.drift.is_empty() && plan.retired.is_empty(), "{plan:?}");
    let notes = review_json(&plan);
    assert_eq!(
        notes["rejected"],
        json!([{
            "record": records[1]["id"],
            "capability_key": "tenant-vcpu-2",
            "code": "QUOTA_REFERENCE_UNKNOWN",
            "field": "metric_ref",
            "detail": "x"
        }])
    );
}

/// Two valid records of one snapshot that claim one capability key are both rejected: neither
/// can be the capability, and the store holds one candidate per key.
#[tokio::test]
async fn a_capability_key_claimed_twice_rejects_both_records() {
    let mut twin = vcpu_record_numbered(2);
    twin["capability_key"] = json!("tenant-vcpu-1");
    let all = judged(&[vcpu_record_numbered(1), twin, vcpu_record_numbered(3)]).await;
    let plan = plan_intake(all.into_iter().map(Ok).collect(), &[]);
    assert_eq!(
        plan.candidates
            .iter()
            .map(|c| c.record.capability_key.as_str())
            .collect::<Vec<_>>(),
        vec!["tenant-vcpu-3"]
    );
    assert_eq!(plan.rejected.len(), 2);
    for rejected in &plan.rejected {
        assert_eq!(
            (rejected.refusal.code, rejected.refusal.field.as_str()),
            ("QUOTA_RECORD_INVALID", "capability_key")
        );
    }
}

/// A capability key that a rejected record names too rejects every record that names it: the valid
/// one becomes no candidate, and the rejected one keeps its own refusal.
#[tokio::test]
async fn a_capability_key_named_by_a_rejected_record_rejects_the_valid_one_too() {
    let records = [vcpu_record_numbered(1), vcpu_record_numbered(2)];
    let all = judged(&records).await;
    let mut twin = vcpu_record_numbered(3);
    twin["capability_key"] = json!("tenant-vcpu-1");
    let plan = plan_intake(
        vec![
            Ok(all[0].clone()),
            Err(rejection(&twin, "QUOTA_REFERENCE_UNKNOWN")),
            Ok(all[1].clone()),
        ],
        &[],
    );
    assert_eq!(
        plan.candidates
            .iter()
            .map(|c| c.record.capability_key.as_str())
            .collect::<Vec<_>>(),
        vec!["tenant-vcpu-2"]
    );
    let codes: Vec<_> = plan
        .rejected
        .iter()
        .map(|r| (r.record.clone(), r.refusal.code, r.refusal.field.clone()))
        .collect();
    assert_eq!(
        codes,
        vec![
            (
                records[0]["id"].as_str().unwrap().to_owned(),
                "QUOTA_RECORD_INVALID",
                "capability_key".to_owned()
            ),
            (
                twin["id"].as_str().unwrap().to_owned(),
                "QUOTA_REFERENCE_UNKNOWN",
                "metric_ref".to_owned()
            ),
        ]
    );
}

#[test]
fn revisions_order_as_numbers() {
    assert_eq!(revision_order("9", "10"), Ordering::Less);
    assert_eq!(revision_order("10", "9"), Ordering::Greater);
    assert_eq!(revision_order("10", "10"), Ordering::Equal);
    assert_eq!(revision_order("0", "1"), Ordering::Less);
    assert_eq!(
        revision_order(&"9".repeat(39), &"1".repeat(39)),
        Ordering::Greater
    );
    assert_eq!(
        revision_order(&"9".repeat(38), &format!("1{}", "0".repeat(38))),
        Ordering::Less
    );
}

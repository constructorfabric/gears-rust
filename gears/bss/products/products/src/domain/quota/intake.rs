//! Planning one intake (P-D-268), purely: which judged records become candidates, which are
//! rejected, and the notes against the target's approved mappings. Nothing published changes on
//! a note; an administrator reads them.
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use bss_products_sdk::quota::catalog::QuotaVersion;
use serde_json::{Value, json};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::MappingVersion;
use super::evidence::EvidenceSet;
use super::record::{JudgedRecord, QUOTA_RECORD_INVALID, RecordRefusal};

/// A record of the snapshot that becomes no candidate, with its refusal.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRecord {
    /// The record's own `id` text, or `#<index>` in the snapshot when it has none.
    pub record: String,
    /// The record's capability key, when it names one.
    pub capability_key: Option<String>,
    pub refusal: RecordRefusal,
}

impl RejectedRecord {
    /// The rejection of the raw record at `index` of the snapshot.
    #[must_use]
    pub fn of(index: usize, raw: &Value, refusal: RecordRefusal) -> Self {
        Self {
            record: raw
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("#{index}"), str::to_owned),
            capability_key: raw
                .get("capability_key")
                .and_then(Value::as_str)
                .map(str::to_owned),
            refusal,
        }
    }
}

/// What one intake stores: the candidates, ordered by capability key, and the notes.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakePlan {
    pub candidates: Vec<JudgedRecord>,
    pub rejected: Vec<RejectedRecord>,
    pub drift: Vec<DriftNote>,
    pub retired: Vec<RetiredNote>,
}

/// A valid record whose capability has an approved mapping with other digests.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftNote {
    pub capability_key: String,
    pub mapping_id: Uuid,
    pub mapping_version: QuotaVersion,
}

/// An approved mapping whose capability the complete snapshot no longer holds.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredNote {
    pub capability_key: String,
    pub mapping_id: Uuid,
    pub mapping_version: QuotaVersion,
}

/// Notes against the target's approved mappings (P-D-268). Pure.
///
/// Every valid record becomes a candidate unless another record of the snapshot, valid or
/// rejected, names its capability key: then every record that names it is rejected, since none
/// can be the capability, and a rejected one keeps its own refusal. Drift is a
/// valid record whose record digest, or the digest of any entity in its evidence, differs from
/// the latest approved mapping of its capability (a mapping whose stored digests do not read
/// counts as differing). Retired is an approved mapping whose capability key no record of the
/// snapshot names, valid or rejected.
// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-notes
#[must_use]
pub fn plan_intake(
    judged: Vec<Result<JudgedRecord, RejectedRecord>>,
    approved: &[MappingVersion],
) -> IntakePlan {
    let mut claims: BTreeMap<String, usize> = BTreeMap::new();
    for key in judged.iter().filter_map(|result| match result {
        Ok(record) => Some(record.record.capability_key.as_str()),
        Err(rejected) => rejected.capability_key.as_deref(),
    }) {
        *claims.entry(key.to_owned()).or_default() += 1;
    }
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    for result in judged {
        match result {
            Ok(record) if claims.get(&record.record.capability_key) == Some(&1) => {
                candidates.push(record);
            }
            Ok(record) => rejected.push(RejectedRecord {
                record: record.record.id.to_string(),
                capability_key: Some(record.record.capability_key.clone()),
                refusal: RecordRefusal {
                    code: QUOTA_RECORD_INVALID,
                    field: "capability_key".to_owned(),
                    detail: "another record of the snapshot claims this capability key".to_owned(),
                },
            }),
            Err(rejection) => rejected.push(rejection),
        }
    }
    candidates.sort_by(|a, b| a.record.capability_key.cmp(&b.record.capability_key));
    let by_key: BTreeMap<&str, &MappingVersion> = approved
        .iter()
        .map(|mapping| (mapping.capability_key.as_str(), mapping))
        .collect();
    let drift = candidates
        .iter()
        .filter_map(|record| {
            let mapping = by_key.get(record.record.capability_key.as_str())?;
            drifted(record, mapping).then(|| DriftNote {
                capability_key: mapping.capability_key.clone(),
                mapping_id: mapping.mapping_id,
                mapping_version: mapping.version,
            })
        })
        .collect();
    let named: BTreeSet<&str> = candidates
        .iter()
        .map(|record| record.record.capability_key.as_str())
        .chain(rejected.iter().filter_map(|r| r.capability_key.as_deref()))
        .collect();
    let retired = approved
        .iter()
        .filter(|mapping| !named.contains(mapping.capability_key.as_str()))
        .map(|mapping| RetiredNote {
            capability_key: mapping.capability_key.clone(),
            mapping_id: mapping.mapping_id,
            mapping_version: mapping.version,
        })
        .collect();
    IntakePlan {
        candidates,
        rejected,
        drift,
        retired,
    }
}

/// Whether `record` differs from what `mapping` was approved from: its own digest, or the digest
/// of any entity of the evidence on either side.
fn drifted(record: &JudgedRecord, mapping: &MappingVersion) -> bool {
    let approved_digest = mapping
        .mapping_json
        .get("record_digest")
        .and_then(Value::as_str);
    let Some(approved_evidence) = mapping
        .mapping_json
        .get("evidence")
        .and_then(|value| EvidenceSet::from_json(value).ok())
    else {
        return true;
    };
    let digests = |set: &EvidenceSet| -> BTreeMap<String, String> {
        set.0
            .iter()
            .map(|(id, evidence)| (id.clone(), evidence.digest.clone()))
            .collect()
    };
    approved_digest != Some(record.record_digest.as_str())
        || digests(&approved_evidence) != digests(&record.evidence)
}

/// The notes as the snapshot's `review_json` stores them:
/// `{"rejected": [{record, capability_key, code, field, detail}], "drift": [{capability_key,
/// mapping_id, mapping_version}], "retired": [...]}`, versions as decimal strings.
#[must_use]
pub fn review_json(plan: &IntakePlan) -> Value {
    let rejected: Vec<Value> = plan
        .rejected
        .iter()
        .map(|r| {
            json!({
                "record": r.record,
                "capability_key": r.capability_key,
                "code": r.refusal.code,
                "field": r.refusal.field,
                "detail": r.refusal.detail,
            })
        })
        .collect();
    let note = |key: &str, id: Uuid, version: QuotaVersion| {
        json!({
            "capability_key": key,
            "mapping_id": id.to_string(),
            "mapping_version": version.to_string(),
        })
    };
    json!({
        "rejected": rejected,
        "drift": plan
            .drift
            .iter()
            .map(|d| note(&d.capability_key, d.mapping_id, d.mapping_version))
            .collect::<Vec<_>>(),
        "retired": plan
            .retired
            .iter()
            .map(|r| note(&r.capability_key, r.mapping_id, r.mapping_version))
            .collect::<Vec<_>>(),
    })
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-notes

/// Revisions compare as numbers: by length, then text (both canonical).
#[must_use]
pub fn revision_order(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod intake_tests;

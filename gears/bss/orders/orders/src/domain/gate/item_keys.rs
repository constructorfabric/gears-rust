//! Proposed Orders-local `item_keys_v1` evidence. Legacy scalars never become provider echoes.
use super::answers::{OverlapKey, OverlapKeyLine};
use bss_pricing_sdk::{
    acceptance::NewSaleQuery,
    read::{AcceptedBinding, ChargeKind},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Versioned classification provenance, supplied by the key provider.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationProvenance {
    /// Provider source.
    pub source: String,
    /// Immutable classification reference.
    pub reference: String,
    /// Classification version.
    pub version: String,
}

/// Complete evidence for exactly one requested recurring selection.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemOverlapKey {
    /// Exactly `item_keys_v1`.
    pub profile: String,
    /// Exact request identity echoed by the provider.
    pub selection: OverlapKeyLine,
    /// Exact provider product identity.
    pub catalog_subscription_product_key: String,
    /// Exact provider scope identity. Multiple products may share it.
    pub overlap_scope_key: String,
    /// Explicit classification provenance.
    pub classification: ClassificationProvenance,
}

/// Nonempty, NUL-free bounded provider evidence.
#[must_use]
pub fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.contains('\0')
}

impl ItemOverlapKey {
    /// Validate the complete profile without inferring missing evidence.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.profile == "item_keys_v1"
            && [
                &self.catalog_subscription_product_key,
                &self.overlap_scope_key,
                &self.classification.source,
                &self.classification.reference,
                &self.classification.version,
            ]
            .into_iter()
            .all(|s| valid_text(s))
            && self
                .selection
                .dimension_value
                .as_ref()
                .is_none_or(|s| valid_text(s))
    }
}

/// Exact coverage, canonical selection order, and strict legacy separation.
/// Invalid lines are omitted; foreign responses invalidate the entire answer before reads.
#[must_use]
pub fn validated_line_keys(
    expected: &[OverlapKeyLine],
    answered: &[OverlapKey],
) -> BTreeMap<Uuid, Vec<OverlapKey>> {
    let mut result = BTreeMap::new();
    if answered.len() > expected.len()
        || answered
            .iter()
            .any(|k| !expected.iter().any(|e| e.line_id == k.line_id))
    {
        return result;
    }
    let lines: BTreeSet<_> = expected.iter().map(|e| e.line_id).collect();
    for line in lines {
        let requests: Vec<_> = expected.iter().filter(|e| e.line_id == line).collect();
        let keys: Vec<_> = answered.iter().filter(|k| k.line_id == line).collect();
        if requests.len() != keys.len()
            || requests.iter().enumerate().any(|(i, e)| {
                requests[..i].iter().any(|other| {
                    other.item_id == e.item_id && other.dimension_value == e.dimension_value
                })
            })
        {
            continue;
        }
        if requests.len() == 1 && keys[0].item_key.is_none() {
            if valid_text(&keys[0].overlap_scope_key)
                && valid_text(&keys[0].catalog_subscription_product_key)
            {
                result.insert(line, vec![keys[0].clone()]);
            }
            continue;
        }
        let mut ordered = Vec::new();
        for request in requests {
            let matching: Vec<_> = keys
                .iter()
                .filter(|k| {
                    k.item_key.as_ref().is_some_and(|item| {
                        item.valid()
                            && item.selection == *request
                            && item.overlap_scope_key == k.overlap_scope_key
                            && item.catalog_subscription_product_key
                                == k.catalog_subscription_product_key
                    })
                })
                .collect();
            if let [only] = matching.as_slice() {
                ordered.push((***only).clone());
            }
        }
        if ordered.len() == keys.len() {
            ordered.sort_by(|a, b| {
                a.item_key
                    .as_ref()
                    .map(|k| (&k.selection.item_id, &k.selection.dimension_value))
                    .cmp(
                        &b.item_key
                            .as_ref()
                            .map(|k| (&k.selection.item_id, &k.selection.dimension_value)),
                    )
            });
            result.insert(line, ordered);
        }
    }
    result
}

/// The keys the gate reads occupancy and the in-flight lookups for, flattened in line-id order, or
/// `None` when the answer is rejected before reads (`SUBSCRIPTIONS_CONTRACT.md` exact coverage: an
/// omitted, extra, foreign, duplicate, mismatched or conflicting answer): a foreign or extra
/// entry, or any requested line without a validated collection. The orchestrator reads nothing
/// on `None`, and `assess` then reports predicates 7 and 9 unevaluable, never passed.
#[must_use]
pub fn read_keys(expected: &[OverlapKeyLine], answered: &[OverlapKey]) -> Option<Vec<OverlapKey>> {
    let kept: Vec<OverlapKey> = validated_line_keys(expected, answered)
        .into_values()
        .flatten()
        .collect();
    (kept.len() == expected.len()).then_some(kept)
}

/// Distinct scopes, retaining every item association in the source collection.
#[must_use]
pub fn distinct_scopes(keys: &[ItemOverlapKey]) -> BTreeSet<&str> {
    keys.iter().map(|k| k.overlap_scope_key.as_str()).collect()
}

/// Verify new frozen evidence against exact query selections and accepted receipt bindings.
#[must_use]
pub fn matches_receipt(
    keys: &[ItemOverlapKey],
    query: &NewSaleQuery,
    bindings: &[AcceptedBinding],
) -> bool {
    if bindings.len() != query.selections.len()
        || query
            .selections
            .iter()
            .zip(bindings)
            .any(|(s, b)| s.item_id != b.item_id)
    {
        return false;
    }
    let expected: Vec<_> = query
        .selections
        .iter()
        .zip(bindings)
        .filter(|(_, b)| b.kind == ChargeKind::Recurring)
        .map(|(s, b)| OverlapKeyLine {
            line_id: query.line_id,
            plan_revision_id: query.plan_revision_id,
            paid_recurring_sku: b.sku_id,
            item_id: s.item_id,
            dimension_value: s.dimension_value.clone(),
            sku_version: b.sku_version,
        })
        .collect();
    let answered: Vec<_> = keys
        .iter()
        .map(|k| OverlapKey {
            line_id: k.selection.line_id,
            catalog_subscription_product_key: k.catalog_subscription_product_key.clone(),
            overlap_scope_key: k.overlap_scope_key.clone(),
            item_key: Some(k.clone()),
        })
        .collect();
    keys.len() == expected.len()
        && (expected.is_empty()
            || validated_line_keys(&expected, &answered)
                .get(&query.line_id)
                .is_some_and(|v| v.len() == expected.len()))
}

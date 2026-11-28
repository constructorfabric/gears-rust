//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-review:p1
//! Building one definition version from a submitted candidate (P-D-270, P-D-271): purely, from
//! the stored candidate, its proposal and the entities the proposal pinned, with no registry or
//! plugin call. The mapping lineage `(target, capability_key)` keeps its latest version when the
//! digest is the same, else takes the next one, and the profile moves with it; the definition takes
//! the next version of the definition the proposal names, or version 1 of a new one; and the
//! version is recorded unavailable for sale until qualification.
use bss_products_sdk::quota::catalog::{QuotaAvailabilityState, QuotaVersion};
use bss_products_sdk::quota::digest::{definition_digest, mapping_digest, profile_digest};
use bss_products_sdk::quota::gts::{
    QuotaAccountingPolicyV1, QuotaCapabilityV1, QuotaMetricV1, QuotaScopePolicyV1,
};
use gts::GtsId;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::evidence::EvidenceSet;
use super::review::{Pinned, basis_token, check_profile, proposal_from_json};
use super::{Availability, DefinitionVersion, MappingVersion, QuotaCandidate, QuotaTarget};

/// Why an approved version is not available for sale yet (P-D-271).
pub const QUALIFICATION_PENDING: &str = "qualification_pending";

/// What one approval writes (P-D-270).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct DefinitionDraft {
    /// The lineage's mapping version the definition names: a new one, or the latest reused.
    pub mapping: MappingVersion,
    /// The lineage's latest mapping has the same digests: nothing is written for it.
    pub reuse_mapping: bool,
    pub definition: DefinitionVersion,
    /// The deterministic UUIDs of the resource types the definition counts.
    pub resource_type_uuids: Vec<Uuid>,
    /// Revision 1, disabled, pending qualification.
    pub availability: Availability,
}

/// A stored member of the candidate, read as `T`.
fn stored<T: DeserializeOwned>(value: &Value, what: &str) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|e| format!("{what}: {e}"))
}

/// The deterministic UUID of a GTS id (package §3.3.1), for the reads' indexed columns.
fn uuid_of(id: &str) -> Result<Uuid, String> {
    GtsId::try_new(id)
        .map(|g| g.to_uuid())
        .map_err(|e| format!("{id} is not a GTS id: {e}"))
}

/// A pinned entity as the definition carries it: its reference, digest and content.
fn pinned_ref(pinned: &Pinned, id: &str) -> Result<Value, String> {
    let entity = pinned
        .get(id)
        .ok_or_else(|| format!("the proposal pinned no {id}"))?;
    Ok(json!({"ref": id, "digest": entity.evidence.digest, "content": entity.content}))
}

/// The entity `id` the proposal pinned, read as `T`.
fn pinned_content<T: DeserializeOwned>(pinned: &Pinned, id: &str) -> Result<T, String> {
    pinned
        .get(id)
        .ok_or_else(|| format!("the proposal pinned no {id}"))
        .and_then(|entity| stored(&entity.content, id))
}

/// The lineage's mapping version for this candidate: the latest reused when its digests are the
/// same, else the next version (the first mints the mapping and profile ids).
fn mapping_of(
    candidate: &QuotaCandidate,
    target: &QuotaTarget,
    record: &QuotaCapabilityV1,
    profile_json: Value,
    lineage: Option<&MappingVersion>,
    approver: Uuid,
    at: OffsetDateTime,
) -> Result<(MappingVersion, bool), String> {
    let mapping_json = json!({
        "target_id": target.id.to_string(),
        "plugin_instance_id": target.plugin_instance_id,
        "target_key": target.target_key,
        "capability_key": record.capability_key,
        "mapping_key": record.mapping_key,
        "metric_ref": record.metric_ref.as_ref(),
        "unit_ref": record.unit_ref.as_ref(),
        "limits": record.limits,
        "operations": record.operations,
        "record_digest": candidate.record_digest,
        "evidence": candidate.evidence_json,
    });
    let digest = mapping_digest(&mapping_json);
    let profile = profile_digest(&profile_json);
    if let Some(latest) = lineage
        && latest.digest == digest
        && latest.profile_digest == profile
    {
        return Ok((latest.clone(), true));
    }
    let next = |version: QuotaVersion| {
        version
            .get()
            .checked_add(1)
            .and_then(|v| QuotaVersion::new(v).ok())
            .ok_or_else(|| "the lineage's next version does not fit".to_owned())
    };
    let first = QuotaVersion::new(1).map_err(|e| e.to_string())?;
    let (mapping_id, version, profile_id, profile_version) = match lineage {
        Some(latest) => (
            latest.mapping_id,
            next(latest.version)?,
            latest.profile_id,
            next(latest.profile_version)?,
        ),
        None => (Uuid::new_v4(), first, Uuid::new_v4(), first),
    };
    Ok((
        MappingVersion {
            mapping_id,
            version,
            target_id: target.id,
            capability_key: record.capability_key.clone(),
            plugin_instance_id: target.plugin_instance_id.clone(),
            target_key: target.target_key.clone(),
            mapping_key: record.mapping_key.clone(),
            mapping_json,
            digest,
            profile_id,
            profile_version,
            profile_json,
            profile_digest: profile,
            created_by: approver,
            created_at: at,
        },
        false,
    ))
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-availability
/// Availability revision 1 of `definition`: disabled, pending qualification, with no
/// qualification reference (P-D-271).
fn pending_availability(
    definition: &DefinitionVersion,
    approver: Uuid,
    at: OffsetDateTime,
) -> Result<Availability, String> {
    Ok(Availability {
        definition_id: definition.definition_id,
        version: definition.version,
        revision: QuotaVersion::new(1).map_err(|e| e.to_string())?,
        state: QuotaAvailabilityState::Disabled,
        reason_code: Some(QUALIFICATION_PENDING.to_owned()),
        qualification: None,
        observed_at: at,
        decided_by: approver,
    })
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-availability

/// Builds the definition version a submitted candidate's approval publishes, from the stored
/// candidate and proposal only: no registry call (P-D-270). `lineage` is the latest mapping of
/// `(target, capability_key)`, `latest_version` the definition's latest version when the proposal
/// names a definition id.
///
/// # Errors
/// A message naming the stored member that does not decode (a corrupt row).
// @cpt-algo:cpt-cf-bss-products-algo-quota-catalog-definition:p1
pub fn build(
    candidate: &QuotaCandidate,
    target: &QuotaTarget,
    lineage: Option<&MappingVersion>,
    latest_version: Option<QuotaVersion>,
    approver: Uuid,
    at: OffsetDateTime,
) -> Result<DefinitionDraft, String> {
    // @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-build
    let record: QuotaCapabilityV1 = stored(&candidate.record_json, "record_json")?;
    let proposal = proposal_from_json(
        candidate
            .proposal_json
            .as_ref()
            .ok_or_else(|| "proposal_json: the candidate holds no proposal".to_owned())?,
    )?;
    let pinned = Pinned::from_json(
        candidate
            .proposal_evidence_json
            .as_ref()
            .ok_or_else(|| "proposal_evidence_json: none".to_owned())?,
    )?;
    let (metric_ref, accounting_ref, scope_ref) = (
        record.metric_ref.as_ref(),
        record.accounting_policy_ref.as_ref(),
        record.scope_policy_ref.as_ref(),
    );
    let metric: QuotaMetricV1 = pinned_content(&pinned, metric_ref)?;
    let accounting: QuotaAccountingPolicyV1 = pinned_content(&pinned, accounting_ref)?;
    let scope: QuotaScopePolicyV1 = pinned_content(&pinned, scope_ref)?;
    let profile = check_profile(&record, &scope, &accounting)
        .map_err(|r| format!("the stored record's profile: {}", r.detail))?;
    let (mapping, reuse_mapping) = mapping_of(
        candidate,
        target,
        &record,
        profile.to_json(),
        lineage,
        approver,
        at,
    )?;
    let definition_id = proposal.definition_id.unwrap_or_else(Uuid::new_v4);
    let version = match (proposal.definition_id, latest_version) {
        (None, _) => QuotaVersion::new(1).map_err(|e| e.to_string())?,
        (Some(_), Some(latest)) => latest
            .get()
            .checked_add(1)
            .and_then(|v| QuotaVersion::new(v).ok())
            .ok_or_else(|| "the definition's next version does not fit".to_owned())?,
        (Some(id), None) => return Err(format!("the named definition {id} has no version")),
    };
    let mut evidence = EvidenceSet::from_json(&candidate.evidence_json)
        .map_err(|e| format!("evidence_json: {e}"))?;
    let mut metric_json = pinned_ref(&pinned, metric_ref)?;
    metric_json["uuid"] = json!(uuid_of(metric_ref)?.to_string());
    let allowed_units: Vec<Value> = proposal
        .allowed_units
        .iter()
        .map(|unit| {
            evidence.insert(unit.evidence.clone());
            json!({
                "unit_ref": unit.unit.id.as_ref(),
                "digest": unit.evidence.digest,
                "symbol": unit.unit.symbol,
                "numerator": unit.unit.numerator,
                "denominator": unit.unit.denominator,
                "min": unit.min,
                "step": unit.step,
                "max": unit.max,
            })
        })
        .collect();
    let resource_type_refs: Vec<&str> = record
        .resource_type_refs
        .iter()
        .map(AsRef::as_ref)
        .collect();
    let content_json = json!({
        "sku_id": proposal.sku_id.to_string(),
        "sku_version": proposal.sku_version.to_string(),
        "metric": metric_json,
        "canonical_unit": pinned_ref(&pinned, &metric.canonical_unit_ref)?,
        "allowed_units": allowed_units,
        "subject_type_ref": record.subject_type_ref.as_ref(),
        "resource_type_refs": resource_type_refs,
        "accounting_policy": pinned_ref(&pinned, accounting_ref)?,
        "scope_policy": pinned_ref(&pinned, scope_ref)?,
        "change_support": record.change_support,
        "supported_bases": proposal
            .supported_bases
            .iter()
            .map(|b| basis_token(*b))
            .collect::<Vec<_>>(),
        "profile_conditions": proposal.profile_conditions,
        "mapping_ref": {
            "id": mapping.mapping_id.to_string(),
            "version": mapping.version.to_string(),
            "digest": mapping.digest,
        },
        "profile_ref": {
            "id": mapping.profile_id.to_string(),
            "version": mapping.profile_version.to_string(),
            "digest": mapping.profile_digest,
        },
        "evidence": evidence.to_json(),
    });
    let resource_type_uuids = resource_type_refs
        .iter()
        .map(|id| uuid_of(id))
        .collect::<Result<Vec<_>, _>>()?;
    let digest = definition_digest(&content_json);
    let definition = DefinitionVersion {
        definition_id,
        version,
        candidate_id: candidate.id,
        sku_id: proposal.sku_id,
        sku_version: proposal.sku_version,
        metric_uuid: uuid_of(metric_ref)?,
        subject_type_uuid: uuid_of(record.subject_type_ref.as_ref())?,
        profile_id: mapping.profile_id,
        mapping_id: mapping.mapping_id,
        mapping_version: mapping.version,
        content_json,
        digest,
        created_by: approver,
        created_at: at,
    };
    // @cpt-end:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-build
    let availability = pending_availability(&definition, approver, at)?;
    Ok(DefinitionDraft {
        mapping,
        reuse_mapping,
        definition,
        resource_type_uuids,
        availability,
    })
}

#[cfg(test)]
#[path = "definition_tests.rs"]
mod definition_tests;

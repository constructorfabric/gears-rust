//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-review:p1
//! A submitter's proposal for a staged candidate (P-D-269, P-D-270): one SKU version, the plan
//! units with the record's bounds converted exactly into each, the bases, the reviewed conditions,
//! and the qualified profile the record's capability supports. Pure: the endpoint resolves what a
//! proposal names before it calls in, and stores what it answers.
//!
//! A proposal also pins the registry entities the definition will carry (the metric, the record's
//! unit and its canonical unit, both policies and each plan unit): their evidence and their
//! content as resolved at submit, so an approval builds the definition from stored rows alone.
use std::collections::{BTreeMap, BTreeSet};

use bss_products_sdk::quota::catalog::{QuotaBasis, QuotaVersion};
use bss_products_sdk::quota::gts::{
    QuantityBasis, QuotaAccountingPolicyV1, QuotaCapabilityV1, QuotaChangeSupport, QuotaLimits,
    QuotaOperations, QuotaScopePolicyV1, QuotaUnitV1, ScopeAggregation,
};
use bss_products_sdk::quota::units::{Amount, Factor, UnitError, convert};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::evidence::{Evidence, EvidenceSet};
use super::locale::{MAX_TEXT_CHARS, check_localized};
use super::record::RecordRefusal;

/// The capability lacks an operation or initial installation the qualified profile requires.
pub const QUOTA_PROFILE_UNSUPPORTED: &str = "QUOTA_PROFILE_UNSUPPORTED";
/// The record's bounds do not convert exactly into a plan unit.
pub const QUOTA_UNIT_INEXACT: &str = "QUOTA_UNIT_INEXACT";
/// The most plan units one proposal allows.
pub const MAX_ALLOWED_UNITS: usize = 8;
/// The qualified profile of version 1 (P-D-270).
pub const BOUNDED_ALLOCATION: &str = "bounded_allocation";

/// One plan unit a proposal allows: the unit as resolved at submit, its evidence, and the record's
/// bounds converted exactly into it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedUnit {
    pub unit: QuotaUnitV1,
    pub evidence: Evidence,
    pub min: String,
    pub step: String,
    pub max: Option<String>,
}

/// What a submitter proposes for a staged candidate (P-D-269).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub sku_id: Uuid,
    /// The SKU version it is proposed against; it applies to later ones too (P-D-270).
    pub sku_version: QuotaVersion,
    /// The definition it versions; `None` for a new definition.
    pub definition_id: Option<Uuid>,
    pub allowed_units: Vec<AllowedUnit>,
    pub supported_bases: Vec<QuotaBasis>,
    pub profile_conditions: BTreeMap<String, String>,
}

/// The qualified profile of version 1 (P-D-270): bounded allocation in the scope policy's
/// aggregation, on the accounting basis, every operation required, and the record's change
/// support.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileV1 {
    pub aggregation: ScopeAggregation,
    pub accounting: QuantityBasis,
    pub change_support: QuotaChangeSupport,
}

impl ProfileV1 {
    /// `{"profile":"bounded_allocation","aggregation":…,"accounting":…,"required_operations":{six
    /// true},"change_support":{…}}` (P-D-270).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let required = QuotaOperations {
            apply: true,
            read_application: true,
            recover_operation: true,
            restrict: true,
            revoke: true,
            resume: true,
        };
        json!({
            "profile": BOUNDED_ALLOCATION,
            "aggregation": self.aggregation,
            "accounting": self.accounting,
            "required_operations": required,
            "change_support": self.change_support,
        })
    }
}

/// One registry entity a proposal pins: its evidence and its content as resolved at submit.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedEntity {
    pub evidence: Evidence,
    pub content: Value,
}

/// Every registry entity a proposal pins, by GTS id; stored as the candidate's proposal evidence:
/// `{gts_id: {uuid, resource_version, digest, content}}`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pinned(pub BTreeMap<String, PinnedEntity>);

impl Pinned {
    /// Keeps `entity` under its id.
    pub fn insert(&mut self, entity: PinnedEntity) {
        self.0.insert(entity.evidence.gts_id.clone(), entity);
    }

    /// The pinned entity `id`.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&PinnedEntity> {
        self.0.get(id)
    }

    /// The evidence alone, as an intake keeps it.
    #[must_use]
    pub fn evidence(&self) -> EvidenceSet {
        let mut set = EvidenceSet::default();
        for entity in self.0.values() {
            set.insert(entity.evidence.clone());
        }
        set
    }

    /// `{gts_id: {uuid, resource_version, digest, content}}`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let evidence = self.evidence().to_json();
        Value::Object(
            self.0
                .iter()
                .map(|(id, entity)| {
                    let mut entry = evidence[id].clone();
                    entry["content"] = entity.content.clone();
                    (id.clone(), entry)
                })
                .collect::<Map<String, Value>>(),
        )
    }

    /// Reads [`Self::to_json`]'s shape back.
    ///
    /// # Errors
    /// A message naming the member that does not decode.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let entries = value
            .as_object()
            .ok_or_else(|| "the pinned entities are not a JSON object".to_owned())?;
        let mut pinned = Self::default();
        for (id, entry) in entries {
            let mut fields = entry
                .as_object()
                .cloned()
                .ok_or_else(|| format!("pinned {id} is not a JSON object"))?;
            let content = fields
                .remove("content")
                .ok_or_else(|| format!("pinned {id} has no content"))?;
            let evidence = EvidenceSet::from_json(&json!({ id: fields }))?;
            let evidence = evidence
                .0
                .into_values()
                .next()
                .ok_or_else(|| format!("pinned {id} has no evidence"))?;
            pinned.insert(PinnedEntity { evidence, content });
        }
        Ok(pinned)
    }
}

/// The token a basis is stored and answered as.
#[must_use]
pub const fn basis_token(basis: QuotaBasis) -> &'static str {
    match basis {
        QuotaBasis::Fixed => "fixed",
        QuotaBasis::PerPlanUnit => "per_plan_unit",
    }
}

/// The basis a token names; `None` for any other text.
#[must_use]
pub fn parse_basis(token: &str) -> Option<QuotaBasis> {
    match token {
        "fixed" => Some(QuotaBasis::Fixed),
        "per_plan_unit" => Some(QuotaBasis::PerPlanUnit),
        _ => None,
    }
}

/// The record's bounds in `to`, exactly (P-D-270): `min`, `step` and `max` stated in `from`, each
/// converted on `u128` with checked multiplication.
///
/// # Errors
/// `UnitError::Inexact` / `Overflow` from `bss_products_sdk::quota::units::convert`; a bound or a
/// factor that does not read.
pub fn convert_bounds(
    limits: &QuotaLimits,
    from: &QuotaUnitV1,
    to: &QuotaUnitV1,
) -> Result<(String, String, Option<String>), UnitError> {
    let from_factor = Factor::parse(&from.numerator, &from.denominator)?;
    let to_factor = Factor::parse(&to.numerator, &to.denominator)?;
    let one = |text: &str| {
        Amount::parse(text)
            .and_then(|amount| convert(amount, from_factor, to_factor))
            .map(|amount| amount.to_string())
    };
    let max = limits.max.as_deref().map(one).transpose()?;
    Ok((one(&limits.min)?, one(&limits.step)?, max))
}

/// The qualified profile the record's capability supports (P-D-270): every operation and initial
/// installation are required; increase support is recorded, not required.
///
/// # Errors
/// `QUOTA_PROFILE_UNSUPPORTED` on `operations.<flag>` or `change_support.initial_set`.
pub fn check_profile(
    record: &QuotaCapabilityV1,
    scope: &QuotaScopePolicyV1,
    accounting: &QuotaAccountingPolicyV1,
) -> Result<ProfileV1, RecordRefusal> {
    let ops = record.operations;
    let flags = [
        ("apply", ops.apply),
        ("read_application", ops.read_application),
        ("recover_operation", ops.recover_operation),
        ("restrict", ops.restrict),
        ("revoke", ops.revoke),
        ("resume", ops.resume),
    ];
    if let Some((flag, _)) = flags.iter().find(|(_, supported)| !supported) {
        return Err(RecordRefusal {
            code: QUOTA_PROFILE_UNSUPPORTED,
            field: format!("operations.{flag}"),
            detail: format!("the {BOUNDED_ALLOCATION} profile requires the {flag} operation"),
        });
    }
    if !record.change_support.initial_set {
        return Err(RecordRefusal {
            code: QUOTA_PROFILE_UNSUPPORTED,
            field: "change_support.initial_set".to_owned(),
            detail: format!("the {BOUNDED_ALLOCATION} profile requires initial installation"),
        });
    }
    Ok(ProfileV1 {
        aggregation: scope.aggregation,
        accounting: accounting.quantity_basis,
        change_support: record.change_support,
    })
}

/// A proposal's shape (P-D-269): one or both bases, each once; one to eight plan units, each
/// once; conditions with English text within the locale caps; a note of at most 2000 characters.
///
/// # Errors
/// `VALIDATION` on `supported_bases` (empty or repeated), on `allowed_unit_refs` (0 or more than
/// 8, repeated), `FIELD_TOO_LONG` on `note`; the locale refusals of `check_localized` on
/// `profile_conditions`.
pub fn check_proposal_shape(
    bases: &[QuotaBasis],
    unit_refs: &[String],
    conditions: &BTreeMap<String, String>,
    note: Option<&str>,
) -> Result<(), RecordRefusal> {
    let refused = |code: &'static str, field: &str, detail: String| RecordRefusal {
        code,
        field: field.to_owned(),
        detail,
    };
    let unique_bases: BTreeSet<&str> = bases.iter().map(|b| basis_token(*b)).collect();
    if bases.is_empty() || unique_bases.len() != bases.len() {
        return Err(refused(
            "VALIDATION",
            "supported_bases",
            "one or both of fixed and per_plan_unit, each once".to_owned(),
        ));
    }
    let unique_units: BTreeSet<&str> = unit_refs.iter().map(String::as_str).collect();
    if !(1..=MAX_ALLOWED_UNITS).contains(&unit_refs.len()) || unique_units.len() != unit_refs.len()
    {
        return Err(refused(
            "VALIDATION",
            "allowed_unit_refs",
            format!("1 to {MAX_ALLOWED_UNITS} unique plan units"),
        ));
    }
    check_localized("profile_conditions", conditions)?;
    if note.is_some_and(|n| n.chars().count() > MAX_TEXT_CHARS) {
        return Err(refused(
            "FIELD_TOO_LONG",
            "note",
            format!("at most {MAX_TEXT_CHARS} characters"),
        ));
    }
    Ok(())
}

/// One allowed unit as a proposal stores it: the unit, its evidence and its bounds.
fn allowed_unit_json(allowed: &AllowedUnit) -> Value {
    let unit = &allowed.unit;
    json!({
        "unit_ref": unit.id.as_ref(),
        "symbol": unit.symbol,
        "canonical_unit_ref": unit.canonical_unit_ref,
        "numerator": unit.numerator,
        "denominator": unit.denominator,
        "uuid": allowed.evidence.uuid.to_string(),
        "resource_version": allowed.evidence.resource_version,
        "digest": allowed.evidence.digest,
        "min": allowed.min,
        "step": allowed.step,
        "max": allowed.max,
    })
}

/// The proposal as the candidate stores and answers it: `sku_version` a decimal string, the bases
/// as their tokens, and each allowed unit with its evidence and bounds.
#[must_use]
pub fn proposal_json(p: &Proposal) -> Value {
    json!({
        "sku_id": p.sku_id.to_string(),
        "sku_version": p.sku_version.to_string(),
        "definition_id": p.definition_id.map(|id| id.to_string()),
        "allowed_units": p.allowed_units.iter().map(allowed_unit_json).collect::<Vec<_>>(),
        "supported_bases": p.supported_bases.iter().map(|b| basis_token(*b)).collect::<Vec<_>>(),
        "profile_conditions": p.profile_conditions,
    })
}

/// Member `name` of `object`, read as `T`.
fn member<T: DeserializeOwned>(object: &Value, name: &str) -> Result<T, String> {
    object
        .get(name)
        .cloned()
        .ok_or_else(|| format!("the proposal has no {name}"))
        .and_then(|v| serde_json::from_value(v).map_err(|e| format!("{name}: {e}")))
}

fn allowed_unit_from_json(value: &Value) -> Result<AllowedUnit, String> {
    let unit_ref: String = member(value, "unit_ref")?;
    let unit: QuotaUnitV1 = serde_json::from_value(json!({
        "id": unit_ref,
        "symbol": member::<String>(value, "symbol")?,
        "canonical_unit_ref": member::<String>(value, "canonical_unit_ref")?,
        "numerator": member::<String>(value, "numerator")?,
        "denominator": member::<String>(value, "denominator")?,
    }))
    .map_err(|e| format!("allowed unit {unit_ref}: {e}"))?;
    let evidence = EvidenceSet::from_json(&json!({ unit_ref.clone(): {
        "uuid": member::<String>(value, "uuid")?,
        "resource_version": member::<u64>(value, "resource_version")?,
        "digest": member::<String>(value, "digest")?,
    }}))?
    .0
    .into_values()
    .next()
    .ok_or_else(|| format!("allowed unit {unit_ref} has no evidence"))?;
    Ok(AllowedUnit {
        unit,
        evidence,
        min: member(value, "min")?,
        step: member(value, "step")?,
        max: member(value, "max")?,
    })
}

/// Reads [`proposal_json`]'s shape back.
///
/// # Errors
/// A message naming the member that does not decode.
pub fn proposal_from_json(v: &Value) -> Result<Proposal, String> {
    let sku_version: String = member(v, "sku_version")?;
    let bases: Vec<String> = member(v, "supported_bases")?;
    let units: Vec<Value> = member(v, "allowed_units")?;
    Ok(Proposal {
        sku_id: member(v, "sku_id")?,
        sku_version: sku_version
            .parse()
            .map_err(|_| format!("sku_version {sku_version:?} is not a version"))?,
        definition_id: member(v, "definition_id")?,
        allowed_units: units
            .iter()
            .map(allowed_unit_from_json)
            .collect::<Result<_, _>>()?,
        supported_bases: bases
            .iter()
            .map(|token| parse_basis(token).ok_or_else(|| format!("supported_bases: {token:?}")))
            .collect::<Result<_, _>>()?,
        profile_conditions: member(v, "profile_conditions")?,
    })
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod review_tests;

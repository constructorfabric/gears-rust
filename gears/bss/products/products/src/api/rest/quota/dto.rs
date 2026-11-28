//! The quota catalog's wire types (P-D-268 … P-D-272): `snake_case`, each name led by
//! `ProductsQuota`. A closed set on a response is its `enum`, mirroring its SDK or domain enum
//! variant by variant, both ways (P-D-217); a request keeps `string`, so its endpoint refuses.
use std::collections::BTreeMap;

use bss_products_sdk::quota::catalog::{
    QuotaAllowedUnit, QuotaAvailability, QuotaAvailabilityState, QuotaBasis, QuotaDefinition,
    QuotaOwnerRef,
};
use bss_products_sdk::quota::gts::{QuotaChangeSupport, QuotaKind};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::quota::intake::{DriftNote, RejectedRecord, RetiredNote};
use crate::domain::quota::{CandidateOrigin, CandidateState, QuotaCandidate, QuotaTarget};

/// What a target create binds: a discovery plugin instance and the adapter's opaque key, and a
/// name for people.
#[toolkit_macros::api_dto(request)]
pub struct ProductsQuotaAdapterTargetRequest {
    /// An instance types-registry holds under the quota discovery plugin specification.
    pub plugin_instance_id: String,
    /// The adapter's opaque key: 1 to 255 printable ASCII bytes.
    pub target_key: String,
    /// 1 to 200 characters.
    pub name: String,
}

/// A tenant's binding of one discovery plugin instance and one target key (P-D-268).
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaAdapterTarget {
    pub id: Uuid,
    pub plugin_instance_id: String,
    pub target_key: String,
    pub name: String,
    pub created_by: Uuid,
    /// The current name of `created_by` (P-D-262); null when no name is available now, and on a
    /// write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<QuotaTarget> for ProductsQuotaAdapterTarget {
    fn from(target: QuotaTarget) -> Self {
        Self {
            id: target.id,
            plugin_instance_id: target.plugin_instance_id,
            target_key: target.target_key,
            name: target.name,
            created_by: target.created_by,
            created_by_name: None,
            created_at: target.created_at,
        }
    }
}

/// What a manual record create sends: the target and one capability record (P-D-268).
#[toolkit_macros::api_dto(request)]
pub struct ProductsQuotaCandidateRequest {
    pub target_id: Uuid,
    /// One capability record of the quota capability schema, as an adapter would answer it.
    pub record: Value,
}

/// What a submit proposes for a staged candidate (P-D-269).
#[toolkit_macros::api_dto(request)]
pub struct ProductsQuotaSubmitRequest {
    /// A SKU of the caller's tenant.
    pub sku_id: Uuid,
    /// A canonical decimal from 1: a version the SKU holds. The definition applies to it and to
    /// every later version of the SKU.
    pub sku_version: String,
    /// The definition this versions, of the candidate's metric; absent for a new definition.
    pub definition_id: Option<Uuid>,
    /// 1 to 8 unit instances of the metric's canonical unit, each once, into which the record's
    /// bounds convert exactly.
    pub allowed_unit_refs: Vec<String>,
    /// `fixed` and/or `per_plan_unit`, each once.
    pub supported_bases: Vec<String>,
    /// The reviewed conditions a plan author shows the buyer, by locale tag; English is required.
    pub profile_conditions: BTreeMap<String, String>,
    /// At most 2000 characters.
    pub note: Option<String>,
}

/// An approval's or a rejection's note (P-D-269): optional on an approval, required on a
/// rejection.
#[toolkit_macros::api_dto(request)]
pub struct ProductsQuotaDecisionRequest {
    /// At most 2000 characters.
    pub note: Option<String>,
}

/// A stored version of an owner's entity, with its digest.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaOwnerRef {
    pub id: Uuid,
    /// A canonical decimal from 1.
    pub version: String,
    /// 64 lowercase hex digits.
    pub digest: String,
}

impl From<QuotaOwnerRef> for ProductsQuotaOwnerRef {
    fn from(owner: QuotaOwnerRef) -> Self {
        Self {
            id: owner.id,
            version: owner.version.to_string(),
            digest: owner.digest,
        }
    }
}

/// One candidate: its record and evidence, never changed, and its review (P-D-268, P-D-269).
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaCandidate {
    pub id: Uuid,
    pub target_id: Uuid,
    pub origin: ProductsQuotaCandidateOrigin,
    /// The stored snapshot row a discovered candidate came from (the discovery answer's
    /// `snapshot.id`, not the adapter's `snapshot.snapshot_id`); null for a manual one.
    pub snapshot_row_id: Option<Uuid>,
    /// The record as received.
    pub record: Value,
    /// The record's canonical digest, 64 lowercase hex digits.
    pub record_digest: String,
    /// Every registry entity the record resolved to: `{gts_id: {uuid, resource_version, digest}}`.
    pub evidence: Value,
    pub state: ProductsQuotaCandidateState,
    /// The candidate's version, also its `ETag`; every review write moves it by one.
    pub version: i64,
    /// The submitted proposal; null until submitted.
    pub proposal: Option<Value>,
    pub created_by: Uuid,
    /// The current name of `created_by` (P-D-262); null when no name is available now, and on a
    /// write answer.
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub submitted_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub submitted_at: Option<OffsetDateTime>,
    pub submit_note: Option<String>,
    pub decided_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    pub decided_note: Option<String>,
    /// The definition version an approval published, with the definition's digest; null until
    /// approved.
    pub definition_ref: Option<ProductsQuotaOwnerRef>,
}

impl ProductsQuotaCandidate {
    /// A stored candidate as the endpoints answer it; `definition_ref` is the approved definition
    /// version's, which the candidate row does not hold.
    #[must_use]
    pub fn of(candidate: QuotaCandidate, definition_ref: Option<ProductsQuotaOwnerRef>) -> Self {
        Self {
            id: candidate.id,
            target_id: candidate.target_id,
            origin: candidate.origin.into(),
            snapshot_row_id: candidate.snapshot_row_id,
            record: candidate.record_json,
            record_digest: candidate.record_digest,
            evidence: candidate.evidence_json,
            state: candidate.state.into(),
            version: candidate.version,
            proposal: candidate.proposal_json,
            created_by: candidate.created_by,
            created_by_name: None,
            created_at: candidate.created_at,
            submitted_by: candidate.submitted_by,
            submitted_at: candidate.submitted_at,
            submit_note: candidate.submit_note,
            decided_by: candidate.decided_by,
            decided_at: candidate.decided_at,
            decided_note: candidate.decided_note,
            definition_ref,
        }
    }
}

/// A record of a snapshot that became no candidate, with its refusal.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaRefusal {
    /// The record's own `id`, or `#<index>` in the snapshot when it has none.
    pub record: String,
    pub code: String,
    pub field: String,
    pub detail: String,
}

impl From<&RejectedRecord> for ProductsQuotaRefusal {
    fn from(rejected: &RejectedRecord) -> Self {
        Self {
            record: rejected.record.clone(),
            code: rejected.refusal.code.to_owned(),
            field: rejected.refusal.field.clone(),
            detail: rejected.refusal.detail.clone(),
        }
    }
}

/// A capability whose approved mapping the snapshot drifted from or no longer holds.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaNote {
    pub capability_key: String,
    pub mapping_id: Uuid,
    /// A canonical decimal from 1.
    pub mapping_version: String,
}

impl From<&DriftNote> for ProductsQuotaNote {
    fn from(note: &DriftNote) -> Self {
        Self {
            capability_key: note.capability_key.clone(),
            mapping_id: note.mapping_id,
            mapping_version: note.mapping_version.to_string(),
        }
    }
}

impl From<&RetiredNote> for ProductsQuotaNote {
    fn from(note: &RetiredNote) -> Self {
        Self {
            capability_key: note.capability_key.clone(),
            mapping_id: note.mapping_id,
            mapping_version: note.mapping_version.to_string(),
        }
    }
}

/// One stored, verified capability snapshot.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaSnapshot {
    /// The stored snapshot's own id.
    pub id: Uuid,
    /// The adapter's snapshot id.
    pub snapshot_id: Uuid,
    /// A canonical decimal of at most 39 digits.
    pub source_revision: String,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub item_count: u64,
    /// The snapshot's canonical digest, 64 lowercase hex digits.
    pub content_digest: String,
}

/// What a discovery staged, or the stored revision it recovered (P-D-268).
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaDiscoveryResult {
    pub snapshot: ProductsQuotaSnapshot,
    /// One staged candidate per valid record, by capability key.
    pub candidates: Vec<ProductsQuotaCandidate>,
    /// The records that became no candidate.
    pub rejected: Vec<ProductsQuotaRefusal>,
    /// Approved capabilities whose record or evidence changed.
    pub drift: Vec<ProductsQuotaNote>,
    /// Approved capabilities the snapshot no longer holds.
    pub retired: Vec<ProductsQuotaNote>,
}

/// A candidate's review state (P-D-269).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ProductsQuotaCandidateState {
    Staged,
    Submitted,
    Approved,
    Rejected,
}

impl From<CandidateState> for ProductsQuotaCandidateState {
    fn from(state: CandidateState) -> Self {
        match state {
            CandidateState::Staged => Self::Staged,
            CandidateState::Submitted => Self::Submitted,
            CandidateState::Approved => Self::Approved,
            CandidateState::Rejected => Self::Rejected,
        }
    }
}

impl From<ProductsQuotaCandidateState> for CandidateState {
    fn from(state: ProductsQuotaCandidateState) -> Self {
        match state {
            ProductsQuotaCandidateState::Staged => Self::Staged,
            ProductsQuotaCandidateState::Submitted => Self::Submitted,
            ProductsQuotaCandidateState::Approved => Self::Approved,
            ProductsQuotaCandidateState::Rejected => Self::Rejected,
        }
    }
}

/// Where a candidate came from (P-D-268).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ProductsQuotaCandidateOrigin {
    Discovery,
    Manual,
}

impl From<CandidateOrigin> for ProductsQuotaCandidateOrigin {
    fn from(origin: CandidateOrigin) -> Self {
        match origin {
            CandidateOrigin::Discovery => Self::Discovery,
            CandidateOrigin::Manual => Self::Manual,
        }
    }
}

impl From<ProductsQuotaCandidateOrigin> for CandidateOrigin {
    fn from(origin: ProductsQuotaCandidateOrigin) -> Self {
        match origin {
            ProductsQuotaCandidateOrigin::Discovery => Self::Discovery,
            ProductsQuotaCandidateOrigin::Manual => Self::Manual,
        }
    }
}

/// Whether a definition version may be sold (P-D-271).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ProductsQuotaAvailabilityState {
    Enabled,
    Disabled,
    Withdrawn,
}

impl From<QuotaAvailabilityState> for ProductsQuotaAvailabilityState {
    fn from(state: QuotaAvailabilityState) -> Self {
        match state {
            QuotaAvailabilityState::Enabled => Self::Enabled,
            QuotaAvailabilityState::Disabled => Self::Disabled,
            QuotaAvailabilityState::Withdrawn => Self::Withdrawn,
        }
    }
}

impl From<ProductsQuotaAvailabilityState> for QuotaAvailabilityState {
    fn from(state: ProductsQuotaAvailabilityState) -> Self {
        match state {
            ProductsQuotaAvailabilityState::Enabled => Self::Enabled,
            ProductsQuotaAvailabilityState::Disabled => Self::Disabled,
            ProductsQuotaAvailabilityState::Withdrawn => Self::Withdrawn,
        }
    }
}

/// How a plan states a quota amount (P-D-270).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ProductsQuotaBasis {
    /// One amount for the whole plan item.
    Fixed,
    /// An amount per purchased plan unit.
    PerPlanUnit,
}

impl From<QuotaBasis> for ProductsQuotaBasis {
    fn from(basis: QuotaBasis) -> Self {
        match basis {
            QuotaBasis::Fixed => Self::Fixed,
            QuotaBasis::PerPlanUnit => Self::PerPlanUnit,
        }
    }
}

impl From<ProductsQuotaBasis> for QuotaBasis {
    fn from(basis: ProductsQuotaBasis) -> Self {
        match basis {
            ProductsQuotaBasis::Fixed => Self::Fixed,
            ProductsQuotaBasis::PerPlanUnit => Self::PerPlanUnit,
        }
    }
}

/// One plan unit a definition allows, with its exact factor and the bounds converted into it.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaAllowedUnit {
    pub unit_ref: String,
    /// The unit's registry content digest, kept at approval.
    pub digest: String,
    /// Display symbol; never an identity.
    pub symbol: String,
    /// The unit's factor into the canonical unit: numerator, a decimal.
    pub numerator: String,
    /// The unit's factor into the canonical unit: denominator, a decimal.
    pub denominator: String,
    /// The smallest amount in this unit.
    pub min: String,
    /// The granularity of an amount in this unit.
    pub step: String,
    /// The largest amount in this unit; null when unbounded.
    pub max: Option<String>,
}

impl From<QuotaAllowedUnit> for ProductsQuotaAllowedUnit {
    fn from(unit: QuotaAllowedUnit) -> Self {
        Self {
            unit_ref: unit.unit_ref,
            digest: unit.digest,
            symbol: unit.symbol,
            numerator: unit.numerator,
            denominator: unit.denominator,
            min: unit.min,
            step: unit.step,
            max: unit.max,
        }
    }
}

/// A definition version's current availability: its highest revision (P-D-271).
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaAvailability {
    pub state: ProductsQuotaAvailabilityState,
    /// A canonical decimal from 1.
    pub revision: String,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// Why the version is not enabled (`qualification_pending` until qualification); null exactly
    /// when it is enabled.
    pub reason_code: Option<String>,
}

impl From<QuotaAvailability> for ProductsQuotaAvailability {
    fn from(availability: QuotaAvailability) -> Self {
        Self {
            state: availability.state.into(),
            revision: availability.revision.to_string(),
            observed_at: availability.observed_at,
            reason_code: availability.reason_code,
        }
    }
}

/// Which changes of an installed quota the adapter supports.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaChangeSupport {
    /// The first installation of a quota.
    pub initial_set: bool,
    /// Raising an installed quota.
    pub increase: bool,
}

impl From<QuotaChangeSupport> for ProductsQuotaChangeSupport {
    fn from(support: QuotaChangeSupport) -> Self {
        Self {
            initial_set: support.initial_set,
            increase: support.increase,
        }
    }
}

/// The kind of quantity a metric counts (P-D-267).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ProductsQuotaKind {
    /// Capacity the service allocates to a subject and enforces.
    Allocation,
}

impl From<QuotaKind> for ProductsQuotaKind {
    fn from(kind: QuotaKind) -> Self {
        match kind {
            QuotaKind::Allocation => Self::Allocation,
        }
    }
}

impl From<ProductsQuotaKind> for QuotaKind {
    fn from(kind: ProductsQuotaKind) -> Self {
        match kind {
            ProductsQuotaKind::Allocation => Self::Allocation,
        }
    }
}

/// One immutable definition version with its current availability (P-D-270, P-D-272): the
/// fields of the quotas package's catalog entry, as the SDK's `QuotaDefinition` answers them.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaDefinition {
    pub definition_id: Uuid,
    /// A canonical decimal from 1.
    pub version: String,
    /// The canonical digest of the version's content, 64 lowercase hex digits.
    pub definition_digest: String,
    pub sku_id: Uuid,
    /// The SKU version it was approved against; it applies to that version and every later one.
    pub sku_version: String,
    pub metric_ref: String,
    /// The metric's registry content digest, kept at approval.
    pub metric_digest: String,
    pub quota_kind: ProductsQuotaKind,
    /// The metric's display name in `locale`.
    pub display_name: String,
    /// The metric's description in `locale`.
    pub description: String,
    /// The locale tag the texts are served in: the requested one, or `en`.
    pub locale: String,
    pub canonical_unit_ref: String,
    pub allowed_units: Vec<ProductsQuotaAllowedUnit>,
    pub subject_type_ref: String,
    pub resource_type_refs: Vec<String>,
    /// The enforcement mapping version.
    pub mapping_ref: ProductsQuotaOwnerRef,
    /// The qualified profile version.
    pub profile_ref: ProductsQuotaOwnerRef,
    /// Null until qualification evidence is recorded (P-D-271).
    pub qualification_ref: Option<ProductsQuotaOwnerRef>,
    pub accounting_policy_ref: String,
    pub accounting_policy_digest: String,
    /// The accounting policy's content, as kept at approval.
    pub accounting_policy: Value,
    pub scope_policy_ref: String,
    pub scope_policy_digest: String,
    /// The scope policy's content, as kept at approval.
    pub scope_policy: Value,
    pub change_support: ProductsQuotaChangeSupport,
    /// The reviewed conditions a plan author shows the buyer, in `locale`.
    pub profile_conditions: String,
    pub supported_bases: Vec<ProductsQuotaBasis>,
    pub availability: ProductsQuotaAvailability,
}

impl From<QuotaDefinition> for ProductsQuotaDefinition {
    fn from(d: QuotaDefinition) -> Self {
        Self {
            definition_id: d.definition_id,
            version: d.version.to_string(),
            definition_digest: d.definition_digest,
            sku_id: d.sku_id,
            sku_version: d.sku_version.to_string(),
            metric_ref: d.metric_ref,
            metric_digest: d.metric_digest,
            quota_kind: d.quota_kind.into(),
            display_name: d.display_name,
            description: d.description,
            locale: d.locale,
            canonical_unit_ref: d.canonical_unit_ref,
            allowed_units: d.allowed_units.into_iter().map(Into::into).collect(),
            subject_type_ref: d.subject_type_ref,
            resource_type_refs: d.resource_type_refs,
            mapping_ref: d.mapping_ref.into(),
            profile_ref: d.profile_ref.into(),
            qualification_ref: d.qualification_ref.map(Into::into),
            accounting_policy_ref: d.accounting_policy_ref,
            accounting_policy_digest: d.accounting_policy_digest,
            // Policy contents serialize as JSON objects; nothing else can fail here.
            accounting_policy: serde_json::to_value(&d.accounting_policy).unwrap_or_default(),
            scope_policy_ref: d.scope_policy_ref,
            scope_policy_digest: d.scope_policy_digest,
            scope_policy: serde_json::to_value(&d.scope_policy).unwrap_or_default(),
            change_support: d.change_support.into(),
            profile_conditions: d.profile_conditions,
            supported_bases: d.supported_bases.into_iter().map(Into::into).collect(),
            availability: d.availability.into(),
        }
    }
}

/// What an approval answers (P-D-269, P-D-270): the approved candidate and the definition version
/// it published, as its version read answers it in English.
#[toolkit_macros::api_dto(response)]
pub struct ProductsQuotaApproval {
    pub candidate: ProductsQuotaCandidate,
    pub definition: ProductsQuotaDefinition,
}

bss_rest::actor_fields!(ProductsQuotaAdapterTarget { created_by => created_by_name } []);
bss_rest::actor_fields!(ProductsQuotaCandidate { created_by => created_by_name } []);

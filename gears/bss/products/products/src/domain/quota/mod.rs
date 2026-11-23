//! The commercial quota catalog (P-D-266): its stored shapes, as the repositories read and write
//! them (P-D-274; DESIGN §3.1, §3.7).
//!
//! Every version a definition answers with is the SDK's [`QuotaVersion`]; storage converts it to
//! and from its `bigint` columns with checked conversions only (P-D-274). A candidate's own
//! `version` is its optimistic-concurrency tag (an `InternalRevision`), not a catalog version.
pub mod definition;
pub mod evidence;
pub mod intake;
pub mod locale;
pub mod ports;
pub mod record;
pub mod review;
pub mod snapshot;

use bss_products_sdk::quota::catalog::{QuotaAvailabilityState, QuotaOwnerRef, QuotaVersion};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// A tenant's binding of one discovery plugin instance and one opaque target key (P-D-268).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaTarget {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// An instance types-registry holds under the discovery plugin specification.
    pub plugin_instance_id: String,
    /// The adapter's opaque key, 1 to 255 printable ASCII bytes.
    pub target_key: String,
    /// 1 to 200 characters.
    pub name: String,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

/// What a target create writes.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewQuotaTarget {
    pub plugin_instance_id: String,
    pub target_key: String,
    pub name: String,
}

/// Where a candidate came from (P-D-268).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateOrigin {
    /// A record of a complete discovery snapshot; the candidate names its snapshot.
    Discovery,
    /// A record an administrator authored; the candidate names no snapshot.
    Manual,
}

impl CandidateOrigin {
    /// The stored token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Manual => "manual",
        }
    }

    /// The origin a stored token names; `None` for any other text.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "discovery" => Some(Self::Discovery),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// A candidate's review state (`cpt-cf-bss-products-state-quota-candidate`): `staged`, then
/// `submitted`, then `approved` or `rejected` (P-D-269).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateState {
    Staged,
    Submitted,
    Approved,
    Rejected,
}

impl CandidateState {
    /// The stored token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Submitted => "submitted",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    /// The state a stored token names; `None` for any other text.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "staged" => Some(Self::Staged),
            "submitted" => Some(Self::Submitted),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// One stored, verified capability snapshot of a target (P-D-268). Its JSON columns are objects.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub id: Uuid,
    pub target_id: Uuid,
    /// The adapter's snapshot id.
    pub snapshot_id: Uuid,
    /// A canonical decimal of at most 39 digits, compared as a number.
    pub source_revision: String,
    /// Stored as given: a caller passes it cut to whole microseconds (`stored_instant`), so both
    /// engines keep the same digits.
    pub observed_at: OffsetDateTime,
    pub item_count: u64,
    /// The snapshot's canonical digest, 64 lowercase hex digits, as verified at intake.
    pub content_digest: String,
    /// The verified snapshot, records sorted by id.
    pub snapshot_json: Value,
    /// The registry evidence of its candidates.
    pub evidence_json: Value,
    /// The `rejected`, `drift` and `retired` notes.
    pub review_json: Value,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

/// One stored candidate: its content, never changed, and its review columns (P-D-268, P-D-269).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaCandidate {
    pub id: Uuid,
    pub target_id: Uuid,
    pub origin: CandidateOrigin,
    /// The snapshot row of a discovered candidate; `None` for a manual one.
    pub snapshot_row_id: Option<Uuid>,
    pub record_id: Uuid,
    pub capability_key: String,
    /// The record as received, an object.
    pub record_json: Value,
    pub record_digest: String,
    pub evidence_json: Value,
    pub state: CandidateState,
    /// The optimistic-concurrency tag: 1 at insert, one more on every review write.
    pub version: i64,
    pub proposal_json: Option<Value>,
    pub proposal_evidence_json: Option<Value>,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
    pub submitted_by: Option<Uuid>,
    pub submitted_at: Option<OffsetDateTime>,
    pub submit_note: Option<String>,
    pub decided_by: Option<Uuid>,
    pub decided_at: Option<OffsetDateTime>,
    pub decided_note: Option<String>,
    pub definition_id: Option<Uuid>,
    pub definition_version: Option<QuotaVersion>,
}

/// What a candidate insert writes: its content columns. It is stored `staged` at version 1.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCandidate {
    pub target_id: Uuid,
    pub origin: CandidateOrigin,
    pub snapshot_row_id: Option<Uuid>,
    pub record_id: Uuid,
    pub capability_key: String,
    pub record_json: Value,
    pub record_digest: String,
    pub evidence_json: Value,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

/// One review transition of a candidate (P-D-269), applied only to the version the caller read.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewWrite {
    /// `staged` to `submitted`, with the proposal and its registry evidence.
    Submit {
        proposal: Value,
        evidence: Value,
        by: Uuid,
        at: OffsetDateTime,
        note: Option<String>,
    },
    /// `submitted` to `approved`, naming the definition version the approval published.
    Approve {
        by: Uuid,
        at: OffsetDateTime,
        note: Option<String>,
        definition_id: Uuid,
        definition_version: QuotaVersion,
    },
    /// `submitted` to `rejected`, with the reason.
    Reject {
        by: Uuid,
        at: OffsetDateTime,
        note: String,
    },
}

impl ReviewWrite {
    /// The state the write moves from.
    #[must_use]
    pub const fn state_before(&self) -> CandidateState {
        match self {
            Self::Submit { .. } => CandidateState::Staged,
            Self::Approve { .. } | Self::Reject { .. } => CandidateState::Submitted,
        }
    }

    /// The state the write moves to.
    #[must_use]
    pub const fn state_after(&self) -> CandidateState {
        match self {
            Self::Submit { .. } => CandidateState::Submitted,
            Self::Approve { .. } => CandidateState::Approved,
            Self::Reject { .. } => CandidateState::Rejected,
        }
    }
}

/// What the candidate list narrows the tenant's candidates by.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CandidateFilter {
    pub target_id: Option<Uuid>,
    pub state: Option<CandidateState>,
}

/// One immutable mapping version of a lineage `(target, capability_key)`, with its qualified
/// profile version (P-D-270).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingVersion {
    pub mapping_id: Uuid,
    pub version: QuotaVersion,
    pub target_id: Uuid,
    pub capability_key: String,
    pub plugin_instance_id: String,
    pub target_key: String,
    pub mapping_key: String,
    pub mapping_json: Value,
    pub digest: String,
    pub profile_id: Uuid,
    pub profile_version: QuotaVersion,
    pub profile_json: Value,
    pub profile_digest: String,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

/// One immutable definition version (P-D-270). It applies to its SKU version and every later one.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionVersion {
    pub definition_id: Uuid,
    pub version: QuotaVersion,
    pub candidate_id: Uuid,
    pub sku_id: Uuid,
    pub sku_version: QuotaVersion,
    /// The metric's deterministic UUID (`GtsId::to_uuid`), for the reads.
    pub metric_uuid: Uuid,
    /// The subject type's deterministic UUID, for the reads.
    pub subject_type_uuid: Uuid,
    pub profile_id: Uuid,
    pub mapping_id: Uuid,
    pub mapping_version: QuotaVersion,
    pub content_json: Value,
    pub digest: String,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
}

/// One availability observation of a definition version (P-D-271); the current one is the
/// highest revision.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub definition_id: Uuid,
    pub version: QuotaVersion,
    pub revision: QuotaVersion,
    pub state: QuotaAvailabilityState,
    /// `None` exactly when the state is enabled.
    pub reason_code: Option<String>,
    /// The qualification evidence the state rests on; `None` until qualification is recorded.
    pub qualification: Option<QuotaOwnerRef>,
    pub observed_at: OffsetDateTime,
    pub decided_by: Uuid,
}

/// What the definition list narrows the tenant's definition versions by (P-D-272).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefinitionFilter {
    pub sku_id: Uuid,
    /// The versions approved at this SKU version or an earlier one apply (P-D-270).
    pub sku_version: QuotaVersion,
    pub subject_type_uuid: Option<Uuid>,
    pub resource_type_uuid: Option<Uuid>,
    pub profile_id: Option<Uuid>,
    /// Only versions whose current availability is enabled.
    pub enabled_only: bool,
}

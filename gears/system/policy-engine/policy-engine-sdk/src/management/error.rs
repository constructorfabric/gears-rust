//! The management surface's typed errors and content limits.
//!
//! [`ManagementError`] is a contract error: on the wire it is an RFC 9457
//! `Problem` whose `error_domain` is [`ERROR_DOMAIN`] and whose `error_code`
//! names the variant, with the variant's fields in `context.data`. Clients
//! match on the code (or rebuild the variant with `TryFrom<Problem>`), never
//! on `detail`.

use serde::{Deserialize, Serialize};
use toolkit_contract::ContractError;
use uuid::Uuid;

use super::models::{BundleId, ValidationFinding, VersionId};

/// The `error_domain` every management error carries on the wire.
pub const ERROR_DOMAIN: &str = "policy_engine.management.v1";

/// Fixed bounds on what the management surface stores, in UTF-8 bytes or
/// counts. The configurable bounds (documents per version, document content
/// bytes) and the admission gate's own bounds (document names, resource types,
/// actions) are not listed here: every [`LimitViolation`] carries its bound.
pub mod limits {
    /// Longest bundle name.
    pub const MAX_BUNDLE_NAME_BYTES: usize = 256;
    /// Longest bundle description.
    pub const MAX_BUNDLE_DESCRIPTION_BYTES: usize = 4_096;
    /// Most resource-type patterns of one document.
    pub const MAX_RESOURCE_TYPES_PER_DOCUMENT: usize = 64;
    /// Most actions of one document.
    pub const MAX_ACTIONS_PER_DOCUMENT: usize = 64;
}

/// Which content bound a [`LimitViolation`] exceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LimitKind {
    /// Too many documents in the version (configured).
    DocumentsPerVersion,
    /// One document's content too large (configured).
    DocumentBytes,
    /// A document name longer than the admission gate accepts in a denial.
    DocumentNameBytes,
    /// Too many resource-type patterns in one document
    /// ([`limits::MAX_RESOURCE_TYPES_PER_DOCUMENT`]).
    ResourceTypesPerDocument,
    /// Too many actions in one document
    /// ([`limits::MAX_ACTIONS_PER_DOCUMENT`]).
    ActionsPerDocument,
    /// A resource-type pattern longer than any resource type the admission
    /// gate accepts.
    ResourceTypeBytes,
    /// An action longer than any action the admission gate accepts.
    ActionBytes,
}

/// One exceeded content bound, attributed to a document where it is
/// document-scoped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitViolation {
    /// Name of the offending document; `None` for a version-level bound.
    pub document: Option<String>,
    /// The bound exceeded.
    pub kind: LimitKind,
    /// The bound.
    pub limit: usize,
    /// The measured value.
    pub actual: usize,
}

/// What a [`ManagementError::NotFound`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ManagementResource {
    /// A bundle.
    Bundle,
    /// A bundle version.
    Version,
    /// An assignment.
    Assignment,
    /// An owning tenant the caller may not manage.
    Tenant,
}

/// Error of the management client. Content the caller may not see is
/// reported exactly like content that does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ContractError, thiserror::Error)]
#[error_domain("policy_engine.management.v1")]
#[non_exhaustive]
pub enum ManagementError {
    /// The content does not exist or the caller may not see it.
    #[error("{resource:?} {id} not found")]
    #[error_code("NOT_FOUND")]
    #[canonical(NotFound)]
    NotFound {
        /// What was looked for.
        resource: ManagementResource,
        /// Its identity.
        id: Uuid,
    },
    /// The version is active or superseded and therefore immutable; only a
    /// draft may be modified or deleted.
    #[error("only a draft version may be changed; seed a new draft")]
    #[error_code("VERSION_NOT_DRAFT")]
    #[canonical(Aborted)]
    VersionNotDraft,
    /// A concurrent change committed first; re-read and retry.
    #[error("a concurrent change committed first; re-read and retry")]
    #[error_code("CONCURRENT_CHANGE")]
    #[canonical(Aborted)]
    ConcurrentChange,
    /// Activation was requested for a draft that fails validation.
    #[error("the draft fails validation ({} finding(s))", findings.len())]
    #[error_code("VALIDATION_FAILED")]
    #[canonical(FailedPrecondition)]
    ValidationFailed {
        /// Every finding.
        findings: Vec<ValidationFinding>,
    },
    /// A bundle with this name already exists in the owning tenant.
    #[error("a bundle named `{name}` already exists in the owning tenant")]
    #[error_code("BUNDLE_NAME_TAKEN")]
    #[canonical(AlreadyExists)]
    BundleNameTaken {
        /// The name.
        name: String,
    },
    /// The bundle already has an open draft; replace or delete it first.
    #[error("bundle {bundle_id} already has an open draft")]
    #[error_code("DRAFT_EXISTS")]
    #[canonical(AlreadyExists)]
    DraftExists {
        /// The bundle.
        bundle_id: BundleId,
    },
    /// The bundle is already assigned to the tenant; update that assignment.
    #[error("bundle {bundle_id} is already assigned to tenant {tenant_id}")]
    #[error_code("ASSIGNMENT_EXISTS")]
    #[canonical(AlreadyExists)]
    AssignmentExists {
        /// The bundle.
        bundle_id: BundleId,
        /// The tenant.
        tenant_id: Uuid,
    },
    /// The version a draft is seeded from is not a retained version of the
    /// same bundle (absent, invisible or of another bundle alike).
    #[error("seed {seed} is not a retained version of this bundle")]
    #[error_code("SEED_NOT_IN_BUNDLE")]
    #[canonical(InvalidArgument)]
    SeedNotInBundle {
        /// The seed.
        seed: VersionId,
    },
    /// Draft content exceeds a content bound.
    #[error("content exceeds {} bound(s)", violations.len())]
    #[error_code("CONTENT_LIMIT_EXCEEDED")]
    #[canonical(InvalidArgument)]
    ContentLimitExceeded {
        /// Every bound exceeded.
        violations: Vec<LimitViolation>,
    },
    /// A document name is empty or holds a control character.
    #[error("document name {name:?} is empty or holds a control character")]
    #[error_code("INVALID_DOCUMENT_NAME")]
    #[canonical(InvalidArgument)]
    InvalidDocumentName {
        /// The name.
        name: String,
    },
    /// Two documents of the content share a name.
    #[error("document name `{name}` is used more than once")]
    #[error_code("DUPLICATE_DOCUMENT_NAME")]
    #[canonical(InvalidArgument)]
    DuplicateDocumentName {
        /// The name.
        name: String,
    },
    /// A bundle field is empty or too long.
    #[error("bundle {field}: {reason}")]
    #[error_code("INVALID_BUNDLE")]
    #[canonical(InvalidArgument)]
    InvalidBundle {
        /// `name` or `description`.
        field: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A listing query the store refused.
    #[error("invalid query: {reason}")]
    #[error_code("INVALID_QUERY")]
    #[canonical(InvalidArgument)]
    InvalidQuery {
        /// What is wrong with it.
        reason: String,
    },
    /// The caller does not hold the capability the operation requires.
    #[error("the caller lacks the capability the operation requires")]
    #[error_code("CAPABILITY_DENIED")]
    #[canonical(PermissionDenied)]
    CapabilityDenied,
    /// The target tenant lies behind a self-managed barrier from the
    /// caller's context.
    #[error("the target tenant is behind a self-managed barrier")]
    #[error_code("TENANT_BOUNDARY")]
    #[canonical(PermissionDenied)]
    TenantBoundary,
    /// A dependency (authorisation, tenant hierarchy, types registry) could
    /// not answer; retry later.
    #[error("a dependency of policy management is unavailable")]
    #[error_code("UNAVAILABLE")]
    #[canonical(ServiceUnavailable)]
    Unavailable,
    /// An internal failure; its detail is logged, never returned.
    #[error("policy management failed internally")]
    #[error_code("INTERNAL")]
    #[canonical(Internal)]
    Internal,
}

//! Wire DTOs of the policy administration REST API, and their mapping to and
//! from the SDK's management models (`policy_engine_sdk::management`).
//!
//! Every DTO is prefixed `PolicyEngine...Dto` so its `OpenAPI` component name
//! cannot collide with another gear's, uses `snake_case` fields
//! (`#[toolkit_macros::api_dto]`), and carries RFC 3339 timestamps as
//! strings. Content - a document's source - appears only in
//! [`PolicyEngineDocumentDto`] (nested in [`PolicyEngineVersionDetailDto`])
//! and [`PolicyEngineDocumentSpecDto`] (nested in
//! [`PolicyEngineReplaceDraftContentRequestDto`]).

use policy_engine_sdk::management::{
    AssignmentSpec, Bundle, BundlePatch, BundleVersion, Document, NewBundle, ValidationFinding,
    ValidationReport, VersionContent, VersionDetail, VersionState,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

/// RFC 3339, falling back to the Unix timestamp on the (practically
/// unreachable) formatting failure - never a panic on a value read back from
/// storage.
fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

fn rfc3339_opt(at: Option<OffsetDateTime>) -> Option<String> {
    at.map(rfc3339)
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// One policy document as an author writes it, **carrying content**. Never
/// nested anywhere but [`PolicyEngineReplaceDraftContentRequestDto`].
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(request)]
pub struct PolicyEngineDocumentSpecDto {
    /// Author-facing name, unique within the version.
    pub name: String,
    /// Rego source defining the boolean rule `deny`.
    pub content: String,
    /// GTS type patterns the document applies to; a concrete type id is a
    /// pattern without a wildcard.
    #[serde(default)]
    pub resource_types: Vec<String>,
    /// Actions the document applies to; empty means every action.
    #[serde(default)]
    pub actions: Vec<String>,
}

impl From<PolicyEngineDocumentSpecDto> for policy_engine_sdk::management::DocumentSpec {
    fn from(value: PolicyEngineDocumentSpecDto) -> Self {
        Self {
            name: value.name,
            content: value.content,
            resource_types: value.resource_types,
            actions: value.actions,
        }
    }
}

/// A stored document: its identity plus what the author wrote, **carrying
/// content**. Never nested anywhere but [`PolicyEngineVersionDetailDto`].
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyEngineDocumentDto {
    /// Document identity, named in refusals.
    pub id: Uuid,
    /// Author-facing name, unique within the version.
    pub name: String,
    /// Rego source, as stored.
    pub content: String,
    /// GTS type patterns the document applies to.
    pub resource_types: Vec<String>,
    /// Actions the document applies to; empty means every action.
    pub actions: Vec<String>,
}

impl From<Document> for PolicyEngineDocumentDto {
    fn from(value: Document) -> Self {
        Self {
            id: value.id,
            name: value.spec.name,
            content: value.spec.content,
            resource_types: value.spec.resource_types,
            actions: value.spec.actions,
        }
    }
}

// ---------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------

/// A policy bundle: a stable identity and a name within its owning tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyEngineBundleDto {
    /// Bundle identity, stable across versions.
    pub id: Uuid,
    /// Owning tenant.
    pub owner_tenant_id: Uuid,
    /// Name, unique within the owning tenant.
    pub name: String,
    /// What the bundle governs (empty when none was given).
    pub description: String,
    /// The version currently active, if any.
    pub active_version_id: Option<Uuid>,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Author identity.
    pub created_by: Uuid,
    /// Last modification time (RFC 3339).
    pub updated_at: String,
}

impl From<Bundle> for PolicyEngineBundleDto {
    fn from(value: Bundle) -> Self {
        Self {
            id: value.id,
            owner_tenant_id: value.owner_tenant_id,
            name: value.name,
            description: value.description,
            active_version_id: value.active_version_id,
            created_at: rfc3339(value.created_at),
            created_by: value.created_by,
            updated_at: rfc3339(value.updated_at),
        }
    }
}

/// `POST /policy-engine/v1/bundles` request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
pub struct PolicyEngineCreateBundleRequestDto {
    /// Owning tenant; absent means the caller's own tenant.
    #[serde(default)]
    pub owner_tenant_id: Option<Uuid>,
    /// Name, unique within the owning tenant.
    pub name: String,
    /// What the bundle governs.
    #[serde(default)]
    pub description: String,
}

impl From<PolicyEngineCreateBundleRequestDto> for NewBundle {
    fn from(value: PolicyEngineCreateBundleRequestDto) -> Self {
        let mut bundle = Self::new(value.name).with_description(value.description);
        if let Some(owner) = value.owner_tenant_id {
            bundle = bundle.with_owner_tenant(owner);
        }
        bundle
    }
}

/// `PATCH /policy-engine/v1/bundles/{id}` request. An absent field leaves the
/// attribute unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[toolkit_macros::api_dto(request)]
pub struct PolicyEngineUpdateBundleRequestDto {
    /// New name; absent leaves it unchanged.
    #[serde(default)]
    pub name: Option<String>,
    /// New description (an empty string clears it); absent leaves it
    /// unchanged.
    #[serde(default)]
    pub description: Option<String>,
}

impl From<PolicyEngineUpdateBundleRequestDto> for BundlePatch {
    fn from(value: PolicyEngineUpdateBundleRequestDto) -> Self {
        Self {
            name: value.name,
            description: value.description,
        }
    }
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// Lifecycle state of a bundle version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum PolicyEngineVersionStateDto {
    /// Editable; never evaluated live.
    Draft,
    /// Immutable; governs through every assignment of its bundle.
    Active,
    /// Immutable and replaced by a later activation; retained as a seed.
    Superseded,
}

impl From<VersionState> for PolicyEngineVersionStateDto {
    fn from(value: VersionState) -> Self {
        match value {
            VersionState::Draft => Self::Draft,
            VersionState::Active => Self::Active,
            VersionState::Superseded => Self::Superseded,
        }
    }
}

/// A bundle version summary: no documents, no content.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyEngineVersionDto {
    /// Version identity.
    pub id: Uuid,
    /// Owning bundle.
    pub bundle_id: Uuid,
    /// Owning tenant (the bundle's).
    pub owner_tenant_id: Uuid,
    /// Monotonic version number within the bundle, starting at 1.
    pub ordinal: u32,
    /// Lifecycle state.
    pub state: PolicyEngineVersionStateDto,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Activation time (RFC 3339); absent while draft.
    pub activated_at: Option<String>,
    /// Activating identity; absent while draft.
    pub activated_by: Option<Uuid>,
}

impl From<BundleVersion> for PolicyEngineVersionDto {
    fn from(value: BundleVersion) -> Self {
        Self {
            id: value.id,
            bundle_id: value.bundle_id,
            owner_tenant_id: value.owner_tenant_id,
            ordinal: value.ordinal,
            state: value.state.into(),
            created_at: rfc3339(value.created_at),
            activated_at: rfc3339_opt(value.activated_at),
            activated_by: value.activated_by,
        }
    }
}

/// `GET .../versions/{version}`, and the result of replacing a draft's
/// content: **the read/write surface that carries content**, alongside
/// [`PolicyEngineReplaceDraftContentRequestDto`].
#[derive(Debug, Clone, PartialEq)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyEngineVersionDetailDto {
    /// The version.
    pub version: PolicyEngineVersionDto,
    /// Its documents, ordered by name.
    pub documents: Vec<PolicyEngineDocumentDto>,
}

impl From<VersionDetail> for PolicyEngineVersionDetailDto {
    fn from(value: VersionDetail) -> Self {
        Self {
            version: value.version.into(),
            documents: value.documents.into_iter().map(Into::into).collect(),
        }
    }
}

/// `POST /policy-engine/v1/bundles/{id}/versions` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[toolkit_macros::api_dto(request)]
pub struct PolicyEngineCreateDraftVersionRequestDto {
    /// Seed the draft from a retained version of the same bundle; absent
    /// creates an empty draft.
    #[serde(default)]
    pub seed_from: Option<Uuid>,
}

/// `PUT .../versions/{version}` request: the complete content of a draft,
/// replaced as a whole. **Carries content.**
#[derive(Debug, Clone, PartialEq, Default)]
#[toolkit_macros::api_dto(request)]
pub struct PolicyEngineReplaceDraftContentRequestDto {
    /// The documents; names must be unique within the version.
    #[serde(default)]
    pub documents: Vec<PolicyEngineDocumentSpecDto>,
}

impl From<PolicyEngineReplaceDraftContentRequestDto> for VersionContent {
    fn from(value: PolicyEngineReplaceDraftContentRequestDto) -> Self {
        Self {
            documents: value.documents.into_iter().map(Into::into).collect(),
        }
    }
}

/// One validation failure, reported against the document that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyEngineValidationFindingDto {
    /// The offending document; absent for a finding about the version as a
    /// whole.
    pub document_name: Option<String>,
    /// Stable code from `policy_engine_sdk::management::finding`.
    pub code: String,
    /// Human-readable explanation for the author.
    pub message: String,
}

impl From<ValidationFinding> for PolicyEngineValidationFindingDto {
    fn from(value: ValidationFinding) -> Self {
        Self {
            document_name: value.document_name,
            code: value.code,
            message: value.message,
        }
    }
}

/// `POST .../versions/{version}/validate` response.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyEngineValidationReportDto {
    /// The version validated.
    pub version_id: Uuid,
    /// `true` when no finding was reported.
    pub valid: bool,
    /// Every failure found; empty means valid.
    pub findings: Vec<PolicyEngineValidationFindingDto>,
    /// When the validation ran (RFC 3339).
    pub validated_at: String,
}

impl From<ValidationReport> for PolicyEngineValidationReportDto {
    fn from(value: ValidationReport) -> Self {
        Self {
            version_id: value.version_id,
            valid: value.findings.is_empty(),
            findings: value.findings.into_iter().map(Into::into).collect(),
            validated_at: rfc3339(value.validated_at),
        }
    }
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

/// A bundle bound to a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyEngineAssignmentDto {
    /// Assignment identity.
    pub id: Uuid,
    /// Assigned bundle.
    pub bundle_id: Uuid,
    /// Tenant the assignment attaches to.
    pub tenant_id: Uuid,
    /// Tenant of the assigning administrator.
    pub owner_tenant_id: Uuid,
    /// Whether a denial by the bundle refuses the operation (`true`) or is
    /// only reported (`false`).
    pub enforce: bool,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Last modification time (RFC 3339).
    pub updated_at: String,
}

impl From<policy_engine_sdk::management::Assignment> for PolicyEngineAssignmentDto {
    fn from(value: policy_engine_sdk::management::Assignment) -> Self {
        Self {
            id: value.id,
            bundle_id: value.bundle_id,
            tenant_id: value.tenant_id,
            owner_tenant_id: value.owner_tenant_id,
            enforce: value.enforce,
            created_at: rfc3339(value.created_at),
            updated_at: rfc3339(value.updated_at),
        }
    }
}

/// `POST /policy-engine/v1/assignments` request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
pub struct PolicyEngineCreateAssignmentRequestDto {
    /// Bundle to assign.
    pub bundle_id: Uuid,
    /// Tenant to attach to.
    pub tenant_id: Uuid,
    /// Whether denials are enforced (default `true`) or only reported.
    #[serde(default = "default_enforce")]
    pub enforce: bool,
}

fn default_enforce() -> bool {
    true
}

impl From<PolicyEngineCreateAssignmentRequestDto> for AssignmentSpec {
    fn from(value: PolicyEngineCreateAssignmentRequestDto) -> Self {
        let spec = Self::new(value.bundle_id, value.tenant_id);
        if value.enforce { spec } else { spec.shadow() }
    }
}

/// `PATCH /policy-engine/v1/assignments/{id}` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
pub struct PolicyEngineUpdateAssignmentRequestDto {
    /// Whether denials are enforced or only reported.
    pub enforce: bool,
}

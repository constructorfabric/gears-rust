//! Management models: bundles, versions, documents and assignments.
//!
//! Plain SDK value types without serde: the REST layer owns its DTOs and
//! projects these unchanged. Everything here speaks GTS identifiers.

use time::OffsetDateTime;
use uuid::Uuid;

/// Identity of a policy bundle.
pub type BundleId = Uuid;
/// Identity of a bundle version.
pub type VersionId = Uuid;
/// Identity of a policy document (named in refusals).
pub type DocumentId = Uuid;
/// Identity of a bundle-to-tenant assignment.
pub type AssignmentId = Uuid;

// ---------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------

/// A policy bundle: a stable identity and a name within its owning tenant.
/// It carries no content; content lives in its [`BundleVersion`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// Bundle identity, stable across versions.
    pub id: BundleId,
    /// Owning tenant.
    pub owner_tenant_id: Uuid,
    /// Name, unique within the owning tenant.
    pub name: String,
    /// What the bundle governs (empty when none was given).
    pub description: String,
    /// The version currently active, if any.
    pub active_version_id: Option<VersionId>,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Author identity.
    pub created_by: Uuid,
    /// Last modification time.
    pub updated_at: OffsetDateTime,
}

/// Request to create a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBundle {
    /// Owning tenant; `None` means the caller's own tenant.
    pub owner_tenant_id: Option<Uuid>,
    /// Name, unique within the owning tenant.
    pub name: String,
    /// What the bundle governs.
    pub description: String,
}

impl NewBundle {
    /// A bundle named `name`, owned by the caller's tenant, without a
    /// description.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            owner_tenant_id: None,
            name: name.into(),
            description: String::new(),
        }
    }

    /// Sets the owning tenant.
    #[must_use]
    pub fn with_owner_tenant(mut self, tenant_id: Uuid) -> Self {
        self.owner_tenant_id = Some(tenant_id);
        self
    }

    /// Sets the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// Change to a bundle's mutable attributes. `None` leaves a field unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BundlePatch {
    /// New name.
    pub name: Option<String>,
    /// New description (an empty string clears it).
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// Lifecycle state of a bundle version: a draft is edited, then activated;
/// activating a draft supersedes the bundle's previously active version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VersionState {
    /// Editable; never evaluated live.
    Draft,
    /// Immutable; governs through every assignment of its bundle. At most
    /// one per bundle.
    Active,
    /// Immutable and replaced by a later activation; retained as a seed.
    Superseded,
}

impl VersionState {
    /// Stable `snake_case` label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Superseded => "superseded",
        }
    }
}

/// One revision of a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleVersion {
    /// Version identity.
    pub id: VersionId,
    /// Owning bundle.
    pub bundle_id: BundleId,
    /// Owning tenant (the bundle's).
    pub owner_tenant_id: Uuid,
    /// Monotonic version number within the bundle, starting at 1.
    pub ordinal: u32,
    /// Lifecycle state.
    pub state: VersionState,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Activation time; `None` while draft.
    pub activated_at: Option<OffsetDateTime>,
    /// Activating identity; `None` while draft.
    pub activated_by: Option<Uuid>,
}

/// A version together with its documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDetail {
    /// The version.
    pub version: BundleVersion,
    /// Its documents, ordered by name.
    pub documents: Vec<Document>,
}

/// The complete content of a draft, replaced as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VersionContent {
    /// The documents; names must be unique within the version.
    pub documents: Vec<DocumentSpec>,
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// One policy document as the author writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSpec {
    /// Author-facing name, unique within the version.
    pub name: String,
    /// Rego source defining the boolean rule
    /// [`POLICY_ENTRYPOINT`](crate::gts::POLICY_ENTRYPOINT); the operation is
    /// denied when it is `true`.
    pub content: String,
    /// GTS type patterns the document applies to; a concrete type id is a
    /// pattern without a wildcard.
    pub resource_types: Vec<String>,
    /// Actions the document applies to; empty means every action.
    pub actions: Vec<String>,
}

/// A stored document: its identity plus what the author wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Document identity, named in refusals.
    pub id: DocumentId,
    /// The document as written.
    pub spec: DocumentSpec,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One validation failure, reported against the document that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationFinding {
    /// The offending document; `None` for a finding about the version as a
    /// whole (for example too many documents).
    pub document_name: Option<String>,
    /// Stable code from [`finding`](super::finding).
    pub code: String,
    /// Human-readable explanation for the author.
    pub message: String,
}

/// Outcome of validating a version. Validation never activates anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    /// The version validated.
    pub version_id: VersionId,
    /// Every failure found; empty means valid.
    pub findings: Vec<ValidationFinding>,
    /// When the validation ran.
    pub validated_at: OffsetDateTime,
}

impl ValidationReport {
    /// `true` when no finding was reported.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.findings.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

/// A bundle bound to a tenant. It names no version: it governs through
/// whichever version of the bundle is active, and survives activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// Assignment identity.
    pub id: AssignmentId,
    /// Assigned bundle.
    pub bundle_id: BundleId,
    /// Tenant the assignment attaches to; it applies there and below.
    pub tenant_id: Uuid,
    /// Tenant of the assigning administrator.
    pub owner_tenant_id: Uuid,
    /// `true` when a denial by the bundle refuses the operation; `false`
    /// only reports it (a shadow denial).
    pub enforce: bool,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Last modification time.
    pub updated_at: OffsetDateTime,
}

/// Request to assign a bundle to a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentSpec {
    /// Bundle to assign.
    pub bundle_id: BundleId,
    /// Tenant to attach to; refused with the boundary cause when it lies
    /// behind a self-managed barrier from the caller's context.
    pub tenant_id: Uuid,
    /// Whether denials are enforced (the default) or only reported.
    pub enforce: bool,
}

impl AssignmentSpec {
    /// An enforcing assignment of `bundle_id` to `tenant_id`.
    #[must_use]
    pub fn new(bundle_id: BundleId, tenant_id: Uuid) -> Self {
        Self {
            bundle_id,
            tenant_id,
            enforce: true,
        }
    }

    /// Makes the assignment report denials without enforcing them.
    #[must_use]
    pub fn shadow(mut self) -> Self {
        self.enforce = false;
        self
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;

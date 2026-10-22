//! Policy content model: bundles, versions, documents and assignments.
//!
//! These are domain values, free of storage and transport concerns.

use admission_control_sdk::{IDENTIFIER_MAX_LEN, MAX_DOCUMENT_NAME_LEN, RESOURCE_TYPE_MAX_LEN};
use std::collections::BTreeMap;

use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

pub use policy_engine_sdk::management::{AssignmentId, BundleId, DocumentId, VersionId};

/// Lifecycle state of a bundle version.
///
/// Content may be modified only while [`VersionState::Draft`]; at most one
/// version of a bundle is [`VersionState::Active`]; activating a draft
/// supersedes the previously active version.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VersionState {
    /// Editable; code `1`.
    Draft,
    /// In force for every assignment of the bundle; code `2`.
    Active,
    /// Replaced by a later activation; retained, immutable, seedable; code `3`.
    Superseded,
}

impl VersionState {
    /// Stable storage code (`policy_engine__bundle_version.state`).
    #[must_use]
    pub const fn code(self) -> i16 {
        match self {
            Self::Draft => 1,
            Self::Active => 2,
            Self::Superseded => 3,
        }
    }

    /// Inverse of [`Self::code`]; `None` for an unknown code.
    #[must_use]
    pub const fn from_code(code: i16) -> Option<Self> {
        match code {
            1 => Some(Self::Draft),
            2 => Some(Self::Active),
            3 => Some(Self::Superseded),
            _ => None,
        }
    }
}

/// One policy document within a version: Rego content defining the boolean
/// rule `deny`, and where it applies.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Document identity.
    pub id: DocumentId,
    /// Author-facing name, unique within the version.
    pub name: String,
    /// Rego source, exactly as submitted.
    pub content: String,
    /// GTS type patterns the document applies to (a concrete id is a pattern
    /// without a wildcard).
    pub resource_types: Vec<String>,
    /// Actions the document applies to; empty means every action.
    pub actions: Vec<String>,
}

/// Stable identity and ownership of a named body of policy. Carries no
/// content.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// Bundle identity.
    pub id: BundleId,
    /// Owning tenant; the scope column.
    pub owner_tenant_id: Uuid,
    /// Name, unique within the owning tenant.
    pub name: String,
    /// What the bundle governs.
    pub description: String,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Author identity.
    pub created_by: Uuid,
    /// Last modification time.
    pub updated_at: OffsetDateTime,
}

/// One revision of a bundle, immutable once it leaves draft except for the
/// transition to superseded.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleVersion {
    /// Version identity.
    pub id: VersionId,
    /// Owning bundle.
    pub bundle_id: BundleId,
    /// Owning tenant; the scope column.
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
    /// The version's documents.
    pub documents: Vec<Document>,
}

/// Binds a bundle to a tenant. Resolves to the bundle's active version.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// Assignment identity.
    pub id: AssignmentId,
    /// Assigned bundle.
    pub bundle_id: BundleId,
    /// Tenant the assignment attaches to.
    pub tenant_id: Uuid,
    /// Owning tenant of the assigning administrator; the scope column.
    pub owner_tenant_id: Uuid,
    /// Whether a denial by the bundle refuses the operation (`true`) or is
    /// only reported as a shadow denial (`false`).
    pub enforce: bool,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Subject that created the assignment.
    pub created_by: Uuid,
    /// Last modification time.
    pub updated_at: OffsetDateTime,
    /// Subject of the last modification (the creator until changed).
    pub updated_by: Uuid,
}

// ---------------------------------------------------------------------------
// Content limits
// ---------------------------------------------------------------------------

/// Operational content bounds, built from the validated configuration.
///
/// Sizes are measured in UTF-8 bytes of document content.
#[domain_model]
#[allow(clippy::struct_field_names)] // names mirror the configuration keys
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentLimits {
    /// Maximum documents in one version.
    pub max_documents_per_version: usize,
    /// Maximum content bytes of one document.
    pub max_document_bytes: usize,
}

pub use policy_engine_sdk::management::{LimitKind, LimitViolation, limits};

impl ContentLimits {
    /// Every bound `documents` exceed: version-level violations first, then
    /// per-document violations in input order. Empty when within bounds.
    #[must_use]
    pub fn check(&self, documents: &[Document]) -> Vec<LimitViolation> {
        let mut out = Vec::new();
        if documents.len() > self.max_documents_per_version {
            out.push(LimitViolation {
                document: None,
                kind: LimitKind::DocumentsPerVersion,
                limit: self.max_documents_per_version,
                actual: documents.len(),
            });
        }
        out.extend(documents.iter().flat_map(|d| self.check_document(d)));
        out
    }

    /// The document-scoped bounds `document` exceeds: the configured content
    /// size, the fixed [`limits`] on its resource types and actions, and the
    /// admission gate's bounds on a document name, a resource type and an
    /// action, so stored content never names what the gate cannot carry.
    #[must_use]
    pub fn check_document(&self, document: &Document) -> Vec<LimitViolation> {
        let longest = |entries: &[String]| entries.iter().map(String::len).max().unwrap_or(0);
        [
            (
                LimitKind::DocumentBytes,
                self.max_document_bytes,
                document.content.len(),
            ),
            (
                LimitKind::DocumentNameBytes,
                MAX_DOCUMENT_NAME_LEN,
                document.name.len(),
            ),
            (
                LimitKind::ResourceTypesPerDocument,
                limits::MAX_RESOURCE_TYPES_PER_DOCUMENT,
                document.resource_types.len(),
            ),
            (
                LimitKind::ActionsPerDocument,
                limits::MAX_ACTIONS_PER_DOCUMENT,
                document.actions.len(),
            ),
            (
                LimitKind::ResourceTypeBytes,
                RESOURCE_TYPE_MAX_LEN,
                longest(&document.resource_types),
            ),
            (
                LimitKind::ActionBytes,
                IDENTIFIER_MAX_LEN,
                longest(&document.actions),
            ),
        ]
        .into_iter()
        .filter(|(_, limit, actual)| actual > limit)
        .map(|(kind, limit, actual)| LimitViolation {
            document: Some(document.name.clone()),
            kind,
            limit,
            actual,
        })
        .collect()
    }
}

/// Whether `name` may name a document: not blank and free of control
/// characters, so it can appear in a denial unchanged.
#[must_use]
pub fn document_name_valid(name: &str) -> bool {
    !name.trim().is_empty() && !name.chars().any(char::is_control)
}

/// The names more than one of `documents` share, with how many share each,
/// in name order. Empty when every name is unique.
#[must_use]
pub fn duplicate_names(documents: &[Document]) -> Vec<(&str, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for document in documents {
        *counts.entry(document.name.as_str()).or_default() += 1;
    }
    counts.into_iter().filter(|(_, n)| *n > 1).collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "content_tests.rs"]
mod content_tests;

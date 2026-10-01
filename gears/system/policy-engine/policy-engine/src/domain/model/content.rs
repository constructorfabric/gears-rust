//! Policy content model: bundles, versions, documents and assignments.
//!
//! These are domain values, free of storage and transport concerns.

use std::fmt;

use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[domain_model]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Wraps an existing UUID.
            #[must_use]
            pub const fn new(id: Uuid) -> Self {
                Self(id)
            }

            /// The wrapped UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id!(
    /// Identity of a policy bundle, stable across its versions.
    BundleId
);
uuid_id!(
    /// Identity of one bundle version.
    VersionId
);
uuid_id!(
    /// Identity of one policy document; refusals name it.
    DocumentId
);
uuid_id!(
    /// Identity of one assignment; the final ordering key of resolution.
    AssignmentId
);

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
    /// Monotonic version number within the bundle.
    pub ordinal: i32,
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
    /// Assignment identity; the final ordering key.
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
    /// Last modification time.
    pub updated_at: OffsetDateTime,
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

/// Which content bound a [`LimitViolation`] exceeds.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitKind {
    /// Too many documents in the version.
    DocumentsPerVersion,
    /// One document's content too large.
    DocumentBytes,
}

/// One exceeded content bound, attributed to a document where it is
/// document-scoped.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitViolation {
    /// Name of the offending document; `None` for a version-level bound.
    pub document: Option<String>,
    /// The bound exceeded.
    pub kind: LimitKind,
    /// The configured bound.
    pub limit: usize,
    /// The measured value.
    pub actual: usize,
}

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
        out.extend(documents.iter().filter_map(|d| self.check_document(d)));
        out
    }

    /// The document-scoped bound `document` exceeds, if any.
    #[must_use]
    pub fn check_document(&self, document: &Document) -> Option<LimitViolation> {
        (document.content.len() > self.max_document_bytes).then(|| LimitViolation {
            document: Some(document.name.clone()),
            kind: LimitKind::DocumentBytes,
            limit: self.max_document_bytes,
            actual: document.content.len(),
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "content_tests.rs"]
mod content_tests;

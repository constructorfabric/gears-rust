//! Policy content model: bundles, versions, documents, assignments and
//! content limits.

pub mod content;

pub use content::{
    Assignment, AssignmentId, Bundle, BundleId, BundleVersion, ContentLimits, Document, DocumentId,
    LimitKind, LimitViolation, VersionId, VersionState,
};

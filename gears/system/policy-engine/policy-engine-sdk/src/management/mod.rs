//! The management surface of the policy engine.
//!
//! - [`PolicyManagementClientV1`] - content lifecycle and assignments. It
//!   backs every endpoint of the policy administration REST API.
//! - [`models`] - bundles, versions, documents and assignments.
//! - [`odata`] - the `OData` paging schema of the bundle listing.
//! - [`reason`] - stable reason codes the management errors carry.
//! - [`finding`] - stable codes of validation findings.
//!
//! # Errors
//!
//! Every method returns [`ManagementError`], the platform's
//! [`CanonicalError`]:
//!
//! | Condition | Category (HTTP) | Reason |
//! |---|---|---|
//! | Modification of a version that is not a draft | `aborted` (409) | [`VERSION_NOT_DRAFT`](reason::VERSION_NOT_DRAFT) |
//! | A concurrent change committed first | `aborted` (409) | [`CONCURRENT_CHANGE`](reason::CONCURRENT_CHANGE) |
//! | Activation of a draft that fails validation | `failed_precondition` (400) | [`VALIDATION_FAILED`](reason::VALIDATION_FAILED) |
//! | A bundle name already used in the owning tenant | `already_exists` (409) | [`BUNDLE_NAME_TAKEN`](reason::BUNDLE_NAME_TAKEN) |
//! | A bundle that already has an open draft | `already_exists` (409) | [`DRAFT_EXISTS`](reason::DRAFT_EXISTS) |
//! | A second assignment of a bundle to the same tenant | `already_exists` (409) | [`ASSIGNMENT_EXISTS`](reason::ASSIGNMENT_EXISTS) |
//! | A tenant behind a self-managed barrier from the caller's context | `permission_denied` (403) | [`TENANT_BOUNDARY`](reason::TENANT_BOUNDARY) |
//! | The caller lacks the capability the operation requires | `permission_denied` (403) | [`CAPABILITY_DENIED`](reason::CAPABILITY_DENIED) |
//! | Malformed input (limits, seed) | `invalid_argument` (400) | the matching code in [`reason`] |
//! | Content the caller may not see, or that does not exist | `not_found` (404) | - |
//!
//! # Authorisation
//!
//! Each method is authorised against the caller's own `SecurityContext` for
//! exactly one of the capabilities of
//! [`Capability`](crate::gts::permissions::Capability); no capability implies
//! another.

use toolkit_canonical_errors::CanonicalError;

pub mod client;
pub mod models;
pub mod odata;

pub use client::PolicyManagementClientV1;
pub use models::{
    Assignment, AssignmentId, AssignmentSpec, Bundle, BundleId, BundlePatch, BundleVersion,
    Document, DocumentId, DocumentSpec, NewBundle, ValidationFinding, ValidationReport,
    VersionContent, VersionDetail, VersionId, VersionState,
};

/// Error of the management client: the platform's canonical error, carrying
/// a reason code from [`reason`] where the condition has one.
pub type ManagementError = CanonicalError;

/// Stable reason codes of the management surface. See the table in the
/// [module documentation](self) for the category each travels in.
pub mod reason {
    /// The version is active or superseded and therefore immutable; only a
    /// draft may be modified, validated for activation, or deleted.
    pub const VERSION_NOT_DRAFT: &str = "VERSION_NOT_DRAFT";
    /// A concurrent change committed first; re-read and retry.
    pub const CONCURRENT_CHANGE: &str = "CONCURRENT_CHANGE";
    /// Activation was requested for a draft that fails validation; the
    /// findings are available from `validate_version`.
    pub const VALIDATION_FAILED: &str = "VALIDATION_FAILED";
    /// A bundle with this name already exists in the owning tenant.
    pub const BUNDLE_NAME_TAKEN: &str = "BUNDLE_NAME_TAKEN";
    /// The bundle already has an open draft.
    pub const DRAFT_EXISTS: &str = "DRAFT_EXISTS";
    /// The bundle is already assigned to this tenant.
    pub const ASSIGNMENT_EXISTS: &str = "ASSIGNMENT_EXISTS";
    /// The version a draft is seeded from is not a retained version of the
    /// same bundle.
    pub const SEED_NOT_IN_BUNDLE: &str = "SEED_NOT_IN_BUNDLE";
    /// Draft content exceeds an operational limit (documents per version,
    /// document or version bytes).
    pub const CONTENT_LIMIT_EXCEEDED: &str = "CONTENT_LIMIT_EXCEEDED";
    /// The caller does not hold the capability the operation requires.
    pub const CAPABILITY_DENIED: &str = "CAPABILITY_DENIED";
    /// The target tenant lies behind a self-managed barrier from the caller's
    /// context (also the engine's denial code for an unreachable resource tenant).
    pub const TENANT_BOUNDARY: &str = crate::error::reason::TENANT_BOUNDARY;

    /// Every management reason code, in declaration order.
    pub const ALL: [&str; 10] = [
        VERSION_NOT_DRAFT,
        CONCURRENT_CHANGE,
        VALIDATION_FAILED,
        BUNDLE_NAME_TAKEN,
        DRAFT_EXISTS,
        ASSIGNMENT_EXISTS,
        SEED_NOT_IN_BUNDLE,
        CONTENT_LIMIT_EXCEEDED,
        CAPABILITY_DENIED,
        TENANT_BOUNDARY,
    ];
}

/// Stable codes of [`ValidationFinding::code`].
pub mod finding {
    /// The Rego source does not parse.
    pub const SYNTAX_ERROR: &str = "SYNTAX_ERROR";
    /// The content does not define the boolean rule
    /// [`POLICY_ENTRYPOINT`](crate::gts::POLICY_ENTRYPOINT).
    pub const ENTRYPOINT_MISSING: &str = "ENTRYPOINT_MISSING";
    /// The content uses a builtin on the determinism or resource denylist.
    pub const DENYLISTED_BUILTIN: &str = "DENYLISTED_BUILTIN";
    /// A concrete resource-type identifier is not known to the types
    /// registry.
    pub const RESOURCE_TYPE_UNKNOWN: &str = "RESOURCE_TYPE_UNKNOWN";
    /// A document lists no resource types, so it would apply to nothing.
    pub const RESOURCE_TYPES_EMPTY: &str = "RESOURCE_TYPES_EMPTY";
    /// A resource-type pattern is not valid GTS pattern syntax.
    pub const INVALID_PATTERN: &str = "INVALID_PATTERN";
    /// Two documents of the version share a name.
    pub const DUPLICATE_DOCUMENT_NAME: &str = "DUPLICATE_DOCUMENT_NAME";
    /// An operational content limit is exceeded.
    pub const LIMIT_EXCEEDED: &str = "LIMIT_EXCEEDED";

    /// Every finding code, in declaration order.
    pub const ALL: [&str; 8] = [
        SYNTAX_ERROR,
        ENTRYPOINT_MISSING,
        DENYLISTED_BUILTIN,
        RESOURCE_TYPE_UNKNOWN,
        RESOURCE_TYPES_EMPTY,
        INVALID_PATTERN,
        DUPLICATE_DOCUMENT_NAME,
        LIMIT_EXCEEDED,
    ];
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::collections::BTreeSet;

    use super::{finding, reason};

    fn assert_codes(codes: &[&str]) {
        let unique: BTreeSet<&str> = codes.iter().copied().collect();
        assert_eq!(unique.len(), codes.len(), "codes collide: {codes:?}");
        for code in codes {
            assert!(
                code.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'),
                "{code} is not SCREAMING_SNAKE_CASE"
            );
        }
    }

    #[test]
    fn reason_and_finding_codes_are_distinct_and_stable() {
        assert_codes(&reason::ALL);
        assert_codes(&finding::ALL);
        assert_eq!(reason::VERSION_NOT_DRAFT, "VERSION_NOT_DRAFT");
        assert_eq!(
            reason::TENANT_BOUNDARY,
            crate::error::reason::TENANT_BOUNDARY
        );
    }
}

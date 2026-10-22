//! The management surface of the policy engine.
//!
//! - [`PolicyManagementClientV1`] - content lifecycle and assignments. It
//!   backs every endpoint of the policy administration REST API.
//! - [`models`] - bundles, versions, documents and assignments.
//! - [`odata`] - the `OData` paging schema of the bundle listing.
//! - [`error`] - the typed [`ManagementError`] and the content limits.
//! - [`finding`] - stable codes of validation findings.
//!
//! # Errors
//!
//! Every method returns [`ManagementError`], a typed contract error. On the
//! wire it is an RFC 9457 `Problem` carrying `error_domain`
//! [`ERROR_DOMAIN`] and the variant's `error_code`; its fields travel in
//! `context.data`. Content the caller may not see is reported exactly like
//! content that does not exist.
//!
//! # Authorisation
//!
//! Each method is authorised against the caller's own `SecurityContext` for
//! exactly one of the capabilities of
//! [`Capability`](crate::gts::permissions::Capability); no capability implies
//! another.

pub mod client;
pub mod error;
pub mod models;
pub mod odata;

pub use client::PolicyManagementClientV1;
pub use error::{
    ERROR_DOMAIN, LimitKind, LimitViolation, ManagementError, ManagementResource, limits,
};
pub use models::{
    Assignment, AssignmentId, AssignmentSpec, Bundle, BundleId, BundlePatch, BundleVersion,
    Document, DocumentId, DocumentSpec, NewBundle, ValidationFinding, ValidationReport,
    VersionContent, VersionDetail, VersionId, VersionState,
};

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
    /// A document name is empty or holds a control character.
    pub const INVALID_DOCUMENT_NAME: &str = "INVALID_DOCUMENT_NAME";

    /// Every finding code, in declaration order.
    pub const ALL: [&str; 9] = [
        SYNTAX_ERROR,
        ENTRYPOINT_MISSING,
        DENYLISTED_BUILTIN,
        RESOURCE_TYPE_UNKNOWN,
        RESOURCE_TYPES_EMPTY,
        INVALID_PATTERN,
        DUPLICATE_DOCUMENT_NAME,
        LIMIT_EXCEEDED,
        INVALID_DOCUMENT_NAME,
    ];
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::collections::BTreeSet;

    use super::finding;

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
    fn finding_codes_are_distinct_and_stable() {
        assert_codes(&finding::ALL);
        assert_eq!(finding::SYNTAX_ERROR, "SYNTAX_ERROR");
    }
}

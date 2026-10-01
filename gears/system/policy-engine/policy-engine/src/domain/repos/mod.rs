//! Repository ports of the content store.
//!
//! The domain codes against these traits; `infra::storage` implements them
//! over the secure data layer. Every method takes the database runner the
//! caller holds (`DbConn` or `DbTx`), so the management service composes one
//! transaction out of several repositories.
//!
//! # Scoping
//!
//! Management methods take the caller's [`AccessScope`] and never widen it.
//! Content outside the scope is reported exactly like content that does not
//! exist ([`RepoError::NotFound`] or an empty result), so forbidden and absent
//! are indistinguishable above this layer.
//!
//! [`ActiveContentLoader::load_for_tenants`] runs under the gear's own
//! **service scope** instead, because the decision path has no scoped caller;
//! it reads active content only.
//!
//! [`AccessScope`]: toolkit_security::AccessScope

// Repository ports are generic over the sealed `toolkit_db` runner, as the
// platform's reference gear does; no `sea_orm` type crosses this boundary.
#![allow(unknown_lints, de0301_no_infra_in_domain)]

use toolkit_macros::domain_model;

pub mod content;

pub use content::{
    ActivationOutcome, ActiveContentLoader, AssignmentRepository, BundleRepository,
    VersionRepository,
};

/// Reasons carried by [`RepoError::Conflict`]. Where the management surface
/// has the same condition, the string equals its SDK reason code.
pub mod conflict {
    /// The version is active or superseded and therefore immutable.
    pub const VERSION_NOT_DRAFT: &str = "VERSION_NOT_DRAFT";
    /// A bundle with this name already exists in the owning tenant.
    pub const BUNDLE_NAME_TAKEN: &str = "BUNDLE_NAME_TAKEN";
    /// The bundle is already assigned to this tenant.
    pub const ASSIGNMENT_EXISTS: &str = "ASSIGNMENT_EXISTS";
    /// The bundle already has a draft (the one-draft index refused another).
    pub const DRAFT_EXISTS: &str = "DRAFT_EXISTS";
    /// Two documents of the version share a name.
    pub const DUPLICATE_DOCUMENT_NAME: &str = "DUPLICATE_DOCUMENT_NAME";
    /// Another activation of the same bundle committed first; the one-active
    /// index refused this one. Re-read the bundle's versions and retry.
    pub const CONCURRENT_ACTIVATION: &str = "CONCURRENT_ACTIVATION";
}

/// Failure of a repository call. Carries no storage type.
#[domain_model]
#[derive(Debug, Clone, thiserror::Error)]
pub enum RepoError {
    /// The target does not exist **or** is outside the caller's scope.
    #[error("not found")]
    NotFound,
    /// The change contradicts the target's current state; `reason` is one of
    /// the [`conflict`] codes.
    #[error("conflict: {reason}")]
    Conflict {
        /// Stable reason code from [`conflict`].
        reason: &'static str,
    },
    /// The listing query (order, cursor, limit) was refused.
    #[error("invalid query: {0}")]
    Query(toolkit_odata::Error),
    /// Storage failure or a stored row that does not decode; never a caller
    /// error.
    #[error("database error: {0}")]
    Database(String),
}

impl RepoError {
    /// Shorthand for [`RepoError::Conflict`].
    #[must_use]
    pub const fn conflict(reason: &'static str) -> Self {
        Self::Conflict { reason }
    }
}

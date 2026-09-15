//! Security primitives shared by every `ToolKit` gear: who a caller is, what
//! they are allowed to reach, and how a service proves its own identity to
//! another.
//!
//! A leaf crate by design — it depends on no other `ToolKit` library, so
//! anything from a bootstrap path to a gear's domain layer can use it without
//! pulling in the runtime.
//!
//! # The two planes
//!
//! `ToolKit` authenticates on two independent planes, and this crate carries
//! the types for both (`cpt-cf-adr-two-plane-auth`):
//!
//! - **Tenant plane** — a user's request. [`SecurityContext`] is the result:
//!   the subject, its tenant, and the token's capability scopes. Produced by a
//!   [`BearerAuthenticator`] from an `Authorization: Bearer` JWT.
//! - **Platform plane** — one service calling another. [`PlatformIdentity`] is
//!   the result, produced by an [`InternalAuthenticator`] from the
//!   [`constants::INTERNAL_TOKEN_HEADER`] credential. Never `Authorization`,
//!   so the two planes cannot be confused for one another.
//!
//! # Authorization
//!
//! [`AccessScope`] is what a policy decision compiles down to: a disjunction of
//! [`ScopeConstraint`]s, each a conjunction of [`ScopeFilter`]s over properties
//! like `owner_tenant_id`. `toolkit-db` turns it into a SQL `WHERE` clause so
//! row-level authorization is enforced by the query rather than by a check a
//! caller has to remember.
//!
//! Two shapes carry meaning and are easy to misread: an *unconstrained* scope
//! permits everything, and a *deny-all* scope permits nothing. The
//! `contains_*` accessors report on the constraint list alone, so both answer
//! "no" — see [`AccessScope::allows_uuid`] for the predicate that accounts for
//! the difference.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

/// What a caller is authorized to reach, and how it compiles to a query filter.
pub mod access_scope;
/// Traits for validating a credential on either plane, and object-safe wrappers.
pub mod authenticator;
/// Binary wire format for a [`SecurityContext`] over gRPC metadata.
pub mod bin_codec;
/// Well-known identifiers and header names.
pub mod constants;
/// The authenticated tenant-plane caller.
pub mod context;
/// Platform-plane identity, credentials, and the authenticator contract.
pub mod internal_auth;
/// TTL-bounded caching for platform-plane validation.
#[cfg(feature = "internal-auth-cache")]
pub mod internal_auth_cache;
/// Configuration selecting how the platform plane authenticates.
pub mod internal_auth_config;
/// The types most gears need, re-exported for a single glob import.
pub mod prelude;
/// A pre-shared-secret [`InternalAuthenticator`], for development and simple
/// deployments.
pub mod shared_secret;

pub use access_scope::{
    AccessScope, EmptyScopeConstraint, EqScopeFilter, InGroupScopeFilter,
    InGroupSubtreeScopeFilter, InScopeFilter, InTenantSubtreeScopeFilter, ScopeConstraint,
    ScopeFilter, ScopeValue, pep_properties,
};
pub use authenticator::{
    AuthNError, BearerAuthenticator, DynBearerAuthenticator, DynInternalAuthenticator,
};
pub use context::{SecurityContext, SecurityContextBuildError};
pub use internal_auth::{
    InternalAuthNError, InternalAuthenticator, InternalCredential, PeerAuthenticated,
    PlatformIdentity, PlatformSecurityContext,
};
#[cfg(feature = "internal-auth-cache")]
pub use internal_auth_cache::{
    CachingInternalAuthenticator, DEFAULT_TOKEN_REVIEW_CACHE_TTL, InvalidCacheTtl,
    MAX_TOKEN_REVIEW_CACHE_TTL,
};
pub use internal_auth_config::{
    BuiltAuthenticator, DEFAULT_INTERNAL_PEER_NAME, InternalAuthConfig,
};
pub use shared_secret::{
    InvalidSharedSecret, REDACTED_PLACEHOLDER, SharedSecretInternalAuthenticator,
};

pub use bin_codec::{
    SECCTX_BIN_VERSION, SecCtxDecodeError, SecCtxEncodeError, decode_bin, encode_bin,
};

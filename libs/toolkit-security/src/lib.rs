#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
pub mod access_scope;
pub mod authenticator;
pub mod bin_codec;
pub mod constants;
pub mod context;
pub mod internal_auth;
pub mod prelude;

pub use access_scope::{
    AccessScope, EqScopeFilter, InGroupScopeFilter, InGroupSubtreeScopeFilter, InScopeFilter,
    InTenantSubtreeScopeFilter, ScopeConstraint, ScopeFilter, ScopeValue, pep_properties,
    rg_tables, tenant_tables,
};
pub use authenticator::{AuthNError, BearerAuthenticator};
pub use context::{SecurityContext, SecurityContextBuildError};
pub use internal_auth::{
    InternalAuthNError, InternalAuthenticator, InternalCredential, PeerAuthenticated,
    PlatformIdentity, PlatformSecurityContext,
};

pub use bin_codec::{
    SECCTX_BIN_VERSION, SecCtxDecodeError, SecCtxEncodeError, decode_bin, encode_bin,
};

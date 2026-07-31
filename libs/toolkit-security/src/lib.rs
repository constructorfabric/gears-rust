#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
pub mod access_scope;
pub mod authenticator;
pub mod bin_codec;
pub mod constants;
pub mod context;
pub mod internal_auth;
pub mod internal_auth_config;
pub mod prelude;
pub mod shared_secret;

pub use access_scope::{
    AccessScope, EqScopeFilter, InGroupScopeFilter, InGroupSubtreeScopeFilter, InScopeFilter,
    InTenantSubtreeScopeFilter, ScopeConstraint, ScopeFilter, ScopeValue, pep_properties,
    rg_tables, tenant_tables,
};
pub use authenticator::{
    AuthNError, BearerAuthenticator, DynBearerAuthenticator, DynInternalAuthenticator,
};
pub use context::{SecurityContext, SecurityContextBuildError};
pub use internal_auth::{
    InternalAuthNError, InternalAuthenticator, InternalCredential, PeerAuthenticated,
    PlatformIdentity, PlatformSecurityContext,
};
pub use internal_auth_config::{DEFAULT_INTERNAL_PEER_NAME, InternalAuthConfig};
pub use shared_secret::SharedSecretInternalAuthenticator;

pub use bin_codec::{
    SECCTX_BIN_VERSION, SecCtxDecodeError, SecCtxEncodeError, decode_bin, encode_bin,
};

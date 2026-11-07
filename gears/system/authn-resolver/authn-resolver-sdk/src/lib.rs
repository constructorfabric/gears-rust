//! `AuthN` Resolver SDK
//!
//! This crate provides the public API for the `authn_resolver` gear:
//!
//! - [`AuthNResolverClient`] - Public API trait for consumers
//! - [`AuthNResolverPluginClient`] - Plugin API trait for implementations
//! - [`AuthenticationRequest`] - Bearer token and plugin-cache control for
//!   [`AuthNResolverClient::authenticate`]
//! - [`AuthenticationResult`] - Authentication result model
//! - [`ClientCredentialsProvider`] - S2S `SecurityContext` provider with caching and on-demand refresh,
//!   created with [`ClientCredentialsProvider::builder`]
//! - [`CachingAuthenticator`] - Opt-in inbound token validation with bounded,
//!   producer-authorized result reuse, created with [`CachingAuthenticator::builder`]
//! - [`ScopeSet`] - Validated, canonical `OAuth2` scope set
//! - [`AuthNResolverError`], [`ClientCredentialsProviderError`] - Error types
//! - [`AuthNResolverPluginSpecV1`] - GTS schema for plugin discovery
//!
//! ## Usage
//!
//! Consumers obtain the client from `ClientHub`
//! (`hub.get::<dyn AuthNResolverClient>()?`) and pass the raw bearer token,
//! without the `Bearer ` prefix:
//!
//! ```no_run
//! # use std::sync::Arc;
//! use authn_resolver_sdk::{AuthNResolverClient, AuthenticationRequest};
//!
//! # async fn example(authn: Arc<dyn AuthNResolverClient>) -> Result<(), Box<dyn std::error::Error>> {
//! // `new` asks for a fresh validation, never a plugin-cached result.
//! let request = AuthenticationRequest::new("eyJhbGciOiJSUzI1NiJ9...");
//! let result = authn.authenticate(&request).await?;
//! let security_context = result.security_context;
//! # let _ = security_context;
//! # Ok(())
//! # }
//! ```
//!
//! ## Cached S2S credentials
//!
//! A [`ClientCredentialsProvider`] exchanges a gear's client credentials once and
//! reuses the result until shortly before it expires:
//!
//! ```no_run
//! # use std::sync::Arc;
//! use authn_resolver_sdk::{
//!     AuthNResolverClient, ClientCredentialsProvider, ClientCredentialsProviderError, ClientCredentialsProviderConfig, ScopeSet,
//! };
//! use secrecy::SecretString;
//!
//! # async fn example(
//! #     authn: Arc<dyn AuthNResolverClient>,
//! #     client_secret: SecretString,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! let provider = ClientCredentialsProvider::builder(authn, "my-service", client_secret)
//!     .with_default_scopes(ScopeSet::new(["usage:write"])?) // optional: IdP defaults otherwise
//!     .with_config(ClientCredentialsProviderConfig::default()) // optional
//!     .build()?;
//!
//! match provider.get().await {
//!     Ok(result) => { /* call downstream with `result.security_context` */ }
//!     // Inspect the resolver error to decide whether to retry or fix the credentials.
//!     Err(ClientCredentialsProviderError::Resolver(source)) => return Err(source.into()),
//!     // Shut down, or no scope-set slot was free.
//!     Err(other) => return Err(other.into()),
//! }
//!
//! provider.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! The repository's `examples/cached_client_credentials.rs` runs the full
//! workflow, including invalidation after a downstream rejection, against an
//! in-process resolver.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod authenticator;
pub mod bearer;
pub mod client_credentials;
pub mod error;
pub mod gts;
mod local_cache;
pub mod models;
pub mod plugin_api;
pub mod scopes;

// Re-export main types at crate root
pub use api::AuthNResolverClient;
pub use authenticator::{
    CachingAuthenticator, CachingAuthenticatorBuilder, CachingAuthenticatorConfig,
    CachingAuthenticatorConfigError, CachingAuthenticatorError,
};
pub use bearer::AuthNResolverBearerAuthenticator;
pub use client_credentials::{
    ClientCredentialsProvider, ClientCredentialsProviderBuilder, ClientCredentialsProviderConfig,
    ClientCredentialsProviderConfigError, ClientCredentialsProviderError,
};
pub use error::AuthNResolverError;
pub use gts::AuthNResolverPluginSpecV1;
pub use models::{
    AuthenticationRequest, AuthenticationResult, CacheControl, CacheTag, ClientCredentialsRequest,
    RequestCacheControl,
};
pub use plugin_api::AuthNResolverPluginClient;
pub use scopes::{ScopeSet, ScopeSetError};

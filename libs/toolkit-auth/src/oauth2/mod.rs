//! Outbound `OAuth2` client credentials flow.
//!
//! This gear implements token acquisition, caching, and automatic injection
//! for outbound HTTP requests to vendor services secured with `OAuth2`.

pub mod auto_refresh;
pub mod builder_ext;
pub mod config;
/// OIDC discovery: resolves the token endpoint from an issuer URL.
pub(crate) mod discovery;
pub mod error;
pub mod fetch;
pub mod layer;
/// Token source implementations that perform the actual token requests.
pub(crate) mod source;
pub mod token;
pub(crate) mod token_watcher;
pub mod types;

pub use auto_refresh::{
    BearerAuthAutoRefreshLayer, BearerAuthAutoRefreshOpts, BearerAuthAutoRefreshService,
    DEFAULT_MIN_INVALIDATION_INTERVAL, ShouldRefreshFn,
};
pub use builder_ext::HttpClientBuilderExt;
pub use config::OAuthClientConfig;
pub use error::TokenError;
pub use fetch::{FetchedToken, fetch_token};
pub use layer::BearerAuthLayer;
pub use token::Token;
pub use types::{ClientAuthMethod, SecretString};

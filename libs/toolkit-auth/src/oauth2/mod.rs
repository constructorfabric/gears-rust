//! Outbound `OAuth2` client credentials flow.
//!
//! This gear implements token acquisition, caching, and automatic injection
//! for outbound HTTP requests to vendor services secured with `OAuth2`.

pub mod authcode;
pub mod auto_refresh;
pub mod builder_ext;
pub mod config;
pub(crate) mod discovery;
pub mod error;
pub mod fetch;
pub mod layer;
pub(crate) mod source;
pub mod token;
pub(crate) mod token_watcher;
pub mod types;

pub use authcode::{
    AuthCodeExchange, AuthorizationServerMetadata, Pkce, ProtectedResourceMetadata,
    RegisteredClient, TokenSet, build_authorize_url, discover_authorization_server,
    discover_from_resource, discover_from_resource_challenge, discover_protected_resource,
    exchange_code, generate_state, refresh_token, register_client,
};
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

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![warn(warnings)]

// Core gears
pub mod errors;
pub mod http_error;
pub mod traits;

// JWT / JWKS infrastructure
pub mod claims_error;
pub mod config;
pub mod metrics;
pub mod providers;
pub mod standard_claims;
pub mod validation;

// Outbound OAuth2 client credentials
pub mod oauth2;

// Core exports
pub use errors::AuthError;
pub use traits::{KeyProvider, TokenValidator};

// JWT / JWKS exports
pub use claims_error::ClaimsError;
pub use config::{AuthConfig, JwksConfig};
pub use metrics::{AuthEvent, AuthMetricLabels, AuthMetrics, LoggingMetrics, NoOpMetrics};
pub use providers::JwksKeyProvider;
pub use standard_claims::StandardClaim;
pub use validation::{ValidationConfig, validate_claims};

// Outbound OAuth2 exports.
//
// `BearerAuthAutoRefreshService`, `ShouldRefreshFn`, and
// `DEFAULT_MIN_INVALIDATION_INTERVAL` are intentionally accessible only via
// the `oauth2` namespace - they are wiring details rather than entry-point
// API.
pub use oauth2::{
    BearerAuthAutoRefreshLayer, BearerAuthAutoRefreshOpts, BearerAuthLayer, ClientAuthMethod,
    FetchedToken, HttpClientBuilderExt, HttpOperation, OAuthClientConfig, SecretString, TokenError,
    TokenProvider, fetch_token,
};

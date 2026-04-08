use crate::{claims_error::ClaimsError, errors::AuthError};
use async_trait::async_trait;
use jsonwebtoken::Header;
use serde_json::Value;

/// Validates and parses JWT tokens
#[async_trait]
pub trait TokenValidator: Send + Sync {
    /// Validate a JWT token and return normalized claims as JSON
    async fn validate_and_parse(&self, token: &str) -> Result<Value, AuthError>;
}

/// Provider that can validate JWT signatures and decode tokens
#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// Returns the name of this provider (for debugging/logging)
    fn name(&self) -> &str;

    /// Attempt to validate the JWT signature and decode its header and claims
    ///
    /// Returns the JWT header and raw claims as JSON if validation succeeds.
    /// Returns an error if the signature is invalid or decoding fails.
    ///
    /// This method should:
    /// - Decode the JWT header
    /// - Find the appropriate key (e.g., by kid)
    /// - Validate the signature
    /// - Return raw claims for further processing
    async fn validate_and_decode(&self, token: &str) -> Result<(Header, Value), ClaimsError>;

    /// Optional: refresh keys if this provider supports it (e.g., JWKS)
    async fn refresh_keys(&self) -> Result<(), ClaimsError> {
        Ok(())
    }
}

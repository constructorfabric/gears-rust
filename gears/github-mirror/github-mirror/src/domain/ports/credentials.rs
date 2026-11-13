//! The port a sync takes its GitHub client from when its request names a
//! credential (gears-rust#5970).

use std::sync::Arc;

use async_trait::async_trait;
use github_mirror_sdk::CredentialRef;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::ports::github::GithubPort;

/// GitHub clients keyed by the credential they read with.
///
/// Every call reads the secret again, so a rotated token is picked up by the
/// next sync. Two references that hold the same token share one client, and
/// with it one rate-limit admission controller (ADR-0003): GitHub budgets
/// per token, not per reference.
#[async_trait]
pub trait GithubCredentials: Send + Sync {
    /// The client that reads GitHub with the secret `reference` names, read
    /// from the credential store as `ctx`.
    ///
    /// # Errors
    /// `Validation` when the key is malformed or the store holds no secret
    /// the caller can read under it, or when the secret is empty or not
    /// UTF-8; `Forbidden` when the store refuses the caller; `Unavailable`
    /// when the store cannot be asked. No error carries the secret.
    async fn client_for(
        &self,
        ctx: &SecurityContext,
        reference: &CredentialRef,
    ) -> Result<Arc<dyn GithubPort>, DomainError>;
}

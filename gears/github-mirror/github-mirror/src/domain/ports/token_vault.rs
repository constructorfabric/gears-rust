use async_trait::async_trait;
use toolkit_security::SecurityContext;
use toolkit_utils::SecretString;

use crate::domain::error::DomainError;

#[must_use]
pub fn vault_secret_name(fingerprint: &str) -> String {
    format!("github-mirror-{fingerprint}")
}

#[async_trait]
pub trait TokenVault: Send + Sync {
    async fn save(
        &self,
        ctx: &SecurityContext,
        token: &SecretString,
    ) -> Result<String, DomainError>;

    async fn delete(&self, ctx: &SecurityContext, fingerprint: &str) -> Result<(), DomainError>;
}

pub struct NoTokenVault;

#[async_trait]
impl TokenVault for NoTokenVault {
    async fn save(
        &self,
        _ctx: &SecurityContext,
        _token: &SecretString,
    ) -> Result<String, DomainError> {
        Err(DomainError::internal(
            "no credential store to keep GitHub tokens in",
        ))
    }

    async fn delete(&self, _ctx: &SecurityContext, _fingerprint: &str) -> Result<(), DomainError> {
        Err(DomainError::internal(
            "no credential store to keep GitHub tokens in",
        ))
    }
}

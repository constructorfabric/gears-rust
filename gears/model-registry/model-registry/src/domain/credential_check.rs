//! Credential binding (DESIGN §4 Credential Binding, ADR-0002): a `secret_ref`
//! is accepted only when the writing tenant's own `CredStore` row holds a
//! secret. The read is metadata only and runs before the write transaction.

use async_trait::async_trait;
use model_registry_sdk::field;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use super::error::{DomainError, Violation};

/// What `CredStore` reports about the caller's own row under a reference.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnRowState {
    /// No record is visible under the reference.
    Missing,
    /// A record resolves, but the caller's tenant holds no row under it.
    NotOwned,
    /// The caller's own row exists without a value.
    Declared,
    /// The caller's own row holds a value.
    Active,
    /// The caller's own row holds a value past its expiry.
    Expired,
}

/// Where the own-row state comes from (`CredStore` in production, a map in
/// tests).
#[async_trait]
pub trait SecretRefSource: Send + Sync {
    async fn own_row_state(
        &self,
        ctx: &SecurityContext,
        secret_ref: &str,
    ) -> Result<OwnRowState, DomainError>;
}

/// The ownership rule: `Active` and `Expired` own rows are accepted;
/// `Missing` is `SECRET_REF_NOT_FOUND`; `NotOwned` and `Declared` are
/// `SECRET_REF_NOT_OWNED`.
pub async fn check_secret_ref(
    source: &dyn SecretRefSource,
    ctx: &SecurityContext,
    secret_ref: &str,
) -> Result<(), DomainError> {
    match source.own_row_state(ctx, secret_ref).await? {
        OwnRowState::Active | OwnRowState::Expired => Ok(()),
        OwnRowState::Missing => Err(DomainError::invalid(Violation::new(
            "secret_ref",
            format!("no credential record under `{secret_ref}`"),
            field::Reason::SecretRefNotFound,
        ))),
        OwnRowState::NotOwned | OwnRowState::Declared => Err(DomainError::invalid(Violation::new(
            "secret_ref",
            format!("the tenant holds no secret under `{secret_ref}`"),
            field::Reason::SecretRefNotOwned,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{StaticSecretRefSource, security_context};

    fn reason(err: DomainError) -> field::Reason {
        match err {
            DomainError::Invalid { violations: v, .. } => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].field, "secret_ref");
                v[0].reason
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ownership_matrix() {
        let source = StaticSecretRefSource::new()
            .with_ref("active", OwnRowState::Active)
            .with_ref("expired", OwnRowState::Expired)
            .with_ref("declared", OwnRowState::Declared)
            .with_ref("inherited", OwnRowState::NotOwned);
        let ctx = security_context(uuid::Uuid::new_v4());
        check_secret_ref(&source, &ctx, "active").await.unwrap();
        check_secret_ref(&source, &ctx, "expired").await.unwrap();
        assert_eq!(
            reason(
                check_secret_ref(&source, &ctx, "declared")
                    .await
                    .unwrap_err()
            ),
            field::Reason::SecretRefNotOwned
        );
        assert_eq!(
            reason(
                check_secret_ref(&source, &ctx, "inherited")
                    .await
                    .unwrap_err()
            ),
            field::Reason::SecretRefNotOwned
        );
        assert_eq!(
            reason(
                check_secret_ref(&source, &ctx, "unknown")
                    .await
                    .unwrap_err()
            ),
            field::Reason::SecretRefNotFound
        );
        assert_eq!(source.calls(), 5);
    }

    #[tokio::test]
    async fn source_errors_pass_through() {
        let source = StaticSecretRefSource::new().failing_with(|| DomainError::not_permitted("x"));
        let ctx = security_context(uuid::Uuid::new_v4());
        assert!(matches!(
            check_secret_ref(&source, &ctx, "any").await.unwrap_err(),
            DomainError::NotPermitted { .. }
        ));
    }
}

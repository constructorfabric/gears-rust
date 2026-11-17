//! [`SecretRefSource`] over the `CredStore` v2 client (DESIGN §3.5 Credential
//! Ownership), bounded by `credstore_deadline_ms`.
//!
//! The client is looked up per call: the v2 gear is not registered in the
//! server until the credstore v2 switch (`gears/credstore/v2/plan.md`), and
//! until then every write that binds a new `secret_ref` fails closed as
//! dependency unavailable.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use credstore_v2_sdk::{CredStoreClientV2, Credential, CredentialStatus, SecretRef};
use model_registry_sdk::field;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::domain::credential_check::{OwnRowState, SecretRefSource};
use crate::domain::error::{Dependency, DomainError, Violation};

/// Operation name reported when `CredStore` denies the record read.
pub const SECRET_REF_READ_OPERATION: &str = "credstore.get_record";

pub struct CredStoreSecretRefSource {
    hub: Arc<ClientHub>,
    deadline: Duration,
}

impl CredStoreSecretRefSource {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, deadline: Duration) -> Self {
        Self { hub, deadline }
    }
}

/// The own-row state of a resolved record (ADR-0002).
fn own_state(status: Option<CredentialStatus>) -> OwnRowState {
    match status {
        None => OwnRowState::NotOwned,
        Some(CredentialStatus::Declared) => OwnRowState::Declared,
        Some(CredentialStatus::Active) => OwnRowState::Active,
        Some(CredentialStatus::Expired) => OwnRowState::Expired,
    }
}

/// `CredStore` outcome mapping of DESIGN §4 Error Handling: no record is
/// `Missing`, a denied read is `NOT_PERMITTED`, everything else is dependency
/// unavailable.
fn map_result(
    result: Result<Option<Credential>, CanonicalError>,
    secret_ref: &str,
) -> Result<OwnRowState, DomainError> {
    match result {
        Ok(None) => Ok(OwnRowState::Missing),
        Ok(Some(credential)) => Ok(own_state(credential.own.map(|own| own.status))),
        Err(canonical @ CanonicalError::PermissionDenied { .. }) => {
            tracing::warn!(
                secret_ref,
                error = %canonical,
                "credstore refused the record read: the caller lacks the CredStore read grant"
            );
            Err(DomainError::not_permitted(SECRET_REF_READ_OPERATION))
        }
        Err(other) => Err(DomainError::unavailable(Dependency::CredStore, other)),
    }
}

#[async_trait]
impl SecretRefSource for CredStoreSecretRefSource {
    async fn own_row_state(
        &self,
        ctx: &SecurityContext,
        secret_ref: &str,
    ) -> Result<OwnRowState, DomainError> {
        let Some(client) = self.hub.try_get::<dyn CredStoreClientV2>() else {
            return Err(DomainError::unavailable(
                Dependency::CredStore,
                "CredStoreClientV2 is not registered",
            ));
        };
        let key = SecretRef::new(secret_ref).map_err(|e| {
            DomainError::invalid(Violation::new(
                "secret_ref",
                e.detail().to_owned(),
                field::Reason::InvalidFormat,
            ))
        })?;
        let call = client.get_record(ctx, &key);
        let result = match tokio::time::timeout(self.deadline, call).await {
            Ok(result) => result,
            Err(_elapsed) => {
                return Err(DomainError::unavailable(
                    Dependency::CredStore,
                    format!("get_record did not answer within {:?}", self.deadline),
                ));
            }
        };
        map_result(result, secret_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_row_status_maps_to_state() {
        assert_eq!(own_state(None), OwnRowState::NotOwned);
        assert_eq!(
            own_state(Some(CredentialStatus::Declared)),
            OwnRowState::Declared
        );
        assert_eq!(
            own_state(Some(CredentialStatus::Active)),
            OwnRowState::Active
        );
        assert_eq!(
            own_state(Some(CredentialStatus::Expired)),
            OwnRowState::Expired
        );
    }

    #[test]
    fn missing_denied_and_failed_reads() {
        assert_eq!(map_result(Ok(None), "k").unwrap(), OwnRowState::Missing);

        let denied = crate::domain::error::ModelScope::permission_denied()
            .with_reason("NO_READ_GRANT")
            .create();
        assert!(matches!(
            map_result(Err(denied), "k"),
            Err(DomainError::NotPermitted {
                operation: SECRET_REF_READ_OPERATION,
                ..
            })
        ));

        let down = CanonicalError::service_unavailable().create();
        assert!(matches!(
            map_result(Err(down), "k"),
            Err(DomainError::DependencyUnavailable {
                dependency: Dependency::CredStore,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn unregistered_client_is_unavailable() {
        let source =
            CredStoreSecretRefSource::new(Arc::new(ClientHub::new()), Duration::from_millis(10));
        let ctx = crate::testing::security_context(uuid::Uuid::new_v4());
        assert!(matches!(
            source.own_row_state(&ctx, "k").await,
            Err(DomainError::DependencyUnavailable {
                dependency: Dependency::CredStore,
                ..
            })
        ));
    }
}

//! Which GitHub credential a sync reads with, and the repository's binding
//! to it (gears-rust#5970).
//!
//! A sync request may name a secret in the credential store. The secret is
//! read as the caller when the sync is requested, so the credential store's
//! own rules decide whether the caller may use it, and the sync reads GitHub
//! with that token only: a token that cannot read the repository fails the
//! sync, and no other token is tried.
//!
//! A sync whose Discovery read the repository binds it to the credential it
//! used (`gm_repo_sync_status.credential_key`). A later sync that names no
//! credential, a resume included, keeps using the bound one, so the gear-wide
//! token never reads a repository a tenant bound to its own credential. A
//! repository that was never bound syncs with the gear-wide token, as before.

use std::sync::Arc;

use github_mirror_sdk::CredentialRef;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};

use super::{EnqueueScopes, Service, actions};
use crate::domain::error::DomainError;
use crate::domain::ports::github::GithubPort;

/// The credential one sync reads GitHub with.
///
/// The default is the gear-wide token: no reference, and the service's own
/// client.
#[domain_model]
#[derive(Clone, Default)]
pub struct SyncCredential {
    /// The secret the sync was asked to use; `None` for the gear-wide token.
    pub reference: Option<CredentialRef>,
    /// The client that reads with it; `None` for the service's own client.
    pub(crate) github: Option<Arc<dyn GithubPort>>,
}

/// Only the reference is shown: the client holds the token.
impl std::fmt::Debug for SyncCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncCredential")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl SyncCredential {
    /// The key the repository is bound to once this credential has read it;
    /// `None` for the gear-wide token.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.reference.as_ref().map(|r| r.key.as_str())
    }
}

/// The reference a request names, checked before the store is asked.
///
/// # Errors
/// `Validation` when it names another tenant than the caller's.
fn own_reference(
    ctx: &SecurityContext,
    reference: CredentialRef,
) -> Result<CredentialRef, DomainError> {
    if reference.tenant_id != ctx.subject_tenant_id() {
        return Err(DomainError::Validation {
            field: "credential_tenant_id".to_owned(),
            message: "a sync may name only a credential of the caller's own tenant".to_owned(),
        });
    }
    Ok(reference)
}

impl Service {
    /// Hand the service the clients a sync takes when its request names a
    /// credential. Without them such a request is refused.
    #[must_use]
    pub fn with_credentials(
        mut self,
        credentials: Arc<dyn crate::domain::ports::credentials::GithubCredentials>,
    ) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// The credential a sync of `repo_full_name` reads with: the one the
    /// request names, else the one the repository is bound to, else the
    /// gear-wide token. A named or bound credential is read from the store
    /// now, once for this sync.
    ///
    /// # Errors
    /// `Validation` when the reference names another tenant or a secret the
    /// caller cannot read, or when this gear has no credential store;
    /// `Forbidden`, `Unavailable` and `Database` as the store and the
    /// run-status table answer.
    pub(super) async fn sync_credential(
        &self,
        ctx: &SecurityContext,
        scopes: &EnqueueScopes,
        repo_full_name: &str,
        requested: Option<CredentialRef>,
    ) -> Result<SyncCredential, DomainError> {
        let reference = match requested {
            Some(reference) => Some(own_reference(ctx, reference)?),
            None => self
                .bound_credential(&scopes.repo_status, repo_full_name)
                .await?
                .map(|key| CredentialRef {
                    tenant_id: ctx.subject_tenant_id(),
                    key,
                }),
        };
        let Some(reference) = reference else {
            return Ok(SyncCredential::default());
        };
        let Some(credentials) = self.credentials.as_ref() else {
            return Err(DomainError::Validation {
                field: "credential_key".to_owned(),
                message:
                    "this github-mirror has no credential store to read a named credential from"
                        .to_owned(),
            });
        };
        let github = credentials.client_for(ctx, &reference).await?;
        Ok(SyncCredential {
            reference: Some(reference),
            github: Some(github),
        })
    }

    /// The credential key the repository is bound to in the caller's tenant.
    async fn bound_credential(
        &self,
        scope: &AccessScope,
        repo_full_name: &str,
    ) -> Result<Option<String>, DomainError> {
        Ok(self
            .repo_sync_status
            .find(scope, repo_full_name)
            .await?
            .and_then(|status| status.credential_key))
    }

    /// The client a sync reads GitHub with.
    pub(super) fn github_for(&self, credential: Option<&SyncCredential>) -> Arc<dyn GithubPort> {
        credential
            .and_then(|credential| credential.github.clone())
            .unwrap_or_else(|| Arc::clone(&self.github))
    }

    /// Record that `credential` read the repository, so later syncs keep
    /// using it. A failure is logged, not returned: the sync itself went
    /// through, and the next one records the binding again.
    pub(super) async fn bind_credential(
        &self,
        ctx: &SecurityContext,
        repo_full_name: &str,
        credential: &SyncCredential,
    ) {
        let bound = async {
            let scope = self.repo_status_scope(ctx, actions::UPSERT).await?;
            self.repo_sync_status
                .bind_credential(&scope, repo_full_name, credential.key())
                .await
        }
        .await;
        if let Err(e) = bound {
            report_binding_not_written(repo_full_name, &e);
        }
    }
}

fn report_binding_not_written(repo_full_name: &str, error: &DomainError) {
    tracing::warn!(
        repository = repo_full_name,
        error = %error.public_text(),
        "could not record which credential synced this repository; the next sync records it"
    );
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn caller(tenant: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .build()
            .unwrap()
    }

    #[test]
    fn a_reference_to_another_tenant_is_refused() {
        let reference = CredentialRef {
            tenant_id: Uuid::new_v4(),
            key: "github".to_owned(),
        };

        let err = own_reference(&caller(Uuid::new_v4()), reference).unwrap_err();

        assert!(
            matches!(err, DomainError::Validation { ref field, .. } if field == "credential_tenant_id"),
            "{err:?}"
        );
    }

    #[test]
    fn a_reference_to_the_callers_tenant_passes() {
        let tenant = Uuid::new_v4();
        let reference = CredentialRef {
            tenant_id: tenant,
            key: "github".to_owned(),
        };

        assert_eq!(
            own_reference(&caller(tenant), reference.clone()).unwrap(),
            reference
        );
    }

    #[test]
    fn debug_shows_the_reference_and_not_the_client() {
        let credential = SyncCredential {
            reference: Some(CredentialRef {
                tenant_id: Uuid::nil(),
                key: "github".to_owned(),
            }),
            github: None,
        };

        let shown = format!("{credential:?}");

        assert!(shown.contains("github"), "{shown}");
        assert!(!shown.contains("github:"), "{shown}");
    }
}

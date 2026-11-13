//! [`CredentialClients`]: the GitHub client of each credential a sync names
//! (gears-rust#5970).
//!
//! The secret is read from the credential store on every call, as the
//! caller, so a rotated token reaches the next sync and the store's own
//! rules decide who may use it. Clients are kept per token, in a bounded
//! map, so one token keeps one HTTP connection pool and one rate-limit
//! admission controller (ADR-0003) however many syncs use it; the map
//! holds the token's SHA-256, never the token.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, CredStoreError, SecretRef};
use github_mirror_sdk::CredentialRef;
use tokio::sync::Semaphore;
use toolkit_security::SecurityContext;
use toolkit_utils::SecretString;

use crate::domain::error::DomainError;
use crate::domain::ports::credentials::GithubCredentials;
use crate::domain::ports::github::GithubPort;
use crate::infra::github::cache::HttpCache;
use crate::infra::github::client::GithubClient;
use crate::infra::github::rate_limit::TokenFingerprint;

/// How each credential's client is built: the settings the gear-wide
/// client is built with.
pub struct ClientSettings {
    pub api_base_url: String,
    pub cache: Arc<dyn HttpCache>,
    /// The request permits every client shares.
    pub permits: Arc<Semaphore>,
    /// How many permits `permits` holds.
    pub max_concurrent_requests: NonZeroUsize,
}

struct Kept {
    client: Arc<GithubClient>,
    used: Instant,
}

/// One GitHub client per token, at most `capacity` of them.
///
/// When a new token would pass the bound, the client used longest ago is
/// dropped from the map. A sync still holding it keeps it until the sync
/// ends; the next sync with that token starts a fresh client, whose
/// controller learns the token's budget from GitHub's first answer.
pub struct CredentialClients {
    credstore: Arc<dyn CredStoreClientV1>,
    settings: ClientSettings,
    capacity: NonZeroUsize,
    clients: Mutex<HashMap<TokenFingerprint, Kept>>,
}

impl CredentialClients {
    #[must_use]
    pub fn new(
        credstore: Arc<dyn CredStoreClientV1>,
        settings: ClientSettings,
        capacity: NonZeroUsize,
    ) -> Self {
        Self {
            credstore,
            settings,
            capacity,
            clients: Mutex::new(HashMap::new()),
        }
    }

    /// How many clients the map holds now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the map holds no client.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<TokenFingerprint, Kept>> {
        self.clients.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read the secret `reference` names, as `ctx`.
    async fn token(
        &self,
        ctx: &SecurityContext,
        reference: &CredentialRef,
    ) -> Result<SecretString, DomainError> {
        let key = SecretRef::new(reference.key.clone()).map_err(|e| DomainError::Validation {
            field: "credential_key".to_owned(),
            message: format!("not a credential-store key: {e}"),
        })?;
        let found = self
            .credstore
            .get(ctx, &key)
            .await
            .map_err(|e| store_error(&reference.key, &e))?
            .ok_or_else(|| DomainError::Validation {
                field: "credential_key".to_owned(),
                message: format!(
                    "the credential store holds no secret `{}` the caller can read",
                    reference.key
                ),
            })?;
        let token = std::str::from_utf8(found.value.as_bytes())
            .ok()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| DomainError::Validation {
                field: "credential_key".to_owned(),
                message: format!(
                    "the secret `{}` is empty or not text, so it holds no GitHub token",
                    reference.key
                ),
            })?;
        Ok(SecretString::new(token))
    }

    /// The kept client for `token`, built on first sight.
    fn client(&self, token: SecretString) -> Result<Arc<GithubClient>, DomainError> {
        let fingerprint = TokenFingerprint::of(token.expose());
        let now = Instant::now();
        if let Some(kept) = self.lock().get_mut(&fingerprint) {
            kept.used = now;
            return Ok(Arc::clone(&kept.client));
        }
        // Built outside the lock: building a client sets up its HTTP pool.
        let settings = &self.settings;
        let built = Arc::new(
            GithubClient::with_cache(
                settings.api_base_url.clone(),
                Some(token),
                Arc::clone(&settings.cache),
            )?
            .with_shared_permits(
                Arc::clone(&settings.permits),
                settings.max_concurrent_requests,
            )
            .without_shared_public_cache(),
        );
        let mut clients = self.lock();
        // Another sync may have built one for the same token meanwhile; the
        // first one kept wins, so the token keeps a single controller.
        if let Some(kept) = clients.get_mut(&fingerprint) {
            kept.used = now;
            return Ok(Arc::clone(&kept.client));
        }
        while clients.len() >= self.capacity.get() {
            let Some(oldest) = clients
                .iter()
                .min_by_key(|(_, kept)| kept.used)
                .map(|(fingerprint, _)| fingerprint.clone())
            else {
                break;
            };
            clients.remove(&oldest);
        }
        clients.insert(
            fingerprint,
            Kept {
                client: Arc::clone(&built),
                used: now,
            },
        );
        Ok(built)
    }
}

/// What a failed credential-store read becomes. The store's own text names
/// no secret value, and is redacted on the way out regardless.
fn store_error(key: &str, error: &CredStoreError) -> DomainError {
    match error {
        CredStoreError::AccessDenied => {
            DomainError::Forbidden(format!("the caller may not read the credential `{key}`"))
        }
        CredStoreError::NotFound => DomainError::Validation {
            field: "credential_key".to_owned(),
            message: format!("the credential store holds no secret `{key}` the caller can read"),
        },
        other => DomainError::Unavailable {
            message: format!(
                "the credential store could not be read for `{key}`: {}",
                crate::redact::redacted(&other.to_string())
            ),
            retry_after_secs: None,
        },
    }
}

#[async_trait]
impl GithubCredentials for CredentialClients {
    async fn client_for(
        &self,
        ctx: &SecurityContext,
        reference: &CredentialRef,
    ) -> Result<Arc<dyn GithubPort>, DomainError> {
        let token = self.token(ctx, reference).await?;
        let client: Arc<dyn GithubPort> = self.client(token)?;
        Ok(client)
    }
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod credentials_tests;

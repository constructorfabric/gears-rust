//! Per-user OAuth token storage seam.
//!
//! [`UserTokenStore`] is the single shared contract between the enrollment
//! service (writer) and the `oauth2_auth_code` auth plugin (reader). It owns
//! the storage-key derivation from `(subject, upstream_id)` so the two sides
//! cannot drift on a config string, and it owns the record type
//! ([`OAuthTokenRecord`], defined in [`super::token_record`]).

use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, SecretRef, SecretValue, SharingMode};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::token_record::OAuthTokenRecord;

/// Error surface of the user token store.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TokenStoreError {
    /// The backend (e.g. credstore) failed.
    #[error("token store error: {0}")]
    Backend(String),
    /// A stored record could not be decoded (corrupt or incompatible version).
    #[error("{0}")]
    Corrupt(String),
}

/// Storage seam for per-user OAuth token records.
///
/// The key is derived internally from the calling subject and the upstream, so
/// callers never pass a storage location — there is no config string to keep in
/// sync between the reader and the writer.
#[async_trait]
pub(crate) trait UserTokenStore: Send + Sync {
    /// Load the calling subject's token record for `upstream_id`, if any.
    async fn load(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<Option<OAuthTokenRecord>, TokenStoreError>;

    /// Persist (create or overwrite) the calling subject's token record.
    async fn store(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
        record: &OAuthTokenRecord,
    ) -> Result<(), TokenStoreError>;

    /// Delete the calling subject's token record for `upstream_id`.
    async fn delete(&self, ctx: &SecurityContext, upstream_id: Uuid)
    -> Result<(), TokenStoreError>;
}

/// CredStore-backed [`UserTokenStore`].
///
/// Records are stored with `Private` sharing (scoped to the calling subject)
/// under a key derived from `(subject, upstream_id)`, so no configurable
/// `token_ref` is needed and reader/writer cannot disagree on the location.
pub(crate) struct CredStoreUserTokenStore {
    credstore: Arc<dyn CredStoreClientV1>,
}

impl CredStoreUserTokenStore {
    pub(crate) fn new(credstore: Arc<dyn CredStoreClientV1>) -> Self {
        Self { credstore }
    }

    /// Deterministic, non-configurable storage key. The subject is included in
    /// the ref in addition to `Private` sharing as defense-in-depth against
    /// cross-subject reads.
    fn token_ref(ctx: &SecurityContext, upstream_id: Uuid) -> Result<SecretRef, TokenStoreError> {
        let subject = ctx.subject_id();
        SecretRef::new(format!("oagw-oauth-token-{subject}-{upstream_id}"))
            .map_err(|e| TokenStoreError::Backend(format!("invalid token ref: {e}")))
    }
}

#[async_trait]
impl UserTokenStore for CredStoreUserTokenStore {
    async fn load(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<Option<OAuthTokenRecord>, TokenStoreError> {
        let key = Self::token_ref(ctx, upstream_id)?;
        let resp = self
            .credstore
            .get(ctx, &key)
            .await
            .map_err(|e| TokenStoreError::Backend(format!("credstore get error: {e}")))?;
        let Some(resp) = resp else {
            return Ok(None);
        };
        let record = OAuthTokenRecord::from_slice(resp.value.as_bytes())
            .map_err(TokenStoreError::Corrupt)?;
        Ok(Some(record))
    }

    async fn store(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
        record: &OAuthTokenRecord,
    ) -> Result<(), TokenStoreError> {
        let key = Self::token_ref(ctx, upstream_id)?;
        let json = serde_json::to_vec(record)
            .map_err(|e| TokenStoreError::Backend(format!("serialize token record: {e}")))?;
        self.credstore
            .put(ctx, &key, SecretValue::new(json), SharingMode::Private)
            .await
            .map_err(|e| TokenStoreError::Backend(format!("credstore put error: {e}")))
    }

    async fn delete(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<(), TokenStoreError> {
        let key = Self::token_ref(ctx, upstream_id)?;
        self.credstore
            .delete(ctx, &key)
            .await
            .map_err(|e| TokenStoreError::Backend(format!("credstore delete error: {e}")))
    }
}

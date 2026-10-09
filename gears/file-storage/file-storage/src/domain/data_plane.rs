//! In-process equivalent of the sidecar's byte path: validate, store, hash, finalize
//! and read blobs, delegating finalize to the control plane via `DataPlanePort`.
//!
//! Tenant scoping is not done here: the `(file_id, version_id)` pair was minted by the
//! control plane, which re-checks scope during `finalize`.

use std::sync::Arc;

use bytes::Bytes;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage_sdk::ByteRange;

use crate::domain::error::DomainError;
use crate::domain::ports::DataPlanePort;
use crate::infra::backend::BackendRegistry;
use crate::infra::content::{hash, mime};

/// Moves bytes between callers and the storage backend.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct DataPlaneService {
    control: Arc<dyn DataPlanePort>,
    backends: BackendRegistry,
}

impl DataPlaneService {
    /// Build a service that delegates finalize to `control` and shares its backends.
    #[must_use]
    pub fn new(control: Arc<dyn DataPlanePort>) -> Self {
        let backends = control.backends().clone();
        Self { control, backends }
    }

    /// Validate, store, hash, and finalize an uploaded blob in one step.
    pub async fn put_content(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        declared_mime: &str,
        bytes: Bytes,
    ) -> Result<(), DomainError> {
        mime::validate(declared_mime, &bytes)?;

        // Authorize before touching the backend so a rejected request never
        // overwrites content; `finalize_upload` re-checks.
        self.control.authorize_write(ctx, file_id).await?;

        let version = self
            .control
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;

        let backend = self.backends.get(&version.backend_id)?;
        backend.put(&version.backend_path, bytes.clone()).await?;

        let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let digest = hash::sha256(&bytes);
        self.control
            .finalize_upload(ctx, file_id, version_id, size, digest)
            .await
    }

    /// Read a (range of a) version's content from its backend.
    pub async fn read_content(
        &self,
        _ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        range: Option<ByteRange>,
    ) -> Result<Bytes, DomainError> {
        let version = self
            .control
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;

        let backend = self.backends.get(&version.backend_id)?;
        match range {
            Some(r) => backend.get_range(&version.backend_path, r).await,
            None => backend.get(&version.backend_path).await,
        }
    }
}

//! Data-plane byte helpers: validate, store, hash, finalize, and read content
//! blobs. This is the in-process equivalent of the sidecar's byte path.
//!
//! [`DataPlaneService`] holds only what it needs to look up a version's backend
//! location and move bytes; it delegates the post-upload finalize step to the
//! control plane via the [`DataPlanePort`] callback.
//!
//! # Responsibilities (data-plane only)
//! * Validate the declared MIME type against the actual bytes.
//! * Write the blob to the correct backend.
//! * Compute the SHA-256 digest and call `finalize_upload` on the control plane.
//! * Read a whole blob or a byte range from the correct backend.
//!
//! # What this service intentionally does NOT do
//! * Authorization — callers (tests, the sidecar callback) are expected to have
//!   already validated the signed URL or the security context before calling in.
//! * Tenant scoping — version look-ups use `AccessScope::allow_all()` because the
//!   data plane operates on a `(file_id, version_id)` pair that was already minted
//!   by the control plane; the control plane re-checks scope during `finalize`.

use std::sync::Arc;

use bytes::Bytes;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage_sdk::ByteRange;

use crate::domain::error::DomainError;
use crate::domain::ports::DataPlanePort;
use crate::infra::backend::BackendRegistry;
use crate::infra::content::{hash, mime};

/// Data-plane service: moves bytes between callers and the storage backend.
///
/// Constructed from an `Arc<dyn DataPlanePort>` to access the control-plane
/// `finalize_upload` callback and version look-ups. Using the narrow port
/// trait (ISP) instead of the full `FileService` type keeps `data_plane.rs`
/// decoupled from the entire control-plane implementation.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct DataPlaneService {
    control: Arc<dyn DataPlanePort>,
    backends: BackendRegistry,
}

impl DataPlaneService {
    /// Build a `DataPlaneService` that delegates finalize to `control`.
    ///
    /// The backend registry is cloned from `control` so both layers share
    /// the same resources without duplication.
    #[must_use]
    pub fn new(control: Arc<dyn DataPlanePort>) -> Self {
        let backends = control.backends().clone();
        Self { control, backends }
    }

    /// Validate, store, hash, and finalize an uploaded blob in one step.
    ///
    /// This is the in-process equivalent of the sidecar's stream-and-bind flow;
    /// the sidecar performs the same steps while streaming bytes to the backend,
    /// then calls `finalize_upload` via the s2s control-plane callback.
    pub async fn put_content(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        declared_mime: &str,
        bytes: Bytes,
    ) -> Result<(), DomainError> {
        // Content-type validation against the actual bytes.
        mime::validate(declared_mime, &bytes)?;

        // Preflight authorization BEFORE touching the backend, so a rejected
        // request never persists or overwrites blob content. The post-write
        // `finalize_upload` re-checks as defense-in-depth.
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

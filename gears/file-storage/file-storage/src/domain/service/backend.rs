//! Backend migration, backend discovery, and `DataPlanePort` implementation.

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::audit::AuditOperation;
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::ports::DataPlanePort;
use crate::domain::service::FileService;
use crate::infra::backend::BackendCapabilities;
use crate::infra::backend::BackendRegistry;
use crate::infra::storage::Store;

// ── backend migration (P2-M4) ──────────────────────────────────────────────────

impl FileService {
    /// Relocate a non-versioned file's content from one backend to another
    /// without changing its identity (`file_id`, ownership, metadata, content
    /// hash).
    ///
    /// Steps:
    /// 1. Verify the file has exactly 1 version (non-versioned files only).
    /// 2. Read the blob from the source backend.
    /// 3. Write the blob to the destination backend at the canonical path.
    /// 4. Verify the content hash matches the stored version hash (SHA-256).
    /// 5. Transactionally update `backend_id` + `backend_path` and emit a
    ///    `BackendMigrate` audit row.
    /// 6. Best-effort delete the source blob (orphan cleanup if this fails).
    ///
    /// Returns `Ok(())` when the file already lives on the target backend
    /// (no-op), or after the migration completes successfully.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
    pub async fn migrate_backend(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        target_backend_id: &str,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // Only non-versioned files (exactly 1 version) may be migrated.
        let versions = self.store.list_versions(file_id).await?;
        if versions.len() != 1 {
            return Err(DomainError::versioned_file_migration_not_supported(file_id));
        }

        let version = &versions[0];

        // The version must be in the `available` state.
        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::conflict(
                "cannot migrate a version whose upload has not been finalized",
            ));
        }

        // No-op if already on the target backend.
        if version.backend_id == target_backend_id {
            return Ok(());
        }

        let source = self.backends.get(&version.backend_id)?;
        let dest = self.backends.get(target_backend_id)?;

        // Read the blob from the source backend.
        let bytes = source.get(&version.backend_path).await?;

        // Verify content hash before writing to destination.
        // Hash computation stays in `Store` (which already owns the SHA-256
        // allow-list import), so `FileService` needs no direct `hash` edge.
        Store::verify_content_hash(&bytes, &version.hash_value)?;

        // Write to the destination at the canonical path.
        let dest_path = Self::backend_path(file_id, version.version_id);
        dest.put(&dest_path, bytes).await?;

        // Transactionally update the version row and emit the audit row.
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::BackendMigrate,
            serde_json::json!({
                "from_backend": version.backend_id,
                "to_backend": target_backend_id,
                "version_id": version.version_id,
            }),
        );
        let updated = self
            .store
            .rebind_version_backend(
                file_id,
                version.version_id,
                target_backend_id,
                &dest_path,
                audit,
            )
            .await?;
        if !updated {
            // Concurrent operation removed the version before we could rebind —
            // the blob we just wrote to the destination is now an orphan; clean
            // it up best-effort and return not-found.
            self.best_effort_blob_delete(dest.id(), &dest_path).await;
            return Err(DomainError::version_not_found(file_id, version.version_id));
        }

        // Best-effort delete the source blob.
        self.best_effort_blob_delete(source.id(), &version.backend_path)
            .await;

        Ok(())
    }

    // ── backends discovery ────────────────────────────────────────────────────

    /// `GET /storages`: configured backends and their capabilities.
    #[must_use]
    pub fn list_backends(&self) -> Vec<(String, BackendCapabilities)> {
        self.backends.list()
    }

    /// `GET /storages/{id}`.
    pub fn get_backend(&self, id: &str) -> Result<(String, BackendCapabilities), DomainError> {
        let b = self.backends.get(id)?;
        Ok((b.id().to_owned(), b.capabilities()))
    }
}

// ── DataPlanePort implementation ──────────────────────────────────────────────

#[async_trait::async_trait]
impl DataPlanePort for FileService {
    fn backends(&self) -> &BackendRegistry {
        &self.backends
    }

    async fn authorize_write(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(), DomainError> {
        FileService::authorize_write(self, ctx, file_id).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<file_storage_sdk::FileVersion>, DomainError> {
        FileService::get_version(self, file_id, version_id).await
    }

    async fn finalize_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<(), DomainError> {
        FileService::finalize_upload(self, ctx, file_id, version_id, size, hash_value).await
    }
}

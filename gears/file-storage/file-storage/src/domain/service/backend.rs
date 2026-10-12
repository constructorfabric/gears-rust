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

impl FileService {
    /// Relocate a non-versioned file's content to another backend, keeping its identity.
    ///
    /// Reads the blob, verifies its content hash, writes it to the destination, then
    /// rebinds `backend_id`/`backend_path` and audits in one transaction; the source blob
    /// is deleted best-effort (left as an orphan if that fails). No-op when the file is
    /// already on the target backend.
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

        let versions = self.store.list_versions(file_id).await?;
        if versions.len() != 1 {
            return Err(DomainError::versioned_file_migration_not_supported(file_id));
        }

        let version = &versions[0];

        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::conflict(
                "cannot migrate a version whose upload has not been finalized",
            ));
        }

        if version.backend_id == target_backend_id {
            return Ok(());
        }

        let source = self.backends.get(&version.backend_id)?;
        let dest = self.backends.get(target_backend_id)?;

        // A non-durable destination (e.g. `memory`) risks data loss on restart, so it
        // requires the elevated admin-policy scope.
        if !dest.capabilities().durable {
            self.authorizer
                .authorize(
                    ctx,
                    actions::ADMIN_POLICY,
                    &file.gts_file_type,
                    Some(file_id),
                )
                .await?;
        }

        let bytes = source.get(&version.backend_path).await?;

        // Verify the content hash before writing to the destination. For multipart-composite
        // versions this uses the durable `version_hash_manifest` row, not the part rows.
        let hash_mode = crate::infra::content::hash_mode::HashMode::parse(&version.hash_mode)
            .ok_or_else(|| {
                DomainError::database(format!(
                    "version {} has an unrecognized hash_mode {:?}",
                    version.version_id, version.hash_mode
                ))
            })?;
        let manifest = match hash_mode {
            crate::infra::content::hash_mode::HashMode::WholeSha256 => None,
            crate::infra::content::hash_mode::HashMode::MultipartCompositeSha256 => {
                Some(self.store.get_version_manifest(version.version_id).await?.ok_or_else(
                    || {
                        DomainError::database(format!(
                            "multipart-composite version {} is missing its version_hash_manifest row",
                            version.version_id
                        ))
                    },
                )?)
            }
        };
        Store::verify_content_hash(&bytes, hash_mode, &version.hash_value, manifest.as_deref())?;

        let dest_path = Self::backend_path(file_id, version.version_id);
        dest.put(&dest_path, bytes).await?;

        // The CAS predicate is the pre-migration snapshot, so a concurrent migration that
        // already moved the pointer is detected rather than overwritten.
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
                &version.backend_id,
                &version.backend_path,
                target_backend_id,
                &dest_path,
                audit,
            )
            .await?;
        if !updated {
            // CAS lost: the version is gone or a concurrent migration moved the pointer.
            // Re-fetch to decide whether our destination blob is safe to delete.
            let current = self.store.get_version(file_id, version.version_id).await?;
            return match current {
                None => {
                    // Version gone: our blob is orphaned.
                    self.best_effort_blob_delete(dest.id(), &dest_path).await;
                    Err(DomainError::version_not_found(file_id, version.version_id))
                }
                Some(now)
                    if now.backend_id == target_backend_id && now.backend_path == dest_path =>
                {
                    // A concurrent migration to the same target committed this exact pointer
                    // (`dest_path` is deterministic): success, and do NOT delete the blob,
                    // it is the winner's live content.
                    Ok(())
                }
                Some(now) => {
                    // A different migration won; our blob is not live, so clean it up
                    // (guarded in case the live pointer coincides with it).
                    if !(now.backend_id == dest.id() && now.backend_path == dest_path) {
                        self.best_effort_blob_delete(dest.id(), &dest_path).await;
                    }
                    Err(DomainError::conflict(
                        "concurrent backend migration in progress",
                    ))
                }
            };
        }

        self.best_effort_blob_delete(source.id(), &version.backend_path)
            .await;

        Ok(())
    }

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

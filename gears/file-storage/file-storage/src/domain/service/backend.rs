//! Backend migration and backend discovery.

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::audit::AuditOperation;
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::service::FileService;
use crate::domain::storage_layout;
use crate::infra::backend::BackendCapabilities;
use crate::infra::content::hash_mode::{HashMode, Manifest};
use crate::infra::content::stream_verify;

// ── backend migration (P2-M4) ──────────────────────────────────────────────────

impl FileService {
    /// Relocate a non-versioned file's content from one backend to another
    /// without changing its identity (`file_id`, ownership, metadata, content
    /// hash).
    ///
    /// Steps:
    /// 1. Verify the file has exactly 1 version (non-versioned files only).
    /// 2. Stream the blob from the source backend into the destination
    ///    backend at the canonical path, verifying its content hash (SHA-256,
    ///    mode-aware per ADR-0006) incrementally on the same pass.
    /// 3. If verification fails (or the source stream breaks mid-read), fail
    ///    without committing the CAS below, and best-effort delete the
    ///    destination object if this call is the one that created it.
    /// 4. Transactionally update `backend_id` + `backend_path` and emit a
    ///    `BackendMigrate` audit row.
    /// 5. Best-effort delete the source blob (orphan cleanup if this fails).
    ///
    /// Returns `Ok(())` when the file already lives on the target backend
    /// (no-op), or after the migration completes successfully.
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

        // Migrating content onto a non-durable backend (e.g. a dev/test
        // `memory` backend) risks silent data loss on the next restart. An
        // ordinary WRITE-authorized caller may not do this implicitly — it
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

        // Stream the blob from the source backend straight into the
        // destination, verifying its content hash incrementally on the same
        // pass (mode-aware, ADR-0006) instead of materializing the whole
        // object in memory. For `whole-sha256` this hashes the object as it
        // streams through. For `multipart-composite-sha256` it fetches the
        // version's `version_hash_manifest` row up front and hashes each part
        // against that manifest ALONE (split-rehash-rebuild-compare) as the
        // corresponding bytes stream past, with no dependency on
        // `multipart_upload_parts` still existing — the manifest is the
        // durable, self-contained record. Either way, the verdict is only
        // known once the destination write below has fully drained the
        // stream — see `infra::content::stream_verify`'s doc comment.
        let expected_len = u64::try_from(version.size).unwrap_or(0);
        let hash_mode = HashMode::parse(&version.hash_mode).ok_or_else(|| {
            DomainError::database(format!(
                "version {} has an unrecognized hash_mode {:?}",
                version.version_id, version.hash_mode
            ))
        })?;
        let manifest = match hash_mode {
            HashMode::WholeSha256 => None,
            HashMode::MultipartCompositeSha256 => {
                let raw = self
                    .store
                    .get_version_manifest(version.version_id)
                    .await?
                    .ok_or_else(|| {
                        DomainError::database(format!(
                            "multipart-composite version {} is missing its version_hash_manifest row",
                            version.version_id
                        ))
                    })?;
                Some(Manifest::from_wire_string(&raw)?)
            }
        };

        let source_stream = source
            .get_stream(&version.backend_path, expected_len)
            .await?;
        let (verified_stream, verify_slot) = stream_verify::verify_stream(
            source_stream,
            expected_len,
            hash_mode,
            version.hash_value.clone(),
            manifest,
        )?;

        // Write to the destination at the canonical path. Create-exclusive
        // (`publish_exclusive`, not `put_stream`): `dest_path` is
        // deterministic (`/{file_id}/{version_id}`), so two concurrent
        // migrations to the SAME target both attempt to write here — with a
        // plain overwriting write the second writer to land always wins
        // physically, which only stays harmless as long as both writers'
        // content is identical. `publish_exclusive` keeps that true even when
        // it might not otherwise be: once the first writer's (verified, or
        // about to be verified) bytes are in place, a second writer whose own
        // read from the source turned out corrupted can never clobber them —
        // it observes `created: false` and its own bytes are simply
        // discarded. `created` below is what decides whether a failed
        // verification may delete the object this call just wrote (see
        // `features/backend-migration.md`).
        let dest_path = storage_layout::backend_path(file_id, version.version_id);
        let outcome = dest
            .publish_exclusive(&dest_path, verified_stream, Some(expected_len))
            .await?;

        let verify_result = verify_slot
            .lock()
            .map_err(|_| DomainError::backend(dest.id(), "poisoned content-verification lock"))?
            .take()
            .unwrap_or_else(|| {
                Err(DomainError::backend(
                    dest.id(),
                    "destination write completed without fully draining the verified source stream",
                ))
            });
        if let Err(verify_err) = verify_result {
            // Only clean up the destination if THIS call actually created the
            // object there: `created: false` means something else (a
            // concurrent migration, or an earlier attempt) already put
            // verified content at this exact path, and it is not this call's
            // to delete.
            if outcome.created {
                self.best_effort_blob_delete(dest.id(), &dest_path).await;
            }
            return Err(verify_err);
        }

        // Transactionally update the version row and emit the audit row. The
        // CAS predicate is the pre-migration snapshot captured above (before
        // the source read / destination write), so a concurrent migration
        // that already moved the pointer is detected rather than silently
        // overwritten.
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
            // The CAS lost: either the version is gone, or a concurrent
            // migration already moved the pointer away from the snapshot we
            // started from. Re-fetch to tell these apart — the destination
            // blob we just wrote may or may not be safe to clean up depending
            // on which case this is.
            let current = self.store.get_version(file_id, version.version_id).await?;
            return match current {
                None => {
                    // Version gone: the blob we wrote is genuinely orphaned.
                    self.best_effort_blob_delete(dest.id(), &dest_path).await;
                    Err(DomainError::version_not_found(file_id, version.version_id))
                }
                Some(now)
                    if now.backend_id == target_backend_id && now.backend_path == dest_path =>
                {
                    // A concurrent migration to the SAME target already
                    // committed this exact pointer as the live one (dest_path
                    // is deterministic, so racers to the same backend collide
                    // on the same path). Treat as a successful no-op and, above
                    // all, do NOT delete the destination blob -- it is the
                    // winner's live content, not ours to clean up.
                    Ok(())
                }
                Some(now) => {
                    // A different concurrent migration won. Our destination
                    // write is not the live pointer, so it is safe to clean up
                    // -- guarded by the belt-and-suspenders check below in
                    // case the live pointer ever coincides with it for some
                    // other reason.
                    if !(now.backend_id == dest.id() && now.backend_path == dest_path) {
                        self.best_effort_blob_delete(dest.id(), &dest_path).await;
                    }
                    Err(DomainError::conflict(
                        "concurrent backend migration in progress",
                    ))
                }
            };
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

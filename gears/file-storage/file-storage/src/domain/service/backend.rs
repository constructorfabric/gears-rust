//! Backend migration and backend discovery.

use futures::StreamExt;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::audit::AuditOperation;
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::service::FileService;
use crate::domain::storage_layout;
use crate::infra::backend::{BackendCapabilities, StorageBackend, classify_stream_io_error};
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
    /// 3. If verification of the source stream fails (or it breaks
    ///    mid-read), fail without committing the CAS below, and best-effort
    ///    delete the destination object if this call is the one that created
    ///    it. If the destination object already existed (`created: false`),
    ///    that alone is not proof it was ever verified — an earlier attempt
    ///    (this exact call retried, or a distinct migration racing on the
    ///    same deterministic path) can be interrupted after writing but
    ///    before its own hash check and cleanup. So the pre-existing object
    ///    is read back and re-verified against the same hash spec before it
    ///    is trusted. Only a **confirmed** mismatch (the object was read in
    ///    full and its hash/length disagree) proves it is garbage and gets
    ///    best-effort deleted; a re-verification that could not be completed
    ///    at all (the read-back never opened, broke off mid-read, or left no
    ///    verdict) proves nothing about the object's content, is left
    ///    untouched, and surfaces as a retryable backend error instead — see
    ///    `Self::verify_preexisting_dest_and_clean_on_confirmed_mismatch`'s
    ///    own doc comment.
    /// 4. Immediately before the CAS below, re-`stat` the destination object
    ///    to confirm it is still there at the expected size. This narrows
    ///    (it cannot fully close) the window between step 3's verification
    ///    and the CAS: see the call site's own comment for why that is
    ///    enough given step 3's stricter deletion rule.
    /// 5. Transactionally update `backend_id` + `backend_path` and emit a
    ///    `BackendMigrate` audit row.
    /// 6. Best-effort delete the source blob (orphan cleanup if this fails).
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
        // object in memory, and only return once that verification (source
        // stream, plus a pre-existing destination object's own read-back
        // where relevant) has passed — see
        // `Self::stream_verify_and_publish_to_dest`'s own doc comment for the
        // full contract, including its cleanup behavior on failure.
        let expected_len = u64::try_from(version.size).unwrap_or(0);
        let dest_path = storage_layout::backend_path(file_id, version.version_id);
        self.stream_verify_and_publish_to_dest(
            source.as_ref(),
            dest.as_ref(),
            version,
            &dest_path,
            expected_len,
        )
        .await?;

        // Immediately before committing the CAS below, re-confirm the object
        // this call is about to make live is actually still there and still
        // the size this version declares. This narrows -- it cannot fully
        // close -- the window between "verified" and "CAS": nothing
        // coordinates a delete that lands in between this `stat` and the CAS
        // call right below it either. What it closes is the specific
        // data-loss scenario this function exists to prevent: the only path
        // inside this function that ever deletes a destination object now
        // requires a *confirmed* hash/length mismatch
        // (`Self::verify_preexisting_dest_and_clean_on_confirmed_mismatch`
        // above) -- content a writer with the correct bytes could never have
        // produced -- so a delayed writer's own correctly-verified object can
        // no longer be destroyed by a concurrent migration's cleanup path. An
        // object that still vanishes here can only be the result of
        // something outside that coordination entirely (an external actor,
        // an operator action, direct backend surgery), which is exactly the
        // class of failure a retryable backend error is the right response
        // to -- not silently proceeding to bind a pointer at nothing.
        match dest.stat(&dest_path).await? {
            Some(actual_len) if actual_len == expected_len => {}
            Some(actual_len) => {
                // A concurrent actor changed the object in the narrow window
                // between this call's own verification and this re-stat — see
                // the doc comment above. Retrying the migration re-verifies
                // and re-CASes from scratch, so this is transient, not a
                // permanent fault.
                return Err(DomainError::backend_unavailable(
                    dest.id(),
                    format!(
                        "destination object size changed before commit: expected \
                         {expected_len} byte(s), found {actual_len}"
                    ),
                ));
            }
            None => {
                return Err(DomainError::backend_unavailable(
                    dest.id(),
                    "destination object disappeared before commit",
                ));
            }
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

    /// The streaming transfer + verification half of `migrate_backend`:
    /// resolves `version`'s mode-aware hash spec (`hash_mode` and, for
    /// `multipart-composite-sha256`, its stored manifest), streams its bytes
    /// from `source` straight into `dest` at `dest_path` (create-exclusive —
    /// see [Concurrent-Migration CAS
    /// Resolution](../../../docs/features/backend-migration.md) for why),
    /// verifying incrementally on the same pass, and returns `Ok(())` only
    /// once that content is confirmed correct at `dest_path`:
    /// - if this call's own write's source-stream verification fails (or the
    ///   source stream breaks mid-read), the destination object is
    ///   best-effort deleted only if this call actually created it
    ///   (`created: false` means something else already had bytes there
    ///   before this call, which this verification says nothing about);
    /// - if this call's own `publish_exclusive` reported `created: false`
    ///   (the destination already held bytes), that pre-existing object is
    ///   independently read back and re-verified — see
    ///   `Self::verify_preexisting_dest_and_clean_on_confirmed_mismatch`'s own
    ///   doc comment for why `created: false` alone is not proof of anything,
    ///   and for that path's own cleanup rule, which only deletes on a
    ///   *confirmed* mismatch and otherwise surfaces a retryable backend
    ///   error without touching the object.
    async fn stream_verify_and_publish_to_dest(
        &self,
        source: &dyn StorageBackend,
        dest: &dyn StorageBackend,
        version: &file_storage_sdk::FileVersion,
        dest_path: &str,
        expected_len: u64,
    ) -> Result<(), DomainError> {
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
            manifest.clone(),
        )?;

        let outcome = dest
            .publish_exclusive(dest_path, verified_stream, Some(expected_len))
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
            if outcome.created {
                self.best_effort_blob_delete(dest.id(), dest_path).await;
            }
            return Err(verify_err);
        }

        if !outcome.created {
            self.verify_preexisting_dest_and_clean_on_confirmed_mismatch(
                dest,
                dest_path,
                expected_len,
                hash_mode,
                version.hash_value.clone(),
                manifest,
            )
            .await?;
        }

        Ok(())
    }

    /// Called from `migrate_backend` only when this call's own
    /// `publish_exclusive` reported `created: false`, i.e. `dest_path`
    /// already held bytes before this call ever tried to write there.
    ///
    /// `created: false` alone is not proof those bytes were ever
    /// hash-checked: an earlier attempt (this exact call retried, or a
    /// distinct migration racing on the same deterministic path) can write
    /// here via its own `publish_exclusive` and then be interrupted —
    /// process crash, cancellation — before it reads its own `verify_slot`
    /// and cleans up on mismatch, leaving unverified bytes sitting at this
    /// path with no live database pointer. That is exactly the object
    /// `migrate_backend`'s CAS is about to make live, so this reads it back
    /// and runs it through the same mode-aware verification the source
    /// stream already went through, rather than trusting `created: false`
    /// as proof someone else already did.
    ///
    /// [`verify_existing_dest_object`] reports one of two outcomes, and they
    /// are handled very differently:
    /// - a [`PreexistingDestVerdict::Mismatch`] means the object was read to
    ///   completion and its hash/length disagree with what this version
    ///   declares. `publish_exclusive` publishes atomically, so any writer
    ///   holding the correct bytes always passes this exact check — a
    ///   passing competitor's blob can never end up here. This is therefore
    ///   *confirmed* garbage (it carries no live database pointer either
    ///   way), safe — and necessary, so it is not leaked forever — to
    ///   best-effort delete unconditionally.
    /// - a [`PreexistingDestVerdict::Unconfirmed`] means the check itself
    ///   could not be completed: the read-back stream never opened, broke
    ///   off mid-read, or the verdict slot was left empty. This proves
    ///   NOTHING about the object's actual content — it may be perfectly
    ///   valid data written by a concurrent migration that is merely
    ///   momentarily unreachable (a dropped connection, a transient backend
    ///   fault) — so deleting it here would recreate exactly the data-loss
    ///   bug this function exists to prevent. It is left untouched and this
    ///   returns the underlying error unchanged: `DomainError::BackendUnavailable`
    ///   when the cause was classified transient, `DomainError::Backend`
    ///   otherwise — either way a rejection of this call, never of the
    ///   migration itself, and `api/rest/error.rs` maps each to its own 5xx
    ///   at the REST boundary.
    async fn verify_preexisting_dest_and_clean_on_confirmed_mismatch(
        &self,
        dest: &dyn StorageBackend,
        dest_path: &str,
        expected_len: u64,
        hash_mode: HashMode,
        hash_value: Vec<u8>,
        manifest: Option<Manifest>,
    ) -> Result<(), DomainError> {
        match verify_existing_dest_object(
            dest,
            dest_path,
            expected_len,
            hash_mode,
            hash_value,
            manifest,
        )
        .await
        {
            Ok(()) => Ok(()),
            Err(PreexistingDestVerdict::Mismatch(mismatch_err)) => {
                self.best_effort_blob_delete(dest.id(), dest_path).await;
                Err(mismatch_err)
            }
            Err(PreexistingDestVerdict::Unconfirmed(backend_err)) => Err(backend_err),
        }
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

/// Outcome of [`verify_existing_dest_object`]'s attempt to establish whether
/// a pre-existing destination object is valid: either the check ran to
/// completion and definitively found the object wrong, or the check itself
/// could not be completed at all. See
/// [`FileService::verify_preexisting_dest_and_clean_on_confirmed_mismatch`]'s
/// doc comment for how each variant is handled.
enum PreexistingDestVerdict {
    /// The object was read in full and its hash/length disagree with what
    /// this version declares. `publish_exclusive` publishes atomically, so
    /// this can only be genuinely bad content, never a passing competitor's
    /// blob caught mid-write.
    Mismatch(DomainError),
    /// The check could not be completed either way: the read-back stream
    /// never opened, broke off mid-read, or the verdict slot was left empty.
    /// This is not evidence the object is bad — it says nothing about its
    /// content at all.
    Unconfirmed(DomainError),
}

/// Read back the object already sitting at `dest_path` on `dest` — reached
/// only when a `publish_exclusive` call reported `created: false`, i.e. this
/// call did not write it — and verify it against the same mode-aware hash
/// spec (`hash_mode`/`hash_value`/`manifest`) the source stream was already
/// checked against in [`FileService::migrate_backend`]. `created: false`
/// means only that *something* wrote here first; it is not evidence that
/// whatever it wrote was ever hash-checked, since a prior writer can crash or
/// be cancelled after its own `publish_exclusive` call returns but before it
/// reads its own `verify_slot` and cleans up on mismatch.
///
/// Every failure short of a fully-drained, definitively-mismatched read is
/// reported as [`PreexistingDestVerdict::Unconfirmed`] rather than assumed to
/// mean the object is bad — opening the stream, a mode/manifest mismatch
/// (`stream_verify::verify_stream`'s own upfront validation), a mid-read
/// error, and an empty verdict slot all land here.
async fn verify_existing_dest_object(
    dest: &dyn StorageBackend,
    dest_path: &str,
    expected_len: u64,
    hash_mode: HashMode,
    hash_value: Vec<u8>,
    manifest: Option<Manifest>,
) -> Result<(), PreexistingDestVerdict> {
    let stream = dest
        .get_stream(dest_path, expected_len)
        .await
        .map_err(PreexistingDestVerdict::Unconfirmed)?;
    let (mut verified, verify_slot) =
        stream_verify::verify_stream(stream, expected_len, hash_mode, hash_value, manifest)
            .map_err(PreexistingDestVerdict::Unconfirmed)?;
    // Drain to the wrapped stream's terminal `None` -- the verdict is only
    // populated once that happens (see `stream_verify`'s module doc comment).
    // The bytes themselves are irrelevant here, only the verdict is, so they
    // are read and dropped. A read error here means the object opened fine
    // and then broke off mid-read -- the verdict slot is never populated in
    // that case, so this reports `Unconfirmed` directly rather than falling
    // through to the (also-empty) slot check below.
    while let Some(chunk) = verified.next().await {
        if let Err(e) = chunk {
            return Err(PreexistingDestVerdict::Unconfirmed(
                classify_stream_io_error(
                    dest.id(),
                    "failed reading back destination object for verification",
                    &e,
                ),
            ));
        }
    }
    let verdict = verify_slot
        .lock()
        .map_err(|_| {
            PreexistingDestVerdict::Unconfirmed(DomainError::backend(
                dest.id(),
                "poisoned content-verification lock",
            ))
        })?
        .take();
    match verdict {
        Some(Ok(())) => Ok(()),
        Some(Err(mismatch_err)) => Err(PreexistingDestVerdict::Mismatch(mismatch_err)),
        None => Err(PreexistingDestVerdict::Unconfirmed(DomainError::backend(
            dest.id(),
            "destination read-back ended without fully draining the verified stream",
        ))),
    }
}

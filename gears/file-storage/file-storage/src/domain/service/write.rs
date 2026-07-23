//! Write-path operations: finalize upload, bind (CAS), metadata update, and ownership transfer.

use std::collections::HashMap;

use futures::StreamExt;
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataPatch, File};

use crate::domain::audit::{AuditEntry, AuditOperation};
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::policy::PolicyResolver;
use crate::domain::ports::AutoBindOnFinalize;
use crate::domain::service::{FileService, FinalizeByTokenOutcome, VersionRef};
use crate::infra::backend::StorageBackend;
use crate::infra::content::hash;
use crate::infra::content::mime::{
    MIME_SNIFF_PREFIX_BYTES, enforce_size_ceiling_for_validated_mime, validate_and_resolve_mime,
};
use crate::infra::external_clients::UsageDelta;
use crate::infra::signed_url::{Claims, Op, UploadConstraints};

/// Read back the blob actually stored at `backend_path` on `backend`,
/// streaming it chunk by chunk rather than buffering the whole object in
/// memory (`cpt-cf-file-storage-fr-backend-abstraction`, memory-safety fix:
/// finalize's read-back is the mirror image of `put_stream`'s streaming write
/// and must be equally memory-bounded, not defeat it by buffering the whole
/// object back in on the read side). Computes the actual byte count and
/// SHA-256 digest incrementally via [`hash::Hasher`], while also capturing up
/// to [`MIME_SNIFF_PREFIX_BYTES`] of the leading bytes for
/// [`mime::validate`]'s magic-byte sniffing (which only ever inspects a small
/// bounded prefix — see that constant's doc comment).
///
/// A genuinely missing object (no prior successful PUT) surfaces as
/// `DomainError::validation("content", ...)` — the client's finalize raced
/// ahead of a completed PUT, a 4xx the caller can act on. A backend/transport
/// failure while opening or reading the object back (e.g. an S3 connection
/// dropped mid-read of a fully-uploaded blob) instead preserves the underlying
/// `DomainError::backend` error, so a transient 5xx is not misreported as
/// "never uploaded" and its root cause is not discarded.
///
/// Returns `(actual_size, actual_hash, mime_sniff_prefix)`.
async fn read_back_and_hash_streaming(
    backend: &dyn StorageBackend,
    backend_path: &str,
) -> Result<(i64, Vec<u8>, Vec<u8>), DomainError> {
    let no_content_err = || {
        DomainError::validation(
            "content",
            "no uploaded content found at the backend path; PUT was not completed",
        )
    };

    let mut stream = match backend.get_stream(backend_path).await {
        Ok(stream) => stream,
        // Opening the read-back stream failed. Only a genuinely-absent object
        // (finalize raced ahead of a completed PUT) is the caller's fault and
        // maps to the not-found validation error; a backend/transport failure
        // preserves its original `DomainError` so a transient 5xx is not
        // masked as "never uploaded". `exists` is the authoritative signal —
        // if it too fails, err on surfacing the real backend error.
        Err(open_err) => {
            return Err(match backend.exists(backend_path).await {
                Ok(false) => no_content_err(),
                _ => open_err,
            });
        }
    };

    let mut hasher = hash::Hasher::new();
    let mut prefix: Vec<u8> = Vec::with_capacity(MIME_SNIFF_PREFIX_BYTES);
    while let Some(chunk) = stream.next().await {
        // A mid-stream read error means the object opened fine and then the
        // backend/transport failed partway through — it is never "not
        // uploaded". Preserve it as a backend error (mirroring `put_stream`'s
        // chunk handling) rather than collapsing it into the not-found case.
        let chunk = chunk.map_err(|e| DomainError::backend(backend.id(), e.to_string()))?;
        if prefix.len() < MIME_SNIFF_PREFIX_BYTES {
            let take = (MIME_SNIFF_PREFIX_BYTES - prefix.len()).min(chunk.len());
            prefix.extend_from_slice(&chunk[..take]);
        }
        hasher.update(&chunk);
    }

    let actual_size = i64::try_from(hasher.len()).unwrap_or(i64::MAX);
    let actual_hash = hasher.finalize();
    Ok((actual_size, actual_hash, prefix))
}

impl FileService {
    /// Authorize a write to `file_id` (WRITE action) without mutating anything.
    /// The data plane calls this as a preflight **before** writing bytes to a
    /// backend, so a rejected request never persists/overwrites blob content
    /// (the post-write `finalize_upload` re-checks as defense-in-depth).
    pub async fn authorize_write(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(), DomainError> {
        let file = self
            .store
            .require_file(&Self::tenant_scope(ctx), file_id)
            .await?;
        self.authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;
        Ok(())
    }

    /// Record an uploaded version's size+hash and mark it available. Called by
    /// the sidecar after streaming bytes to the backend (write action).
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    #[tracing::instrument(skip_all)]
    pub async fn finalize_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-actor-request
        // Entry point: the actor's write request (finalize is one of the
        // audited operations recorded by `cpt-cf-file-storage-flow-audit-trail-record-write`).
        // @cpt-end:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-actor-request
        if size < 0 {
            return Err(DomainError::validation("size", "must be non-negative"));
        }

        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // @cpt-cf-file-storage-fr-size-limits-policy
        // Defense-in-depth size check: re-enforce the policy size ceiling at
        // finalization time even though the sidecar already checked the
        // upload constraint in the signed URL.
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;
        let version_mime = version.mime_type.clone();
        let backend_id = version.backend_id.clone();
        let policy = self
            .get_effective_policy_internal(ctx.subject_tenant_id(), file.owner_id)
            .await?;
        let backend = if backend_id.is_empty() {
            self.backends.default_backend()
        } else {
            self.backends.get(&backend_id)?
        };
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &version_mime,
            backend.capabilities().max_size_bytes,
        );
        // @cpt-begin:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-size-compare
        if let Some(limit) = effective_max
            && size > 0
            && size.cast_unsigned() > limit
        {
            // @cpt-end:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-size-compare
            // @cpt-begin:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-return
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
            // @cpt-end:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-return
        }

        // Never trust the caller's claimed size/hash: stream the blob
        // actually present at the version's backend path and recompute both
        // from the real bytes, never buffering more than one chunk (plus a
        // small MIME-sniff prefix) in memory regardless of object size. A
        // finalize with no prior successful PUT (no object at that path) or a
        // forged size/hash claim is rejected here rather than silently
        // persisted.
        let (actual_size, actual_hash, mime_sniff_prefix) =
            read_back_and_hash_streaming(backend.as_ref(), &version.backend_path).await?;
        if actual_size != size {
            return Err(DomainError::validation(
                "size",
                "claimed size does not match the uploaded content",
            ));
        }
        if actual_hash != hash_value {
            return Err(DomainError::hash_mismatch(
                hex::encode(&hash_value),
                hex::encode(&actual_hash),
            ));
        }

        // Declared MIME type is never trustworthy either: validate the
        // read-back blob's real bytes against `version_mime` (reusing the
        // same magic-byte sniffing the in-process data plane runs at
        // ingress), rejecting a mismatch before anything is finalized. Only
        // the leading `MIME_SNIFF_PREFIX_BYTES` are needed — see that
        // constant's doc comment for why that is always sufficient. The
        // returned type is the sniffed/canonical one when the bytes carry a
        // recognizable signature, otherwise the declared type unchanged.
        // @cpt-cf-file-storage-fr-content-type-validation
        let validated_mime = validate_and_resolve_mime(&version_mime, &mime_sniff_prefix)?;
        enforce_size_ceiling_for_validated_mime(
            &policy,
            &version_mime,
            &validated_mime,
            backend.capabilities().max_size_bytes,
            actual_size,
        )?;

        // @cpt-cf-file-storage-fr-audit-trail
        // @cpt-begin:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-build
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            // @cpt-begin:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-operation
            AuditOperation::FinalizeVersion,
            // @cpt-end:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-operation
            // @cpt-begin:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-detail
            serde_json::json!({ "version_id": version_id, "size": size }),
            // @cpt-end:cpt-cf-file-storage-algo-audit-trail-build-entry:p1:inst-buildentry-detail
        );
        // @cpt-end:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-build

        // Persist the read-back-derived size and the verified hash, not the
        // caller's size claim. `validated_mime` is persisted in place of the
        // client's original declaration.
        // @cpt-begin:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-pass-through
        let ok = self
            .store
            .finalize_version(
                file_id,
                version_id,
                actual_size,
                actual_hash,
                // Single-part upload → always whole-object SHA-256 (ADR-0006
                // mode 1). No part count, no offset-manifest row.
                crate::infra::content::hash_mode::HashMode::WholeSha256,
                None,
                None,
                Some(validated_mime),
                audit,
                // No auto-bind on the user-facing (JWT-authorized) finalize
                // path — binding stays an explicit `bind` call here.
                None,
            )
            .await?
            .updated;
        // @cpt-end:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-pass-through
        if !ok {
            // Distinguish "already finalized" (409, using the `version`
            // snapshot read earlier in this call) from "row is gone" (404).
            return Err(
                if version.status == file_storage_sdk::VersionStatus::Available {
                    DomainError::conflict("version already finalized")
                } else {
                    DomainError::version_not_found(file_id, version_id)
                },
            );
        }

        // @cpt-cf-file-storage-fr-usage-reporting
        // Credit the read-back-derived bytes now that the version is durably
        // finalized. `create_file` already reported `+1 file` with `0 bytes`
        // (bytes are unknown at creation time), so `file_count_delta` here is
        // `0` -- this is the byte-crediting complement that makes the total
        // symmetric with the debit at `delete_file_inner`/`delete_version`.
        self.report_usage(UsageDelta {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            bytes_delta: actual_size,
            file_count_delta: 0,
        });

        self.metrics.record_operation("finalize_upload", "ok");
        // @cpt-begin:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-return
        Ok(())
        // @cpt-end:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-return
    }

    /// `POST /files/{id}/bind`: swap the content pointer to `version_id` under
    /// optimistic CAS guarded by the `If-Match` content ETag. Returns the
    /// updated file; `PreconditionFailed` on conflict (re-read the ETag and
    /// rebind). The REST layer maps that canonical error to HTTP 400.
    ///
    /// `if_match` is the opaque content ETag (or `*`, or `None` for the first
    /// bind). The server recomputes the current ETag and compares — it never
    /// reverses the ETag back to a `content_id`.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    #[tracing::instrument(skip_all)]
    pub async fn bind(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<File, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // The version must exist and be available.
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;
        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::conflict(
                "cannot bind a version whose upload has not been finalized",
            ));
        }

        // Validate the If-Match precondition against the current content ETag.
        let expected_content_id = file.content_id;
        let current_etag = expected_content_id.map(|c| etag::content_etag(file_id, c));
        match if_match {
            // The first bind (no content yet) may omit If-Match; rebinding
            // already-bound content MUST carry it, otherwise the advertised
            // conditional update degrades into an unconditional overwrite.
            None => {
                if expected_content_id.is_some() {
                    return Err(DomainError::precondition_failed(
                        "If-Match is required to rebind already-bound content",
                    ));
                }
            }
            Some(m) => {
                let m = m.trim();
                if m != "*" && Some(m) != current_etag.as_deref() {
                    return Err(DomainError::precondition_failed(
                        "If-Match does not match the current content ETag",
                    ));
                }
            }
        }

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::PatchContent,
            serde_json::json!({ "version_id": version_id }),
        );

        // @cpt-cf-file-storage-fr-file-events
        let event = Some(Self::make_file_event(
            file.tenant_id,
            file.owner_id,
            file_id,
            "file.content_updated",
            serde_json::json!({ "version_id": version_id }),
        ));

        // Swap the content pointer (CAS) and flip `is_current` in a SINGLE
        // transaction so `files.content_id` and `file_versions.is_current` can
        // never diverge if a later write fails (DESIGN §3.7 bind invariant).
        let now = OffsetDateTime::now_utc();
        let swapped = self
            .store
            .bind_atomic_with_event(
                &scope,
                file_id,
                expected_content_id,
                version_id,
                now,
                audit,
                event,
            )
            .await?;
        if !swapped {
            return Err(DomainError::precondition_failed(
                "content pointer changed concurrently; re-read the ETag and rebind",
            ));
        }

        let bound = self.store.require_file(&scope, file_id).await?;
        self.metrics.record_operation("bind", "ok");
        Ok(bound)
    }

    /// Issue a signed download URL for a version (shared helper used by
    /// `read_ops.rs`). Visibility is `pub(super)` so only sibling modules use it.
    ///
    /// `download_meta` is `Some((content_type, etag))` (P2 1.11) — threaded
    /// straight through to `sign_url`'s `Op::Get`-only claims population.
    pub(super) fn build_download_url(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        backend_id: String,
        backend_path: String,
        download_meta: Option<(String, String)>,
    ) -> Result<String, DomainError> {
        self.sign_url(
            Op::Get,
            &VersionRef {
                file_id,
                version_id,
                backend_id,
                backend_path,
            },
            UploadConstraints::default(),
            download_meta,
        )
    }

    // ── metadata update ────────────────────────────────────────────────────────

    /// `PATCH /files/{id}`: JSON-merge-patch the custom metadata and bump
    /// `meta_version`, optionally guarded by `If-Match-Metadata`.
    ///
    /// A `PatchMetadata` audit row and a `file.metadata_updated` file event are
    /// enqueued in the same transaction as the CAS + patch.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    pub async fn update_metadata(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        patch: CustomMetadataPatch,
        expected_meta_version: Option<i64>,
    ) -> Result<File, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // @cpt-cf-file-storage-fr-metadata-limits
        // Compute what the resulting metadata will look like after this patch,
        // then validate against the effective policy.
        let policy = self
            .get_effective_policy_internal(ctx.subject_tenant_id(), file.owner_id)
            .await?;
        let existing = self.store.list_metadata(file_id).await?;
        // Build a map from existing entries and apply the patch (merge semantics).
        let mut merged: HashMap<String, String> =
            existing.into_iter().map(|e| (e.key, e.value)).collect();
        for (key, value) in &patch.entries {
            match value {
                Some(v) => {
                    merged.insert(key.clone(), v.clone());
                }
                None => {
                    merged.remove(key);
                }
            }
        }
        let result_pairs: Vec<(String, String)> = merged.into_iter().collect();
        PolicyResolver::check_metadata_limits(&policy, &result_pairs)?;

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::PatchMetadata,
            serde_json::json!({ "expected_meta_version": expected_meta_version }),
        );

        // @cpt-cf-file-storage-fr-file-events
        // The `meta_version` payload is left empty here and stamped with the
        // authoritative committed revision inside `patch_metadata_atomic`'s
        // transaction: an unconditional patch (`expected_meta_version = None`)
        // that races another cannot know its post-bump revision from the
        // pre-transaction snapshot, so the store fills it from the CAS result.
        let event = Some(Self::make_file_event(
            file.tenant_id,
            file.owner_id,
            file_id,
            "file.metadata_updated",
            serde_json::json!({}),
        ));

        // Apply the meta-version CAS and the patch in ONE transaction. The CAS
        // runs first, so a stale `expected_meta_version` aborts before any row
        // is touched and the rollback guarantees no partial metadata change is
        // committed (the optimistic-concurrency guard cannot be bypassed). The
        // per-key delete-then-insert upsert is also covered by the rollback, so
        // a failed insert can never leave a key permanently removed.
        let now = OffsetDateTime::now_utc();
        let bumped = self
            .store
            .patch_metadata_atomic(
                &scope,
                file_id,
                expected_meta_version,
                patch,
                now,
                audit,
                event,
            )
            .await?;
        if !bumped {
            return Err(DomainError::precondition_failed(
                "metadata revision changed concurrently (If-Match-Metadata)",
            ));
        }
        self.store.require_file(&scope, file_id).await
    }

    // ── ownership transfer (P2-M5) ────────────────────────────────────────────

    /// `POST /files/{id}/transfer`: transfer ownership of a file to a new owner.
    ///
    /// The new owner's `owner_kind` and `owner_id` replace the current values.
    /// An audit row (`TransferOwnership`) and a file event (`file.owner_transferred`)
    /// are enqueued in the same transaction as the update.
    ///
    /// `new_owner_id` is rejected if it is the nil UUID. This gear has no
    /// principal directory (no account-management SDK is wired into
    /// `cf-gears-file-storage`), so it cannot verify that `new_owner_id` names
    /// a real, same-tenant principal — only that it is not an obviously
    /// malformed sentinel. Note that a *cross-tenant* transfer is already
    /// structurally impossible through this endpoint: `tenant_id` on the
    /// updated row always comes from the existing file (scoped to
    /// `ctx.subject_tenant_id()` via [`Self::tenant_scope`]), never from the
    /// request, so `new_owner_id` can only ever be recorded under the
    /// caller's own tenant. Full existence/same-tenant-*membership*
    /// validation of an arbitrary `new_owner_id` (i.e. "is this UUID actually
    /// a principal in my tenant?") would require a cross-gear
    /// account-management lookup and is a follow-up (🛑, also ties into
    /// whether this action should require a distinct privileged-transfer
    /// grant rather than reusing the file WRITE grant — see 0.7's
    /// admin-scope decision).
    ///
    /// @cpt-cf-file-storage-fr-ownership-transfer
    /// @cpt-cf-file-storage-fr-usage-reporting
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-dod:cpt-cf-file-storage-dod-ownership-transfer-endpoint:p1
    pub async fn transfer_ownership(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        new_owner_kind: file_storage_sdk::OwnerKind,
        new_owner_id: Uuid,
    ) -> Result<File, DomainError> {
        // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-nil-check
        if new_owner_id.is_nil() {
            return Err(DomainError::validation(
                "new_owner_id",
                "must not be the nil UUID",
            ));
        }
        // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-nil-check

        // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-authz
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-authz

        let now = OffsetDateTime::now_utc();
        let tenant_id = file.tenant_id;
        let old_owner_id = file.owner_id;
        let new_owner_kind_str = new_owner_kind.as_str().to_owned();

        // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-build-audit-event
        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::TransferOwnership,
            serde_json::json!({
                "from_owner_kind": file.owner_kind.as_str(),
                "from_owner_id": old_owner_id,
                "to_owner_kind": new_owner_kind_str,
                "to_owner_id": new_owner_id,
            }),
        );

        // @cpt-cf-file-storage-fr-file-events
        let event = Some(Self::make_file_event(
            tenant_id,
            new_owner_id,
            file_id,
            "file.owner_transferred",
            serde_json::json!({
                "from_owner_kind": file.owner_kind.as_str(),
                "from_owner_id": old_owner_id,
                "to_owner_kind": new_owner_kind_str,
                "to_owner_id": new_owner_id,
            }),
        ));
        // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-build-audit-event

        // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-atomic-update
        let updated = self
            .store
            .transfer_ownership_atomic(
                &scope,
                file_id,
                &new_owner_kind_str,
                new_owner_id,
                now,
                audit,
                event,
            )
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-atomic-update

        // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-not-found
        if !updated {
            return Err(DomainError::file_not_found(file_id));
        }
        // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-not-found

        // @cpt-cf-file-storage-fr-usage-reporting
        // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-usage-rebalance
        // Debit old owner, credit new owner. Bytes are unchanged.
        // @cpt-begin:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-sum
        let total_bytes: i64 = self
            .store
            .list_versions(file_id)
            .await?
            .iter()
            .filter(|v| v.status == file_storage_sdk::VersionStatus::Available)
            .map(|v| v.size)
            .sum();
        // @cpt-end:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-sum
        // @cpt-begin:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-debit
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id: old_owner_id,
            bytes_delta: -total_bytes,
            file_count_delta: -1,
        });
        // @cpt-end:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-debit
        // @cpt-begin:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-credit
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id: new_owner_id,
            bytes_delta: total_bytes,
            file_count_delta: 1,
        });
        // @cpt-end:cpt-cf-file-storage-algo-ownership-transfer-usage-rebalance:p1:inst-rebalance-credit
        // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-usage-rebalance

        // Return the file reflecting the just-committed owner swap, built
        // from the pre-transfer row plus the exact fields
        // `transfer_ownership_atomic`/`FileRepo::update_owner` mutate
        // (`owner_kind`, `owner_id`, `last_modified_at` — see that repo
        // method: `meta_version` is untouched by a transfer), rather than
        // re-reading via `require_file` with the pre-transfer `scope`. The
        // `files` entity declares `owner_col = "owner_id"`
        // (`infra/storage/entity/file.rs`), so an owner-constrained
        // `AccessScope` — one that only matches the *old* owner — would no
        // longer match the row after a successful swap, and a re-read with
        // that stale scope would incorrectly surface a 404 for a transfer
        // that already committed (usage deltas and all).
        let mut updated_file = file;
        updated_file.owner_kind = new_owner_kind;
        updated_file.owner_id = new_owner_id;
        updated_file.last_modified_at = now;
        Ok(updated_file)
    }

    /// Record an uploaded version's size+hash and mark it available, authorized
    /// by the sidecar's signed upload token rather than a user `SecurityContext`.
    ///
    /// This is the token-authenticated variant of [`finalize_upload`]. The
    /// control plane minted the token at presign time, so verifying it here
    /// constitutes full authorization — no separate user re-auth is needed
    /// (DESIGN §bind-service "Trusts a sidecar-reported size/hash (the upload
    /// URL was control-signed)").
    ///
    /// The `claims` have already been verified by the caller (signature + expiry
    /// + `op == Put` + `file_id`/`version_id` match).
    ///
    /// This method performs the same defense-in-depth policy size check as the
    /// user-facing path.
    ///
    /// The actor in the audit row is recorded as `"sidecar"` with the `Uuid::nil`
    /// actor id, since no user identity is present in a sidecar callback.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    #[tracing::instrument(skip_all)]
    /// Returns the auto-bind outcome (upload-flow redesign): `bound` is
    /// `true` only when the token carried `bind_on_finalize` and the
    /// `content_id IS NULL` CAS won (first content of a brand-new file);
    /// `etag` is the resulting content ETag in that case. The sidecar echoes
    /// both to the uploading client as `X-FS-Bound`/`ETag` response headers.
    pub async fn finalize_upload_by_token(
        &self,
        claims: &Claims,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<FinalizeByTokenOutcome, DomainError> {
        if size < 0 {
            return Err(DomainError::validation("size", "must be non-negative"));
        }

        let file_id = claims.file_id;
        let version_id = claims.version_id;

        // Fetch file via allow_all scope: the data plane operates on a
        // (file_id, version_id) pair already minted by the control plane.
        let file = self
            .store
            .require_file(&AccessScope::allow_all(), file_id)
            .await?;

        // Defense-in-depth size check: re-enforce the policy size ceiling at
        // finalization time even though the sidecar already checked the upload
        // constraint in the signed URL.
        // @cpt-cf-file-storage-fr-size-limits-policy
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;

        // Idempotent PUT-retry convergence (upload-flow redesign; the
        // dominant single-part case): the sidecar retries a `PUT` whose
        // response was lost, `publish_exclusive` refuses the second write
        // (replay-safe), and this finalize arrives for an ALREADY-available
        // version. Converge to the same success the original got — never a
        // 409 on an honest retry — but only when the retry reported the same
        // bytes (size+hash match what was verified and stored); a mismatched
        // replay stays rejected.
        if version.status == file_storage_sdk::VersionStatus::Available
            && claims.bind_on_finalize
        {
            if version.size != size || version.hash_value != hash_value {
                return Err(DomainError::hash_mismatch(
                    hex::encode(&hash_value),
                    hex::encode(&version.hash_value),
                ));
            }
            // Bind already decided by the original finalize's atomic CAS —
            // report the current state, run no new CAS.
            return Ok(if file.content_id == Some(version_id) {
                FinalizeByTokenOutcome {
                    bind_state: Some(crate::domain::multipart::BindState::Bound),
                    etag: Some(etag::content_etag(file_id, version_id)),
                    current_etag: None,
                }
            } else {
                FinalizeByTokenOutcome {
                    bind_state: Some(crate::domain::multipart::BindState::Conflict),
                    etag: None,
                    current_etag: etag::etag_for(&file),
                }
            });
        }

        let version_mime = version.mime_type.clone();
        let backend_id = version.backend_id.clone();
        let policy = self
            .get_effective_policy_internal(file.tenant_id, file.owner_id)
            .await?;
        let backend = if backend_id.is_empty() {
            self.backends.default_backend()
        } else {
            self.backends.get(&backend_id)?
        };
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &version_mime,
            backend.capabilities().max_size_bytes,
        );
        // @cpt-begin:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-size-compare
        if let Some(limit) = effective_max
            && size > 0
            && size.cast_unsigned() > limit
        {
            // @cpt-end:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-size-compare
            // @cpt-begin:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-return
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
            // @cpt-end:cpt-cf-file-storage-algo-enforce-policy-at-upload:p1:inst-enforce-return
        }

        // Never trust the caller's claimed size/hash: stream the blob
        // actually present at the version's backend path and recompute both
        // from the real bytes, never buffering more than one chunk (plus a
        // small MIME-sniff prefix) in memory regardless of object size. A
        // finalize with no prior successful PUT (no object at that path) or a
        // forged size/hash claim is rejected here rather than silently
        // persisted.
        let (actual_size, actual_hash, mime_sniff_prefix) =
            read_back_and_hash_streaming(backend.as_ref(), &version.backend_path).await?;
        if actual_size != size {
            return Err(DomainError::validation(
                "size",
                "claimed size does not match the uploaded content",
            ));
        }
        if actual_hash != hash_value {
            return Err(DomainError::hash_mismatch(
                hex::encode(&hash_value),
                hex::encode(&actual_hash),
            ));
        }

        // Declared MIME type is never trustworthy either: validate the
        // read-back blob's real bytes against `version_mime` (reusing the
        // same magic-byte sniffing the in-process data plane runs at
        // ingress), rejecting a mismatch before anything is finalized. Only
        // the leading `MIME_SNIFF_PREFIX_BYTES` are needed — see that
        // constant's doc comment for why that is always sufficient. The
        // returned type is the sniffed/canonical one when the bytes carry a
        // recognizable signature, otherwise the declared type unchanged.
        // @cpt-cf-file-storage-fr-content-type-validation
        let validated_mime = validate_and_resolve_mime(&version_mime, &mime_sniff_prefix)?;
        enforce_size_ceiling_for_validated_mime(
            &policy,
            &version_mime,
            &validated_mime,
            backend.capabilities().max_size_bytes,
            actual_size,
        )?;

        // @cpt-cf-file-storage-fr-audit-trail
        // Actor is "sidecar" with nil UUID — no user identity is available in
        // a token-authenticated callback.
        let audit = AuditEntry::success(
            file.tenant_id,
            "sidecar",
            Uuid::nil(),
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": version_id, "size": size }),
        );

        // Upload-flow redesign: a `bind_on_finalize` token additionally binds
        // this version as the file's FIRST content, in the same transaction,
        // under a strict `content_id IS NULL` CAS. Minted only by the
        // new-file `create_file` path with `bind: "auto"` — this is a
        // deliberate, narrow extension of the signed-URL delegated-authz
        // model (DESIGN §3.6 amendment): the pointer swap happens under the
        // authority of the token minted at create time, bounded by its `exp`,
        // and can never replace existing content (a non-NULL pointer loses
        // the CAS and the version simply stays available + unbound).
        let auto_bind = claims.bind_on_finalize.then(|| AutoBindOnFinalize {
            expected_content_id: None,
            audit: AuditEntry::success(
                file.tenant_id,
                "sidecar",
                Uuid::nil(),
                Some(file_id),
                AuditOperation::PatchContent,
                serde_json::json!({ "version_id": version_id, "auto_bind": true }),
            ),
            event: Some(Self::make_file_event(
                file.tenant_id,
                file.owner_id,
                file_id,
                "file.content_updated",
                serde_json::json!({ "version_id": version_id, "auto_bind": true }),
            )),
        });
        let bind_attempted = auto_bind.is_some();

        // Persist the read-back-derived size and the verified hash, not the
        // caller's size claim. `validated_mime` is persisted in place of the
        // client's original declaration.
        let outcome = self
            .store
            .finalize_version(
                file_id,
                version_id,
                actual_size,
                actual_hash,
                // Single-part upload → always whole-object SHA-256 (ADR-0006
                // mode 1). No part count, no offset-manifest row.
                crate::infra::content::hash_mode::HashMode::WholeSha256,
                None,
                None,
                Some(validated_mime),
                audit,
                auto_bind,
            )
            .await?;
        if !outcome.updated {
            // Distinguish "already finalized" (409, using the `version`
            // snapshot read earlier in this call) from "row is gone" (404).
            return Err(
                if version.status == file_storage_sdk::VersionStatus::Available {
                    DomainError::conflict("version already finalized")
                } else {
                    DomainError::version_not_found(file_id, version_id)
                },
            );
        }

        // @cpt-cf-file-storage-fr-usage-reporting
        // Same byte-crediting complement as `finalize_upload` (see its
        // comment) for the sidecar-callback / token-authenticated path.
        self.report_usage(UsageDelta {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            bytes_delta: actual_size,
            file_count_delta: 0,
        });

        self.metrics
            .record_operation("finalize_upload_by_token", "ok");
        if !bind_attempted {
            return Ok(FinalizeByTokenOutcome {
                bind_state: None,
                etag: None,
                current_etag: None,
            });
        }
        Ok(if outcome.bound {
            FinalizeByTokenOutcome {
                bind_state: Some(crate::domain::multipart::BindState::Bound),
                etag: Some(etag::content_etag(file_id, version_id)),
                current_etag: None,
            }
        } else {
            // Lost the `content_id IS NULL` CAS (e.g. two create-tokens on
            // the same new file — the first finalize bound). Report the
            // pointer that won so the client can resolve with a manual bind.
            let fresh = self
                .store
                .require_file(&AccessScope::allow_all(), file_id)
                .await?;
            FinalizeByTokenOutcome {
                bind_state: Some(crate::domain::multipart::BindState::Conflict),
                etag: None,
                current_etag: etag::etag_for(&fresh),
            }
        })
    }

    /// Delete a backend blob, logging (not failing) on error. A failed delete
    /// degrades to an orphan reconciled by the P2 cleanup engine.
    pub(super) async fn best_effort_blob_delete(&self, backend_id: &str, path: &str) {
        let Ok(backend) = self.backends.get(backend_id) else {
            return;
        };
        if let Err(err) = backend.delete(path).await {
            self.metrics.record_backend_error(backend_id, "delete");
            tracing::warn!(?err, path, "best-effort backend delete failed");
        }
    }
}

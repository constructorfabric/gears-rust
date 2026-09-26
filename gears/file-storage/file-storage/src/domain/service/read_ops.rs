//! Read-only queries (file, metadata, versions) and version-lifecycle operations
//! (download URL issuance, version listing, restore, and deletion).

use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, OwnerFilter};

use crate::domain::audit::AuditOperation;
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::ports::DeleteVersionOutcome;
use crate::domain::service::{DownloadTicket, FileService};
use crate::infra::external_clients::UsageDelta;

impl FileService {
    // ── reads ─────────────────────────────────────────────────────────────────

    /// Get a file's metadata.
    pub async fn get_file(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<File, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let scope = self
            .authorizer
            .authorize(ctx, actions::READ, &file.gts_file_type, Some(file_id))
            .await?;
        self.store.require_file(&scope, file_id).await
    }

    /// Get a file plus its custom metadata.
    pub async fn get_file_with_metadata(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(File, Vec<CustomMetadataEntry>), DomainError> {
        let file = self.get_file(ctx, file_id).await?;
        let meta = self.store.list_metadata(file_id).await?;
        Ok((file, meta))
    }

    /// Batched custom-metadata lookup for `list_files`'s page of results:
    /// one `IN (...)` query for every file id in `file_ids` instead of one
    /// `list_metadata` call per file (`GET /files` was previously built with
    /// an empty `custom_metadata` on every item -- see `handlers::list_files`).
    ///
    /// Takes raw file ids with **no `SecurityContext`/authorization check of
    /// its own** -- it trusts the caller to have already authorized every id
    /// in `file_ids` (e.g. via a preceding `list_files` call, which
    /// tenant-scopes and owner-gates its results before returning them). It
    /// is `pub(crate)`, not `pub`, precisely to keep its only intended caller
    /// (`handlers::list_files`, same crate) from becoming a foot-gun for a
    /// future caller reachable from outside this crate that might pass
    /// unauthorized ids straight through to an unscoped batched DB read.
    /// Do **not** call this with file ids that have not already come out of
    /// an authorized listing for the current `SecurityContext`.
    pub(crate) async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        self.store.list_metadata_for_files(file_ids).await
    }

    /// List files for a mandatory owner filter, offset-paginated.
    pub async fn list_files(
        &self,
        ctx: &SecurityContext,
        owner: OwnerFilter,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Vec<File>, DomainError> {
        // Authorize (access gate), then always tenant-scope the query so the
        // tenant boundary holds regardless of the PDP's returned constraints.
        self.authorizer
            .authorize(ctx, actions::READ, "", None)
            .await?;
        // Ownership gate: the coarse READ check above is resource-less (see
        // module docs) — it only answers "may this subject read files at
        // all," not "whose files." `owner` is attacker-controlled (built from
        // the request query), so without this gate any tenant member could
        // enumerate another subject's file listing via
        // `?owner_kind=user&owner_id=<victim>`. A caller listing their own
        // files (the `(owner_kind, owner_id)` pair matching the caller's own
        // kind and id) proceeds unconditionally; any other owner requires
        // `ADMIN_POLICY` — on `Forbidden` this propagates via `?` instead of
        // listing.
        //
        // Checking `owner_id` alone is not enough: `owner_kind` picks between
        // two *disjoint* owner spaces (`OwnerKind::User` / `OwnerKind::App`),
        // so a caller whose own subject id happens to equal some app's id
        // could pass `owner_kind=app&owner_id=<self>` and have the
        // `owner_id == ctx.subject_id()` comparison alone pass while actually
        // listing the *app* owner space's files instead of their own. Mirrors
        // the symmetric write-side guard `create.rs::create_file`/
        // `create_file_bare` already apply via `Self::actor_kind(ctx)`.
        if owner.owner_id != ctx.subject_id() || owner.owner_kind.as_str() != Self::actor_kind(ctx)
        {
            self.authorizer
                .authorize(ctx, actions::ADMIN_POLICY, "", None)
                .await?;
        }
        let limit = limit
            .unwrap_or(self.cfg.default_page_size)
            .min(self.cfg.max_page_size);
        self.store
            .list_files(&Self::tenant_scope(ctx), owner, limit, offset)
            .await
    }

    /// `GET /files`, plus every file's custom metadata: the same listing
    /// [`Self::list_files`] does, then a single batched
    /// [`Self::list_metadata_for_files`] lookup for the whole page instead of
    /// one `list_metadata` call per file. Extracted here (rather than left
    /// inline in the REST handler) so both callers — the REST handler and the
    /// SDK local client — attach metadata the exact same way, mirroring
    /// [`Self::list_versions_with_manifests`] for the versions listing.
    pub async fn list_files_with_metadata(
        &self,
        ctx: &SecurityContext,
        owner: OwnerFilter,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Vec<(File, Vec<CustomMetadataEntry>)>, DomainError> {
        let files = self.list_files(ctx, owner, limit, offset).await?;
        let file_ids: Vec<Uuid> = files.iter().map(|f| f.file_id).collect();
        let mut metadata_by_file = self.list_metadata_for_files(&file_ids).await?;
        Ok(files
            .into_iter()
            .map(|f| {
                let meta = metadata_by_file.remove(&f.file_id).unwrap_or_default();
                (f, meta)
            })
            .collect())
    }

    /// Authorize `GET /storages` and `GET /storages/{id}` (backend
    /// discovery). Both handlers previously extracted no `SecurityContext`
    /// at all and called the synchronous, authz-free
    /// `FileService::list_backends`/`get_backend` directly — any
    /// authenticated subject of any tenant could enumerate backend ids and
    /// capabilities. Those two methods stay synchronous and unauthorized
    /// (their `AccessScope`-free signature is used elsewhere too, see
    /// `domain/service/backend.rs`); this is a small async wrapper the
    /// handlers call first, purely to gate the read behind the same
    /// resource-less `READ` check the rest of this gear's coarse read paths
    /// use (e.g. `list_files`, `list_retention_rules`).
    pub async fn authorize_backends_read(&self, ctx: &SecurityContext) -> Result<(), DomainError> {
        self.authorizer
            .authorize(ctx, actions::READ, "", None)
            .await
            .map(|_| ())
    }

    // ── download + versioning ─────────────────────────────────────────────────

    /// `GET /files/{id}/download-url`: issue a signed download URL pinned to the
    /// current content (or a specific `version_id`).
    #[tracing::instrument(skip_all)]
    pub async fn download_url(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Option<Uuid>,
    ) -> Result<DownloadTicket, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::READ, &file.gts_file_type, Some(file_id))
            .await?;

        let target = match version_id {
            Some(v) => v,
            None => file
                .content_id
                .ok_or_else(|| DomainError::conflict("file has no bound content yet"))?,
        };
        let version = self
            .store
            .get_version(file_id, target)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, target))?;

        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::conflict(
                "cannot issue a download URL for a version whose upload has not been finalized",
            ));
        }

        // P2 1.11: the content ETag is computed once and threaded both into
        // the GET token's claims (so the sidecar can echo it as a real
        // `ETag` header with no DB lookup) and into the ticket returned here
        // — one source of truth (`etag::content_etag`).
        let content_etag = etag::content_etag(file_id, target);
        let download_url = self.build_download_url(
            file_id,
            target,
            version.backend_id,
            version.backend_path,
            Some((version.mime_type, content_etag.clone())),
        )?;
        self.metrics.record_operation("download_url", "ok");
        Ok(DownloadTicket {
            download_url,
            etag: content_etag,
            version_id: target,
        })
    }

    /// `GET /files/{id}/versions`: list a page of a file's versions, newest
    /// first, offset-paginated and capped at `ServiceConfig::max_page_size`
    /// (P2 2.2 — closes the unbounded-listing amplification surface).
    pub async fn list_versions(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::READ, &file.gts_file_type, Some(file_id))
            .await?;
        let limit = limit
            .unwrap_or(self.cfg.default_page_size)
            .min(self.cfg.max_page_size);
        self.store.list_versions_page(file_id, limit, offset).await
    }

    /// Batched manifest lookup for `list_versions`'s page of results: one
    /// `IN (...)` query for every version id in `version_ids` instead of one
    /// `get_version_manifest` call per version. Only
    /// `multipart-composite-sha256` versions have a manifest row, so
    /// `whole-sha256` version ids are simply absent from the map.
    ///
    /// Takes raw version ids with **no `SecurityContext`/authorization check
    /// of its own** — it trusts the caller to have already authorized every
    /// id in `version_ids` (e.g. via a preceding `list_versions` call, which
    /// tenant-scopes and `read`-authorizes the file before returning its
    /// versions). It is `pub(crate)`, not `pub`, for exactly the same
    /// foot-gun-avoidance reason as [`Self::list_metadata_for_files`] above.
    pub(crate) async fn manifests_for_versions(
        &self,
        version_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>, DomainError> {
        self.store.get_version_manifests(version_ids).await
    }

    /// `GET /files/{id}/versions`, plus the ADR-0006 offset-manifest for
    /// every `multipart-composite-sha256` version on the page: the same
    /// listing [`Self::list_versions`] does, batch-fetching (and, per
    /// [`fetch_manifests_within_budget`], budget-truncating) the manifests
    /// the REST handler and the SDK local client both need to attach.
    /// Extracted here (rather than left inline in the REST handler) so both
    /// callers apply the exact same manifest-byte budget and truncation —
    /// see [`fetch_manifests_within_budget`]'s doc for the full contract.
    pub async fn list_versions_with_manifests(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<Vec<(FileVersion, Option<String>)>, DomainError> {
        let mut versions = self.list_versions(ctx, file_id, limit, offset).await?;
        let mut manifests = self
            .fetch_manifests_within_budget(&mut versions, LIST_VERSIONS_MANIFEST_BUDGET_BYTES)
            .await?;
        Ok(versions
            .into_iter()
            .map(|v| {
                let manifest = manifests.remove(&v.version_id);
                (v, manifest)
            })
            .collect())
    }

    /// Fetch every multipart-composite version's manifest for one
    /// `list_versions` page, in page order and in
    /// [`MANIFEST_FETCH_BATCH_SIZE`]-sized batches, stopping (and truncating
    /// `versions` in place via [`manifest_budget_cutoff`]) the moment the
    /// running manifest byte total would cross `budget_bytes` -- so a page
    /// whose budget is exhausted at the k-th version never fetches manifests
    /// beyond (at most one batch past) that cutoff. Fetching every composite
    /// version's manifest on the page unconditionally in one query, then
    /// truncating the response after the fact, would pay for whatever
    /// [`manifest_budget_cutoff`] was about to throw away.
    ///
    /// After each batch, [`manifest_budget_cutoff`] runs only over the *safe
    /// prefix* of `versions` whose composite manifests are now all known
    /// (`composite_positions[..fetched]`'s last entry, plus any trailing
    /// whole-sha256 versions that consume no budget either way) -- never over
    /// the whole page, which would otherwise treat a composite version whose
    /// manifest hasn't been fetched *yet* the same as one with no manifest row
    /// at all (free, no budget consumed) and silently admit it.
    async fn fetch_manifests_within_budget(
        &self,
        versions: &mut Vec<FileVersion>,
        budget_bytes: u64,
    ) -> Result<std::collections::HashMap<Uuid, String>, DomainError> {
        // Every composite version's id and its position within `versions`, in
        // page order.
        let mut composite_ids: Vec<Uuid> = Vec::new();
        let mut composite_positions: Vec<usize> = Vec::new();
        for (i, v) in versions.iter().enumerate() {
            if v.hash_mode == crate::infra::content::hash_mode::HashMode::MULTIPART_COMPOSITE_SHA256
            {
                composite_ids.push(v.version_id);
                composite_positions.push(i);
            }
        }

        let mut manifests: std::collections::HashMap<Uuid, String> =
            std::collections::HashMap::new();
        let mut fetched = 0usize;

        for chunk in composite_ids.chunks(MANIFEST_FETCH_BATCH_SIZE) {
            let batch = self.manifests_for_versions(chunk).await?;
            manifests.extend(batch);
            fetched += chunk.len();

            let safe_upto = composite_positions
                .get(fetched)
                .copied()
                .unwrap_or(versions.len());
            let cutoff = manifest_budget_cutoff(&versions[..safe_upto], &manifests, budget_bytes);
            if cutoff < safe_upto {
                versions.truncate(cutoff);
                return Ok(manifests);
            }
            if safe_upto == versions.len() {
                // Reached the end of the page without ever exceeding budget.
                return Ok(manifests);
            }
        }
        Ok(manifests)
    }

    /// Restore a prior version as current (a rebind: pointer swap, no re-upload).
    pub async fn restore_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<file_storage_sdk::File, DomainError> {
        let file = self.get_file(ctx, file_id).await?;
        let if_match = etag::etag_for(&file);
        self.bind(ctx, file_id, version_id, if_match.as_deref())
            .await
    }

    // ── delete ──────────────────────────────────────────────────────────────────

    /// `DELETE /files/{id}`: remove the file and all versions (FK cascade) under
    /// an `If-Match` content-ETag precondition, then best-effort delete the
    /// backend blobs. `If-Match` is **required** (see api.md §DELETE); pass `"*"`
    /// to delete unconditionally when the ETag is unknown.
    #[tracing::instrument(skip_all)]
    pub async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::DELETE, &file.gts_file_type, Some(file_id))
            .await?;

        // Validate the If-Match precondition against the current content ETag.
        let current_etag = etag::etag_for(&file);
        match if_match {
            None => {
                return Err(DomainError::precondition_failed(
                    "If-Match is required to delete a file",
                ));
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

        self.delete_file_inner(ctx, file_id).await?;
        self.metrics.record_operation("delete_file", "ok");
        Ok(())
    }

    /// Inner (unconditional) file deletion: authorization and If-Match must have
    /// already been checked by the caller. Removes the DB row (and FK children
    /// via cascade), then best-effort-deletes all backend blobs.
    ///
    /// The version list used for the audit/event `version_count` and for
    /// backend-blob cleanup is collected by
    /// [`crate::infra::storage::Store::delete_file_collecting_versions`]
    /// **inside the same transaction** as the delete itself -- not by a
    /// separate call before it, the way this used to work. A version
    /// inserted by a concurrent `presign_version`/`initiate_multipart_upload`
    /// on this file, any time between an earlier read and this delete's
    /// commit, would otherwise be cascade-removed without ever being queued
    /// for cleanup -- a permanent backend-storage leak the cleanup engine
    /// can never detect (it only ever looks at rows still in the database).
    /// See that method's doc comment for the full mechanism.
    pub(super) async fn delete_file_inner(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(), DomainError> {
        // Authorization has already been verified by callers; use allow_all() for
        // the DB scope — the tenant boundary was enforced by require_file() above.
        let scope = AccessScope::allow_all();

        // We need the file's tenant/owner for the event payload; fetch before deletion.
        let file_meta = self.store.get_file(&scope, file_id).await?;
        let (event_tenant, event_owner) = file_meta.as_ref().map_or_else(
            || (ctx.subject_tenant_id(), Uuid::nil()),
            |f| (f.tenant_id, f.owner_id),
        );

        // `version_count` is a placeholder here -- the true count isn't known
        // until the delete transaction re-lists the versions itself; the
        // store patches this same key with the real count before persisting
        // either row (see `delete_file_collecting_versions`'s doc comment).
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::DeleteFile,
            serde_json::json!({ "version_count": 0 }),
        );
        let event = Some(Self::make_file_event(
            event_tenant,
            event_owner,
            file_id,
            "file.deleted",
            serde_json::json!({ "version_count": 0 }),
        ));

        let deleted = self
            .store
            .delete_file_collecting_versions(&scope, file_id, audit, event)
            .await?;
        if !deleted.removed {
            return Err(DomainError::file_not_found(file_id));
        }

        let total_bytes: i64 = deleted.versions.iter().map(|v| v.size).sum();
        self.report_usage(UsageDelta {
            tenant_id: event_tenant,
            owner_id: event_owner,
            bytes_delta: -total_bytes,
            file_count_delta: -1,
        });

        // Best-effort backend cleanup; a failure degrades to an orphan (P2 GC).
        for v in deleted.versions {
            self.best_effort_blob_delete(&v.backend_id, &v.backend_path)
                .await;
        }
        Ok(())
    }

    /// Delete a single version (and its backend blob). Deleting the only version
    /// is equivalent to deleting the file.
    ///
    /// The "is `version_id` the file's only version?" decision -- which of
    /// the two very different deletes actually runs -- is made **inside**
    /// [`crate::infra::storage::Store::delete_version_or_whole_file`]'s own
    /// transaction, re-listing the file's versions there instead of trusting
    /// a snapshot read before this method opened any transaction. A version
    /// inserted by a concurrent `presign_version` on this file, between such
    /// a snapshot and the eventual delete, used to make a stale "only
    /// version" verdict stick: the whole file (and that brand-new,
    /// unrelated version) would be deleted even though the caller only ever
    /// asked to remove `version_id`. See that method's doc comment for the
    /// full mechanism.
    #[tracing::instrument(skip_all)]
    pub async fn delete_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::DELETE, &file.gts_file_type, Some(file_id))
            .await?;

        let version_audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::DeleteVersion,
            serde_json::json!({ "version_id": version_id }),
        );
        // Built unconditionally alongside `version_audit` -- cheap (no I/O) --
        // because which of the two this call actually persists is decided
        // transactionally, inside the store method below, not here. Mirrors
        // `delete_file_inner`'s whole-file audit/event shape exactly (this
        // branch is, semantically, that same whole-file delete): unlike
        // that method's dynamic `version_count`, this one is always exactly
        // 1 by construction -- `Store::delete_version_or_whole_file` only
        // takes the `FileRemoved` branch when `version_id` is the file's
        // sole version.
        let file_audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::DeleteFile,
            serde_json::json!({ "version_count": 1 }),
        );
        let file_event = Some(Self::make_file_event(
            file.tenant_id,
            file.owner_id,
            file_id,
            "file.deleted",
            serde_json::json!({ "version_count": 1 }),
        ));

        let outcome = self
            .store
            .delete_version_or_whole_file(
                file_id,
                version_id,
                version_audit,
                file_audit,
                file_event,
            )
            .await?;

        match outcome {
            DeleteVersionOutcome::NotFound => {
                Err(DomainError::version_not_found(file_id, version_id))
            }
            DeleteVersionOutcome::IsCurrent => Err(DomainError::conflict(
                "cannot delete the current version; bind another version first",
            )),
            DeleteVersionOutcome::FileRemoved(removed) => {
                // Whole-file debit -- mirrors `delete_file_inner`'s.
                self.report_usage(UsageDelta {
                    tenant_id: file.tenant_id,
                    owner_id: file.owner_id,
                    bytes_delta: -removed.size,
                    file_count_delta: -1,
                });
                self.best_effort_blob_delete(&removed.backend_id, &removed.backend_path)
                    .await;
                self.metrics.record_operation("delete_version", "ok");
                Ok(())
            }
            DeleteVersionOutcome::VersionRemoved(removed) => {
                // Debit this non-current version's bytes only -- the file
                // itself, and its other versions, are untouched.
                self.report_usage(UsageDelta {
                    tenant_id: file.tenant_id,
                    owner_id: file.owner_id,
                    bytes_delta: -removed.size,
                    file_count_delta: 0,
                });
                self.best_effort_blob_delete(&removed.backend_id, &removed.backend_path)
                    .await;
                self.metrics.record_operation("delete_version", "ok");
                Ok(())
            }
        }
    }
}

/// Aggregate manifest-byte budget for one [`FileService::list_versions_with_manifests`]
/// response page. Enforced *while* manifests are being fetched (see
/// [`FileService::fetch_manifests_within_budget`]), against the manifests
/// that actually make the cut -- not as an up-front cap on `?limit` itself,
/// which would shrink every page regardless of `hash_mode` (a real
/// regression: see `docs/api.md`).
const LIST_VERSIONS_MANIFEST_BUDGET_BYTES: u64 = 4 * 1024 * 1024;

/// How many composite versions' manifests to fetch per DB round trip while
/// walking a page in order (see [`FileService::fetch_manifests_within_budget`]).
/// Small enough that a page whose budget is exhausted early (say, at the 3rd
/// composite version) never pulls more than one extra batch's worth of
/// manifests past the cutoff; large enough that a page that fits entirely
/// within budget (the common case) still costs only a handful of queries,
/// not one per version.
const MANIFEST_FETCH_BATCH_SIZE: usize = 8;

/// How many of `versions` (already offset/limit-paginated in the service's
/// return order, newest first) to keep so the summed byte length of their
/// `manifests` entries never exceeds `budget_bytes`.
///
/// The first version is always kept, even if its own manifest alone exceeds
/// the budget: a single manifest is already bounded to roughly `MAX_PART_COUNT`
/// (10 000) parts (~1 MiB, see [`crate::domain::multipart::CompletedMultipartUpload`]'s
/// doc comment), so this never admits an unbounded response, and keeping it
/// unconditionally guarantees forward progress -- a client resuming right
/// after this version can never have its whole next page truncated to
/// nothing by the same oversized manifest.
///
/// `VersionDtoList` serializes as a bare JSON array (see `docs/api.md`), so
/// there is no `has_more`/`next_offset`/cursor field available to signal an
/// early truncation without changing the wire format. Continuing correctly
/// after a truncated page therefore relies on the client resuming at
/// `offset + <number of versions actually received>` rather than
/// `offset + limit` -- already the correct, general offset-pagination
/// client contract (it is exactly how a short *final* page had to be
/// handled even before this budget existed), so an early-truncated page
/// composes with it with no special case. A client that instead always
/// advances by `limit` risks skipping the remainder after a
/// budget-truncated page.
fn manifest_budget_cutoff(
    versions: &[FileVersion],
    manifests: &std::collections::HashMap<Uuid, String>,
    budget_bytes: u64,
) -> usize {
    let mut used_bytes: u64 = 0;
    for (i, v) in versions.iter().enumerate() {
        let Some(manifest) = manifests.get(&v.version_id) else {
            continue; // whole-sha256 (or no manifest found): no budget consumed
        };
        let size = manifest.len() as u64;
        if i > 0 && used_bytes.saturating_add(size) > budget_bytes {
            return i;
        }
        used_bytes = used_bytes.saturating_add(size);
    }
    versions.len()
}

#[cfg(test)]
#[path = "read_ops_tests.rs"]
mod read_ops_tests;

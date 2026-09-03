//! `POST /files` and `POST /files/{id}/versions` — file creation and upload presigning.

use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage_sdk::NewFile;

use crate::domain::audit::AuditOperation;
use crate::domain::authz::actions;
use crate::domain::error::DomainError;
use crate::domain::policy::PolicyResolver;
use crate::domain::service::{FileService, IdempotencyTicket, UploadTicket, VersionRef};
use crate::infra::external_clients::UsageDelta;
use crate::infra::signed_url::{Op, UploadConstraints};
use crate::infra::storage::store::IdempotencyInsert;

impl FileService {
    // ── policy enforcement helpers ────────────────────────────────────────────

    /// Resolve the effective policy for a given `(tenant_id, owner_id)` pair
    /// using an internal (`allow_all`) scope — callers have already been
    /// authorized for the file operation; this is a preflight check only.
    pub(super) async fn get_effective_policy_internal(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
    ) -> Result<crate::domain::policy::EffectivePolicy, DomainError> {
        use crate::domain::policy::PolicyScope;
        use toolkit_security::AccessScope;
        let scope = AccessScope::allow_all();
        let tenant_policy = self
            .store
            .get_policy(&scope, tenant_id, &PolicyScope::Tenant, None)
            .await?;
        let user_policy = self
            .store
            .get_policy(&scope, tenant_id, &PolicyScope::User, Some(owner_id))
            .await?;
        Ok(PolicyResolver::resolve(
            tenant_policy.as_ref().map(|p| &p.body),
            user_policy.as_ref().map(|p| &p.body),
        ))
    }

    /// Run a quota preflight check for `additional_bytes` of new storage.
    ///
    /// Passes `effective_max_bytes.unwrap_or(1)` as the pessimistic upper bound:
    /// if the maximum allowed size would bust the quota, we deny early.
    ///
    /// **Fail-closed**: if the quota client returns an error, the error is
    /// propagated and the request is denied. A failing quota service is safer
    /// than silently allowing unbounded storage growth.
    ///
    /// `op` labels the caller (`"create_file"` / `"presign_version"`) for the
    /// `quota_denied` metric (P2 1.8 remediation).
    pub(super) async fn check_quota(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        effective_max_bytes: Option<u64>,
        op: &str,
    ) -> Result<(), DomainError> {
        use crate::infra::external_clients::QuotaDecision;
        let Some(qc) = &self.quota_client else {
            return Ok(()); // no quota client configured — permissive
        };
        // Use the effective max as the pessimistic size estimate. If no max is
        // configured (unlimited policy), pass 1 as a token check that any new
        // storage at all is permitted.
        let additional_bytes = effective_max_bytes.unwrap_or(1);
        match qc
            .check_storage_quota(
                tenant_id,
                owner_id,
                additional_bytes,
                super::QUOTA_METRIC_NAME,
            )
            .await?
        {
            QuotaDecision::Allowed => Ok(()),
            QuotaDecision::Denied { reason } => {
                self.metrics.record_quota_denied(op);
                Err(DomainError::quota_exceeded(reason))
            }
        }
    }

    // ── create + presign ─────────────────────────────────────────────────────

    /// `POST /files`: create a file and presign the first content upload.
    /// An optional `idempotency_key` deduplicates retried requests.
    ///
    /// `auto_bind` (upload-flow redesign): when `true` the minted upload
    /// token carries `bind_on_finalize`, so the sidecar's finalize callback
    /// binds this first content itself under a `content_id IS NULL` CAS —
    /// no separate client `bind` request. Safe to scope to this path only:
    /// the file is brand-new, so the CAS can never replace existing content.
    /// The flag is not part of the idempotency `request_hash` — a replay
    /// re-mints the URL with the *current* request's flag, exactly like the
    /// policy-derived `max_size` is re-derived on replay.
    #[tracing::instrument(skip_all)]
    pub async fn create_file(
        &self,
        ctx: &SecurityContext,
        new: NewFile,
        idempotency_key: Option<String>,
        auto_bind: bool,
    ) -> Result<UploadTicket, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let owner_id = new.owner_id;
        let owner_kind_str = new.owner_kind.as_str().to_owned();

        // Canonicalize the current request into a comparable hash up front —
        // both the replay-comparison path (below) and the fresh-insert path
        // (further down) must hash the exact same encoding of the same
        // request, so this is computed exactly once.
        let initial_meta: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();
        let request_hash = crate::domain::idempotency::compute_request_hash(
            &owner_kind_str,
            owner_id,
            &new.name,
            &new.gts_file_type,
            &new.mime_type,
            &initial_meta,
        );

        // Authorize the write BEFORE consulting any stored idempotency
        // record. The idempotency lookup used to run first and return early
        // with a live signed upload URL — a caller whose WRITE grant was
        // revoked (or was never authorized) could still replay a stored
        // ticket. Every replay must now clear the caller's *current* grants,
        // exactly like a fresh request would.
        Self::validate_gts_type(&new.gts_file_type)?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &new.gts_file_type, None)
            .await?;

        // `owner_id`/`owner_kind` are caller-supplied (from the request body,
        // via `new`) and drive which owner's effective policy is resolved
        // (allowed mime types, size limits, metadata limits) and whose quota
        // is debited, both further down -- and, via `owner_id`, whose
        // listing later surfaces this file (effective policy and listing
        // both key off `owner_id` alone, see `get_effective_policy_internal`
        // above and `read_ops::list_files`). Without this check, a subject
        // holding only ordinary file `WRITE` could create a file under an
        // arbitrary `owner_id`, evaluate it against *that* owner's (possibly
        // laxer) policy instead of their own, and charge the bytes to a
        // victim's quota instead of their own.
        //
        // Checking `owner_id` alone is not enough: `owner_kind` picks
        // between two *disjoint* owner spaces (`OwnerKind::User` /
        // `OwnerKind::App`, see `file_storage_sdk::OwnerKind`), so a caller
        // whose own subject id happens to equal some app's id could pass
        // `owner_id: <self>, owner_kind: app` and have the `owner_id ==
        // ctx.subject_id()` comparison alone pass while actually authoring
        // the file into the *app* owner space -- a different effective
        // policy and a different owner's listing/quota than the caller's
        // own `user`-space identity. `Self::actor_kind(ctx)` (already used by
        // the audit-trail helpers in `service/mod.rs`) derives the caller's
        // own kind from `SecurityContext::subject_type()` the same way, so
        // comparing it against the request's `owner_kind` closes this
        // without inventing a new classification. The self-service fast path
        // (no `ADMIN_POLICY` needed) now requires the *pair* to match, not
        // just `owner_id`. Mirrors the symmetric read-side guard
        // `read_ops::list_files` already applies to a caller-supplied
        // `owner.owner_id` (require `ADMIN_POLICY` for any owner other than
        // the caller) — that guard still only compares `owner_id`, leaving
        // the same `owner_kind`-substitution gap open on the read side (not
        // fixed here: `read_ops.rs` is outside this change's scope) —
        // closing it here on the *creation* path is still worthwhile on its
        // own, since creation is what lets a caller pick which owner space a
        // new file's policy/quota is evaluated against in the first place.
        if owner_id != ctx.subject_id() || owner_kind_str != Self::actor_kind(ctx) {
            self.authorizer
                .authorize(ctx, actions::ADMIN_POLICY, "", None)
                .await?;
        }

        // Now that the caller is authorized, consult the idempotency store.
        // The stored record is bound to the subject that created it
        // (`subject_id`); a caller can never surface another caller's ticket
        // by reusing/guessing their `(owner_kind, owner_id, key)` tuple —
        // a subject mismatch is treated as `Forbidden` rather than silently
        // falling through to a fresh create (which would otherwise race the
        // still-live row on insert).
        if let Some(ref key) = idempotency_key {
            let now = OffsetDateTime::now_utc();
            if let Some(record) = self
                .store
                .get_idempotency_key(tenant_id, &owner_kind_str, owner_id, key, now)
                .await?
            {
                if record.subject_id != ctx.subject_id() {
                    return Err(DomainError::Forbidden);
                }
                // P2 remediation 2.1: a retried request with the same key but
                // a materially different body (owner, name, gts_file_type,
                // mime_type, custom_metadata) must never silently replay the
                // original ticket — that would surface a response for a
                // request the caller never actually made.
                if record.request_hash != request_hash {
                    return Err(DomainError::conflict(
                        "idempotency key reused with a different request body",
                    ));
                }

                // A replay must clear the CURRENT effective policy, not just
                // the policy in effect when the original ticket was minted —
                // otherwise a policy tightened after the original create
                // would be silently bypassed for the idempotency TTL. Mirrors
                // the same checks the fresh-create path below runs, against
                // the same (already-hash-verified-unchanged)
                // `new.mime_type`/`initial_meta`.
                let policy = self
                    .get_effective_policy_internal(tenant_id, owner_id)
                    .await?;
                PolicyResolver::check_allowed_mime(&policy, &new.mime_type)?;
                PolicyResolver::check_metadata_limits(&policy, &initial_meta)?;

                let stored: IdempotencyTicket = serde_json::from_str(&record.response_body)
                    .map_err(|_| DomainError::database("failed to deserialize idempotency body"))?;

                // The size ceiling above only *validates* the replay against
                // the current policy for allowed-mime/metadata — it must also
                // re-mint the signed upload URL under the CURRENT effective
                // `max_size`. The stored `upload_url` was signed once, at the
                // original `create_file` call, against the policy in effect
                // *then*; naively replaying that same token verbatim would let
                // a size limit tightened afterward be silently bypassed for
                // the rest of the idempotency TTL, since the sidecar only
                // enforces whatever `max_size` claim the token itself carries
                // (DESIGN §4.5). Re-mint exactly like the fresh-create path
                // below: same file/version/backend identity (the file and its
                // pending version were already created and never change on
                // replay), fresh `effective_max` from the current policy.
                let version = self
                    .store
                    .get_version(stored.file_id, stored.version_id)
                    .await?
                    .ok_or_else(|| {
                        DomainError::version_not_found(stored.file_id, stored.version_id)
                    })?;
                let backend = if version.backend_id.is_empty() {
                    self.backends.default_backend()
                } else {
                    self.backends.get(&version.backend_id)?
                };
                let effective_max = PolicyResolver::compute_effective_max_bytes(
                    &policy,
                    &version.mime_type,
                    backend.capabilities().max_size_bytes,
                );
                // Quota preflight on replay too: re-minting the upload URL
                // grants a fresh, usable storage capability, so it must clear
                // the CURRENT quota exactly like the fresh-create path below.
                // Otherwise a quota exhausted after the original create could
                // still be bypassed by replaying the idempotency key.
                self.check_quota(tenant_id, owner_id, effective_max, "create_file")
                    .await?;
                let upload_url = self.sign_url_with_bind(
                    Op::Put,
                    &VersionRef {
                        file_id: stored.file_id,
                        version_id: stored.version_id,
                        backend_id: version.backend_id,
                        backend_path: version.backend_path,
                    },
                    UploadConstraints {
                        max_size: effective_max,
                        ..UploadConstraints::default()
                    },
                    None,
                    auto_bind,
                )?;
                let ticket = UploadTicket {
                    file_id: stored.file_id,
                    version_id: stored.version_id,
                    upload_url,
                };
                self.metrics.record_operation("create_file", "replayed");
                return Ok(ticket);
            }
        }

        let policy = self
            .get_effective_policy_internal(tenant_id, owner_id)
            .await?;

        // Validate allowed mime types.
        PolicyResolver::check_allowed_mime(&policy, &new.mime_type)?;

        // Compute effective size ceiling and validate initial metadata.
        let backend = self.backends.default_backend();
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &new.mime_type,
            backend.capabilities().max_size_bytes,
        );

        // Validate initial custom metadata against limits (`initial_meta` was
        // already collected above, for `request_hash`).
        PolicyResolver::check_metadata_limits(&policy, &initial_meta)?;

        // Quota preflight — pessimistic: check whether max allowed size fits quota.
        self.check_quota(tenant_id, owner_id, effective_max, "create_file")
            .await?;

        let now = OffsetDateTime::now_utc();
        let file_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let backend_id = backend.id().to_owned();
        let backend_path = Self::backend_path(file_id, version_id);

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::Create,
            serde_json::json!({ "version_id": version_id, "gts_file_type": new.gts_file_type }),
        );

        let event = Some(Self::make_file_event(
            tenant_id,
            owner_id,
            file_id,
            "file.created",
            serde_json::json!({ "version_id": version_id, "gts_file_type": new.gts_file_type }),
        ));

        // Sign the upload URL up front — `sign_url` has no DB dependency, so the
        // ticket (and the idempotency replay body derived from it) can be built
        // before the create transaction and persisted atomically within it.
        let upload_url = self.sign_url_with_bind(
            Op::Put,
            &VersionRef {
                file_id,
                version_id,
                backend_id: backend_id.clone(),
                backend_path: backend_path.clone(),
            },
            UploadConstraints {
                max_size: effective_max,
                ..UploadConstraints::default()
            },
            None,
            auto_bind,
        )?;
        let ticket = UploadTicket {
            file_id,
            version_id,
            upload_url,
        };

        // Build the idempotency row so the create transaction persists it in the
        // same commit as the file — a committed create always leaves a replay
        // record behind, so a retry with the same key never creates a 2nd file.
        let idempotency = idempotency_key.as_ref().map(|key| {
            let response_body = serde_json::to_string(&IdempotencyTicket {
                file_id: ticket.file_id,
                version_id: ticket.version_id,
                upload_url: ticket.upload_url.clone(),
            })
            .unwrap_or_default();
            let expires_at = now
                + time::Duration::seconds(
                    i64::try_from(self.cfg.idempotency_ttl_secs).unwrap_or(86400),
                );
            IdempotencyInsert {
                tenant_id,
                owner_kind: owner_kind_str.clone(),
                owner_id,
                key: key.clone(),
                subject_id: ctx.subject_id(),
                response_status: 201,
                response_body,
                response_etag: String::new(),
                request_hash: request_hash.clone(),
                expires_at,
            }
        });

        self.store
            .create_file_with_pending_version_and_event(
                &new,
                file_id,
                version_id,
                tenant_id,
                &backend_id,
                &backend_path,
                now,
                audit,
                event,
                idempotency,
            )
            .await?;

        // Fire-and-forget: report +1 file to usage collector.
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id,
            bytes_delta: 0, // bytes unknown at creation; finalize_upload updates the backend
            file_count_delta: 1,
        });

        self.metrics.record_operation("create_file", "ok");
        Ok(ticket)
    }

    /// Create the file row only — no pending version, no presigned URL
    /// (upload-flow redesign). Used by the merged `POST /files` create+plan
    /// path: the multipart initiate that follows registers its own pending
    /// version, so pre-registering a single-part one here would only mint the
    /// orphan the redesign exists to remove. Runs the same authz + policy +
    /// quota gates as [`Self::create_file`]. Returns the new `file_id`.
    #[tracing::instrument(skip_all)]
    pub async fn create_file_bare(
        &self,
        ctx: &SecurityContext,
        new: NewFile,
    ) -> Result<Uuid, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let owner_id = new.owner_id;

        Self::validate_gts_type(&new.gts_file_type)?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &new.gts_file_type, None)
            .await?;

        // Same cross-owner guard as `create_file` above (see its comment for
        // the full rationale, including why `owner_id` alone is not enough
        // and `owner_kind` must match too): `owner_id`/`owner_kind` are
        // caller-supplied and drive both policy resolution and quota
        // debiting below, so creating under a foreign owner -- including the
        // same `owner_id` under the *other* `owner_kind`'s disjoint owner
        // space -- requires `ADMIN_POLICY`, symmetric with the cross-owner
        // guard `read_ops::list_files` applies on the read side.
        if owner_id != ctx.subject_id() || new.owner_kind.as_str() != Self::actor_kind(ctx) {
            self.authorizer
                .authorize(ctx, actions::ADMIN_POLICY, "", None)
                .await?;
        }

        // Same policy + quota gates as `create_file`.
        let policy = self
            .get_effective_policy_internal(tenant_id, owner_id)
            .await?;
        PolicyResolver::check_allowed_mime(&policy, &new.mime_type)?;
        let backend = self.backends.default_backend();
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &new.mime_type,
            backend.capabilities().max_size_bytes,
        );
        let initial_meta: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();
        PolicyResolver::check_metadata_limits(&policy, &initial_meta)?;
        self.check_quota(tenant_id, owner_id, effective_max, "create_file")
            .await?;

        let now = OffsetDateTime::now_utc();
        let file_id = Uuid::now_v7();

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::Create,
            serde_json::json!({ "gts_file_type": new.gts_file_type }),
        );
        let event = Some(Self::make_file_event(
            tenant_id,
            owner_id,
            file_id,
            "file.created",
            serde_json::json!({ "gts_file_type": new.gts_file_type }),
        ));

        self.store
            .create_file_with_event(&new, file_id, tenant_id, now, audit, event)
            .await?;

        self.report_usage(UsageDelta {
            tenant_id,
            owner_id,
            bytes_delta: 0,
            file_count_delta: 1,
        });

        self.metrics.record_operation("create_file", "ok");
        Ok(file_id)
    }

    /// Compensating action for the merged `POST /files` create+plan path:
    /// [`Self::create_file_bare`] commits a version-less `files` row
    /// *before* the multipart plan is initiated;
    /// if initiation then fails (a capability rejection, a backend-side
    /// error), nothing else ever triggers the orphan-parent reclamation
    /// path (`CleanupEngine`'s sweep only reaches
    /// `delete_orphan_file_with_event` as a side effect of reclaiming an
    /// abandoned *pending version*, and none was ever created here — the
    /// multipart initiate registers its own version, which also never
    /// happened). Call this immediately after an initiate failure, with the
    /// `file_id` `create_file_bare` just returned, to delete the orphan
    /// synchronously instead of leaving it as a permanent, unreclaimed row.
    ///
    /// Reuses [`crate::infra::storage::store::Store::delete_orphan_file_with_event`]
    /// — the same transactionally-guarded primitive the background sweep
    /// uses — rather than an unconditional delete: it re-verifies, inside
    /// the same transaction as the delete, that the file still has zero
    /// versions and a `NULL` `content_id` before removing it. This makes
    /// the call safe to make unconditionally here even though in practice
    /// nothing else can have touched `file_id` yet (it was never returned
    /// to any caller before this compensation runs).
    ///
    /// Best-effort: a failure here (including the guard correctly declining
    /// to delete, e.g. because something unexpected already gave this file
    /// a version) is logged, never propagated — the caller must still
    /// surface the *original* initiate error, and a file this compensation
    /// fails to remove is still eventually reclaimed by the background
    /// sweep once it ages past `orphan_grace_secs`, which backstops this
    /// compensation rather than being the only reclaim path.
    pub async fn compensate_failed_multipart_initiate(&self, ctx: &SecurityContext, file_id: Uuid) {
        use toolkit_security::AccessScope;
        let scope = AccessScope::allow_all();
        let file = match self.store.get_file(&scope, file_id).await {
            Ok(Some(f)) => f,
            Ok(None) => return, // already gone somehow -- nothing to compensate
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "failed to load file for multipart-initiate compensation"
                );
                return;
            }
        };
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::OrphanReconcile,
            serde_json::json!({ "reason": "multipart_initiate_failed" }),
        );
        let event = Some(Self::make_file_event(
            file.tenant_id,
            file.owner_id,
            file_id,
            "file.deleted",
            serde_json::json!({ "reason": "multipart_initiate_failed" }),
        ));
        match self
            .store
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
        {
            Ok(true) => {
                // Credit back the +1 create_file_bare reported -- this file
                // never got any bytes, so bytes_delta stays 0.
                self.report_usage(UsageDelta {
                    tenant_id: file.tenant_id,
                    owner_id: file.owner_id,
                    bytes_delta: 0,
                    file_count_delta: -1,
                });
            }
            Ok(false) => {
                // Guard declined (a version now exists / content is bound --
                // should not be reachable in this call's actual use, but the
                // guard is what makes calling this unconditionally safe) or
                // a concurrent sweep already removed it. Either way, fine.
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "failed to compensate orphan file after multipart-initiate failure -- \
                     the background sweep will reclaim it once it ages past orphan_grace_secs"
                );
            }
        }
    }

    /// `POST /files/{id}/versions`: presign a new content version on an existing
    /// file (the upload's bytes will be bound via `bind`).
    pub async fn presign_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<UploadTicket, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // Reuse the current version's mime as the declared type placeholder.
        let mime_type = self
            .store
            .current_version_mime(&file)
            .await?
            .unwrap_or_else(|| "application/octet-stream".to_owned());

        let tenant_id = ctx.subject_tenant_id();
        let owner_id = file.owner_id;
        let policy = self
            .get_effective_policy_internal(tenant_id, owner_id)
            .await?;

        PolicyResolver::check_allowed_mime(&policy, &mime_type)?;

        let backend = self.backends.default_backend();
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &mime_type,
            backend.capabilities().max_size_bytes,
        );

        self.check_quota(tenant_id, owner_id, effective_max, "presign_version")
            .await?;

        let now = OffsetDateTime::now_utc();
        let version_id = Uuid::now_v7();
        let backend_id = backend.id().to_owned();
        let backend_path = Self::backend_path(file_id, version_id);

        self.store
            .insert_pending_version(
                file_id,
                version_id,
                &mime_type,
                &backend_id,
                &backend_path,
                now,
            )
            .await?;

        let upload_url = self.sign_url(
            Op::Put,
            &VersionRef {
                file_id,
                version_id,
                backend_id,
                backend_path,
            },
            UploadConstraints {
                max_size: effective_max,
                ..UploadConstraints::default()
            },
            None,
        )?;
        Ok(UploadTicket {
            file_id,
            version_id,
            upload_url,
        })
    }
}

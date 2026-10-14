//! `POST /files` and `POST /files/{id}/versions` — file creation and upload presigning.

use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
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

/// Conflict message for an idempotency replay that differs from the stored ticket (in
/// `request_hash` or `bind`).
const IDEMPOTENCY_BODY_MISMATCH: &str = "idempotency key reused with a different request body";

impl FileService {
    /// Effective policy for `(tenant_id, owner_id)` under an `allow_all` scope; callers
    /// are already authorized for the file operation.
    pub(super) async fn get_effective_policy_internal(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
    ) -> Result<crate::domain::policy::EffectivePolicy, DomainError> {
        use crate::domain::policy::PolicyScope;
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

    /// Quota preflight using `effective_max_bytes.unwrap_or(1)` as a pessimistic size.
    ///
    /// Fail-closed: a quota client error denies the request. `op` labels the
    /// `quota_denied` metric.
    pub(super) async fn check_quota(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        effective_max_bytes: Option<u64>,
        op: &str,
    ) -> Result<(), DomainError> {
        use crate::infra::external_clients::QuotaDecision;
        let Some(qc) = &self.quota_client else {
            return Ok(()); // no quota client configured
        };
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

    /// `POST /files`: create a file and presign the first content upload.
    /// An optional `idempotency_key` deduplicates retried requests.
    ///
    /// `auto_bind` mints a `bind_on_finalize` claim; it is excluded from `request_hash`, so a replay
    /// with a different mode is a conflict.
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

        // Computed once: the replay comparison and the fresh insert must use the same hash.
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

        // Authorize BEFORE consulting the idempotency record, so a replay (which returns a
        // live signed URL) always clears the caller's current grants.
        Self::validate_gts_type(&new.gts_file_type)?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &new.gts_file_type, None)
            .await?;

        // Stored record is bound to its creating subject; a mismatch is `Forbidden`, not a fresh create.
        if let Some(ref key) = idempotency_key
            && let Some(ticket) = self
                .replay_idempotency_key(
                    ctx,
                    tenant_id,
                    &owner_kind_str,
                    owner_id,
                    key,
                    &request_hash,
                    &new,
                    &initial_meta,
                    auto_bind,
                )
                .await?
        {
            return Ok(ticket);
        }

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

        PolicyResolver::check_metadata_limits(&policy, &initial_meta)?;

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

        // `sign_url` has no DB dependency, so the ticket and the idempotency replay body
        // are built first and persisted atomically in the create transaction.
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

        // Persisted in the same commit as the file, so a retry never creates a second file.
        let idempotency = idempotency_key.as_ref().map(|key| {
            let response_body = serde_json::to_string(&IdempotencyTicket {
                file_id: ticket.file_id,
                version_id: ticket.version_id,
                upload_url: ticket.upload_url.clone(),
                auto_bind,
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

        // Fire-and-forget usage report.
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id,
            bytes_delta: 0, // bytes unknown at creation; finalize_upload updates the backend
            file_count_delta: 1,
        });

        self.metrics.record_operation("create_file", "ok");
        Ok(ticket)
    }

    /// [`Self::create_file`]'s idempotency-replay path (split out for clippy's line-count limit).
    /// `Ok(None)` means no live record; any other failure must propagate, never fall through to a create.
    #[allow(clippy::too_many_arguments)]
    async fn replay_idempotency_key(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        owner_kind_str: &str,
        owner_id: Uuid,
        key: &str,
        request_hash: &[u8],
        new: &NewFile,
        initial_meta: &[(String, String)],
        auto_bind: bool,
    ) -> Result<Option<UploadTicket>, DomainError> {
        let now = OffsetDateTime::now_utc();
        let Some(record) = self
            .store
            .get_idempotency_key(tenant_id, owner_kind_str, owner_id, key, now)
            .await?
        else {
            return Ok(None);
        };

        if record.subject_id != ctx.subject_id() {
            return Err(DomainError::Forbidden);
        }
        if record.request_hash != request_hash {
            return Err(DomainError::conflict(IDEMPOTENCY_BODY_MISMATCH));
        }

        let stored: IdempotencyTicket = serde_json::from_str(&record.response_body)
            .map_err(|_| DomainError::database("failed to deserialize idempotency body"))?;

        if stored.auto_bind != auto_bind {
            return Err(DomainError::conflict(IDEMPOTENCY_BODY_MISMATCH));
        }

        // The lookup is keyed by the request's owner and `transfer_ownership` never updates a stored
        // ticket: re-read the live owner so a stale replay cannot keep minting write capability.
        let live_file = self
            .store
            .require_file(&AccessScope::allow_all(), stored.file_id)
            .await?;
        if live_file.owner_kind.as_str() != owner_kind_str || live_file.owner_id != owner_id {
            return Err(DomainError::conflict(
                "idempotency key's file has changed owner since it was created",
            ));
        }

        let policy = self
            .get_effective_policy_internal(tenant_id, owner_id)
            .await?;
        PolicyResolver::check_allowed_mime(&policy, &new.mime_type)?;
        PolicyResolver::check_metadata_limits(&policy, initial_meta)?;

        // Re-mint under the CURRENT `max_size`: the sidecar enforces only the token's claim.
        let version = self
            .store
            .get_version(stored.file_id, stored.version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(stored.file_id, stored.version_id))?;
        // Never re-mint once the version is no longer pending: that would grant write capability against existing content.
        if version.status != file_storage_sdk::VersionStatus::Pending {
            return Err(DomainError::conflict(
                "idempotency key's target version is no longer pending",
            ));
        }
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
            stored.auto_bind,
        )?;
        self.metrics.record_operation("create_file", "replayed");
        Ok(Some(UploadTicket {
            file_id: stored.file_id,
            version_id: stored.version_id,
            upload_url,
        }))
    }

    /// Create the file row only (no pending version, no URL) for the merged `POST /files` path;
    /// same gates as [`Self::create_file`]. Returns the new `file_id`.
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

    /// Compensating action for the merged path: deletes the version-less file row
    /// [`Self::create_file_bare`] committed when multipart initiate fails. Best-effort (logged);
    /// leftovers are reclaimed by `CleanupEngine::sweep_versionless_files`.
    pub async fn compensate_failed_multipart_initiate(&self, ctx: &SecurityContext, file_id: Uuid) {
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
                self.report_usage(UsageDelta {
                    tenant_id: file.tenant_id,
                    owner_id: file.owner_id,
                    bytes_delta: 0,
                    file_count_delta: -1,
                });
            }
            Ok(false) => {
                // Guard declined (a version now exists) or a sweep already removed it.
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "failed to compensate orphan file after multipart-initiate failure -- \
                     CleanupEngine's sweep_versionless_files phase will reclaim it once it \
                     ages past orphan_grace_secs"
                );
            }
        }
    }

    /// `POST /files/{id}/versions`: presign a new content version (bound later via `bind`).
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

        // The current version's mime stands in as the declared type.
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

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
    ///
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    /// @cpt-cf-file-storage-fr-size-limits-policy
    /// @cpt-cf-file-storage-fr-metadata-limits
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
    /// @cpt-cf-file-storage-fr-storage-quota
    pub(super) async fn check_quota(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        effective_max_bytes: Option<u64>,
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
            QuotaDecision::Denied { reason } => Err(DomainError::quota_exceeded(reason)),
        }
    }

    // ── create + presign ─────────────────────────────────────────────────────

    /// `POST /files`: create a file and presign the first content upload.
    /// An optional `idempotency_key` deduplicates retried requests.
    ///
    /// @cpt-cf-file-storage-fr-upload-idempotency
    /// @cpt-cf-file-storage-fr-audit-trail
    pub async fn create_file(
        &self,
        ctx: &SecurityContext,
        new: NewFile,
        idempotency_key: Option<String>,
    ) -> Result<UploadTicket, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let owner_id = new.owner_id;
        let owner_kind_str = new.owner_kind.as_str().to_owned();

        // @cpt-cf-file-storage-fr-upload-idempotency
        // Check for an existing idempotency record before doing any work.
        if let Some(ref key) = idempotency_key {
            let now = OffsetDateTime::now_utc();
            if let Some(record) = self
                .store
                .get_idempotency_key(tenant_id, &owner_kind_str, owner_id, key, now)
                .await?
            {
                let ticket: UploadTicket =
                    serde_json::from_str::<IdempotencyTicket>(&record.response_body)
                        .map(Into::into)
                        .map_err(|_| {
                            DomainError::database("failed to deserialize idempotency body")
                        })?;
                return Ok(ticket);
            }
        }

        Self::validate_gts_type(&new.gts_file_type)?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &new.gts_file_type, None)
            .await?;

        // @cpt-cf-file-storage-fr-allowed-types-policy
        // @cpt-cf-file-storage-fr-size-limits-policy
        // @cpt-cf-file-storage-fr-metadata-limits
        // @cpt-cf-file-storage-fr-storage-quota
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

        // Validate initial custom metadata against limits.
        let initial_meta: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();
        PolicyResolver::check_metadata_limits(&policy, &initial_meta)?;

        // Quota preflight — pessimistic: check whether max allowed size fits quota.
        self.check_quota(tenant_id, owner_id, effective_max).await?;

        let now = OffsetDateTime::now_utc();
        let file_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let backend_id = backend.id().to_owned();
        let backend_path = Self::backend_path(file_id, version_id);

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::Create,
            serde_json::json!({ "version_id": version_id, "gts_file_type": new.gts_file_type }),
        );

        // @cpt-cf-file-storage-fr-file-events
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
        let upload_url = self.sign_url(
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
        )?;
        let ticket = UploadTicket {
            file_id,
            version_id,
            upload_url,
        };

        // @cpt-cf-file-storage-fr-upload-idempotency
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
                response_status: 201,
                response_body,
                response_etag: String::new(),
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

        // @cpt-cf-file-storage-fr-usage-reporting
        // Fire-and-forget: report +1 file to usage collector.
        self.report_usage(UsageDelta {
            tenant_id,
            owner_id,
            bytes_delta: 0, // bytes unknown at creation; finalize_upload updates the backend
            file_count_delta: 1,
        });

        Ok(ticket)
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

        // @cpt-cf-file-storage-fr-allowed-types-policy
        // @cpt-cf-file-storage-fr-size-limits-policy
        // @cpt-cf-file-storage-fr-storage-quota
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

        self.check_quota(tenant_id, owner_id, effective_max).await?;

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
        )?;
        Ok(UploadTicket {
            file_id,
            version_id,
            upload_url,
        })
    }
}

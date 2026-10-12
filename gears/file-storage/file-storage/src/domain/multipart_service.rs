//! `MultipartService` — multipart upload control-plane logic: initiate (server-authoritative
//! plan with per-part signed sidecar URLs), report part, introspect, complete and abort.
//!
//! Part bytes flow only to the sidecar via those signed URLs. The service holds its own
//! copies of its dependencies rather than referencing `FileService`, to keep coupling low.

// Domain terms (ETag, If-Match, FileStorage, GET/PUT, BLAKE3) appear in the docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation};
use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::{
    CompletedMultipartUpload, DEFAULT_MIN_PART_SIZE, MAX_PART_SIZE, MissingPart, MultipartPart,
    MultipartPartPlan, MultipartPlan, MultipartUploadSession, MultipartUploadState,
    MultipartUploadStatus, ReceivedPart, compute_plan,
};
use crate::domain::policy::{PolicyResolver, PolicyScope};
use crate::domain::ports::{FileStorageMetricsPort, MultipartStore};
use crate::infra::backend::BackendRegistry;
use crate::infra::content::mime::{
    MIME_SNIFF_PREFIX_BYTES, enforce_size_ceiling_for_validated_mime, validate_and_resolve_mime,
};
use crate::infra::external_clients::{QuotaClient, QuotaDecision, UsageDelta, UsageReporter};
use crate::infra::metrics::NoopMetrics;
use crate::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage_sdk::ByteRange;

/// Quota metric name (same platform metric as in `service.rs`).
const QUOTA_METRIC_NAME: &str = gts_id!("cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1");

/// Diff the plan's expected part numbers against the reported parts, returning the missing
/// ones in ascending order.
///
/// `expected_count` mirrors [`compute_plan`], including `declared_size == 0` (one zero-byte
/// part, never reported as missing).
pub(crate) fn missing_part_numbers(
    session: &MultipartUploadSession,
    parts: &[MultipartPart],
) -> Vec<u32> {
    let expected_count = if session.declared_size == 0 {
        1
    } else {
        session.declared_size.div_ceil(session.part_size.max(1))
    };
    let reported: std::collections::HashSet<u32> = parts.iter().map(|p| p.part_number).collect();
    (1..=expected_count)
        .filter_map(|n| u32::try_from(n).ok())
        .filter(|n| !reported.contains(n))
        .collect()
}

/// Recompute one part's `(offset, size)` from the session's `(declared_size, part_size)`,
/// matching [`compute_plan`] (`declared_size == 0` is a single zero-byte part).
///
/// Saturating arithmetic guards against a corrupted session row; callers only pass numbers
/// returned by `missing_part_numbers`.
pub(crate) fn part_bounds(session: &MultipartUploadSession, part_number: u32) -> (u64, u64) {
    if session.declared_size == 0 {
        return (0, 0);
    }
    let part_size = session.part_size.max(1);
    let offset = u64::from(part_number.saturating_sub(1)).saturating_mul(part_size);
    let size = part_size.min(session.declared_size.saturating_sub(offset));
    (offset, size)
}

/// The multipart-upload service; wired alongside `FileService` in `gear.rs` under the same
/// REST prefix.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct MultipartService {
    store: Arc<dyn MultipartStore>,
    backends: BackendRegistry,
    authorizer: Arc<dyn Authorizer>,
    quota_client: Option<Arc<dyn QuotaClient>>,
    /// Signed-URL issuer for minting per-part sidecar tokens.
    issuer: Arc<Issuer>,
    /// Base URL of the sidecar (e.g. `"http://sidecar.example.com"`).
    sidecar_base_url: String,
    /// Signed-URL TTL in seconds (shared with the session expiry).
    url_ttl_secs: i64,
    /// Metrics port; no-op by default, see [`Self::with_metrics`].
    metrics: Arc<dyn FileStorageMetricsPort>,
    /// Usage-reporting sink; `None` disables reporting.
    usage_reporter: Option<Arc<dyn UsageReporter>>,
}

impl MultipartService {
    pub fn new(
        store: Arc<dyn MultipartStore>,
        backends: BackendRegistry,
        authorizer: Arc<dyn Authorizer>,
        quota_client: Option<Arc<dyn QuotaClient>>,
        issuer: Arc<Issuer>,
        sidecar_base_url: String,
        url_ttl_secs: i64,
    ) -> Self {
        Self {
            store,
            backends,
            authorizer,
            quota_client,
            issuer,
            sidecar_base_url,
            url_ttl_secs,
            metrics: Arc::new(NoopMetrics),
            usage_reporter: None,
        }
    }

    /// Install a real metrics port (builder step, so `new()` call sites stay unchanged).
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn FileStorageMetricsPort>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Install a usage-reporting sink.
    #[must_use]
    pub fn with_usage_reporter(mut self, usage_reporter: Option<Arc<dyn UsageReporter>>) -> Self {
        self.usage_reporter = usage_reporter;
        self
    }

    /// Fire-and-forget usage delta report; a failing reporter never blocks operations.
    fn report_usage(&self, delta: UsageDelta) {
        if let Some(reporter) = self.usage_reporter.clone() {
            tokio::spawn(async move {
                reporter.report(delta).await;
            });
        }
    }

    fn tenant_scope(ctx: &SecurityContext) -> AccessScope {
        AccessScope::for_tenant(ctx.subject_tenant_id())
    }

    fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
        crate::domain::storage_layout::backend_path(file_id, version_id)
    }

    fn actor_kind(ctx: &SecurityContext) -> &'static str {
        match ctx.subject_type() {
            Some("app") => "app",
            _ => "user",
        }
    }

    fn audit_ok(
        ctx: &SecurityContext,
        file_id: Option<Uuid>,
        operation: AuditOperation,
        detail: serde_json::Value,
    ) -> AuditEntry {
        AuditEntry::success(
            ctx.subject_tenant_id(),
            Self::actor_kind(ctx),
            ctx.subject_id(),
            file_id,
            operation,
            detail,
        )
    }

    /// Resolve the effective policy for a given `(tenant_id, owner_id)` pair.
    async fn get_effective_policy_internal(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
    ) -> Result<crate::domain::policy::EffectivePolicy, DomainError> {
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

    /// Quota preflight for `additional_bytes` of new storage (the declared total size at
    /// initiate). **Fail-closed**: a failing quota client denies the request.
    async fn check_quota_bytes(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        additional_bytes: u64,
    ) -> Result<(), DomainError> {
        let Some(qc) = &self.quota_client else {
            return Ok(());
        };
        match qc
            .check_storage_quota(tenant_id, owner_id, additional_bytes, QUOTA_METRIC_NAME)
            .await?
        {
            QuotaDecision::Allowed => Ok(()),
            QuotaDecision::Denied { reason } => {
                self.metrics
                    .record_quota_denied("initiate_multipart_upload");
                Err(DomainError::quota_exceeded(reason))
            }
        }
    }

    /// Best-effort compensation when session persistence fails after the backend handle and
    /// pending version row were created: abort the handle and delete the row. Errors are
    /// logged, the caller's original error is returned, and leftovers are reclaimed by the
    /// cleanup sweep.
    async fn compensate_failed_session_create(
        &self,
        ctx: &SecurityContext,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_path: &str,
        backend_handle: &str,
    ) {
        let backend = self.backends.default_backend();
        if let Err(abort_err) = backend.abort_multipart(backend_path, backend_handle).await {
            self.metrics
                .record_backend_error(backend.id(), "abort_multipart");
            tracing::warn!(
                ?abort_err,
                %upload_id,
                "best-effort backend abort failed after session persistence error"
            );
        }
        if let Err(del_err) = self
            .store
            .delete_version(
                file_id,
                version_id,
                Self::audit_ok(
                    ctx,
                    Some(file_id),
                    AuditOperation::DeleteVersion,
                    serde_json::json!({
                        "version_id": version_id,
                        "reason": "multipart_session_create_failed"
                    }),
                ),
            )
            .await
        {
            tracing::warn!(
                ?del_err,
                %upload_id,
                "best-effort pending-version delete failed after session persistence error"
            );
        }
    }

    /// Mint one signed per-part upload URL. `exp` is chosen by the caller: a fresh full TTL
    /// at initiate, the session's remaining `expires_at` when resuming.
    #[allow(clippy::too_many_arguments)]
    fn mint_part_url(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        backend_id: &str,
        backend_path: &str,
        upload_id: Uuid,
        backend_handle: &str,
        part_number: u32,
        offset: u64,
        size: u64,
        exp: i64,
        request_id: &str,
        now: OffsetDateTime,
    ) -> Result<String, DomainError> {
        let claims = Claims {
            op: Op::MultipartPart,
            file_id,
            version_id,
            backend_id: backend_id.to_owned(),
            backend_path: backend_path.to_owned(),
            exp,
            upload: UploadConstraints::default(),
            multipart: MultipartClaims {
                upload_id,
                part_number,
                offset,
                size,
                backend_handle: backend_handle.to_owned(),
            },
            request_id: request_id.to_owned(),
            // `content_type`/`etag` are GET-only claims.
            content_type: String::new(),
            etag: String::new(),
        };
        let token = self.issuer.issue(claims, now)?;
        Ok(format!(
            "{}/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/{part_number}?fs-token={token}",
            self.sidecar_base_url
        ))
    }

    /// `POST /files/{id}/multipart`: validate, pre-register a `pending` version, create the
    /// backend session and return the exact parts plan with one signed sidecar URL per part.
    ///
    /// Gates: MIME not allowed `415`, declared size over the effective max `413`, storage
    /// quota `507`. The complete-time total-size check remains as defence-in-depth.
    #[tracing::instrument(skip_all)]
    pub async fn initiate_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        preferred_part_size: Option<u64>,
        _concurrency: Option<u32>,
    ) -> Result<MultipartPlan, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let backend = self.backends.default_backend();
        if !backend.capabilities().multipart_native {
            return Err(DomainError::multipart_not_supported(backend.id()));
        }

        // Reject (rather than clamp) an out-of-range hint before `compute_plan`: a huge value
        // risks arithmetic overflow or a huge allocation.
        if let Some(preferred) = preferred_part_size
            && !(DEFAULT_MIN_PART_SIZE..=MAX_PART_SIZE).contains(&preferred)
        {
            return Err(DomainError::validation(
                "preferred_part_size",
                format!(
                    "must be between {DEFAULT_MIN_PART_SIZE} and {MAX_PART_SIZE} bytes \
                     (got {preferred})"
                ),
            ));
        }

        // Policy gates against the declared total size.
        let tenant_id = ctx.subject_tenant_id();
        let policy = self
            .get_effective_policy_internal(tenant_id, file.owner_id)
            .await?;
        PolicyResolver::check_allowed_mime(&policy, declared_mime)?;
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            declared_mime,
            backend.capabilities().max_size_bytes,
        );

        // Reject up front if the declared size exceeds the effective limit.
        if let Some(limit) = effective_max
            && declared_size > limit
        {
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
        }

        // Quota check against the declared size, not the pessimistic `effective_max`.
        self.check_quota_bytes(tenant_id, file.owner_id, declared_size)
            .await?;

        let now = OffsetDateTime::now_utc();
        let upload_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let backend_path = Self::backend_path(file_id, version_id);
        let backend_id = backend.id().to_owned();

        // `BackendCapabilities` exposes no minimum part size, so `DEFAULT_MIN_PART_SIZE` applies.
        let (chosen_part_size, raw_parts) = compute_plan(declared_size, preferred_part_size, None)?;

        self.store
            .insert_pending_version(
                file_id,
                version_id,
                declared_mime,
                &backend_id,
                &backend_path,
                now,
            )
            .await?;

        let backend_handle = backend.initiate_multipart(&backend_path).await?;

        // One TTL for both the session row and the signed URLs.
        let expires_at = now + time::Duration::seconds(self.url_ttl_secs.max(1));

        // On failure, compensate so the backend handle and pending row are not orphaned.
        if let Err(err) = self
            .store
            .create_multipart_upload(
                upload_id,
                file_id,
                version_id,
                &backend_handle,
                declared_mime,
                declared_size,
                chosen_part_size,
                expires_at,
                now,
            )
            .await
        {
            self.compensate_failed_session_create(
                ctx,
                upload_id,
                file_id,
                version_id,
                &backend_path,
                &backend_handle,
            )
            .await;
            return Err(err);
        }

        // One signed URL per part, each carrying the exact `size` claim the sidecar enforces.
        // All parts share one request id so the sidecar's report-part callbacks correlate.
        let exp = expires_at.unix_timestamp();
        let request_id = Uuid::now_v7().to_string();
        let mut parts = Vec::with_capacity(raw_parts.len());
        for (part_number, offset, size) in raw_parts {
            let upload_url = self.mint_part_url(
                file_id,
                version_id,
                &backend_id,
                &backend_path,
                upload_id,
                &backend_handle,
                part_number,
                offset,
                size,
                exp,
                &request_id,
                now,
            )?;
            parts.push(MultipartPartPlan {
                part_number,
                offset,
                size,
                upload_url,
            });
        }

        self.metrics
            .record_operation("initiate_multipart_upload", "ok");
        Ok(MultipartPlan {
            upload_id,
            version_id,
            part_hash_algorithm: "SHA-256".to_owned(),
            part_size: chosen_part_size,
            parts,
            expires_at,
        })
    }

    /// Token-authenticated sidecar callback recording a successfully written part.
    ///
    /// `claims` are already verified by the caller (including `op == MultipartPart`); this
    /// re-validates them against the session so a token for a different or finished session
    /// cannot touch another upload's part list. A caller-supplied `size` differing from
    /// `claims.multipart.size` is rejected, so a token holder cannot forge a part size and
    /// corrupt the version size summed at complete.
    pub async fn report_part(
        &self,
        claims: &Claims,
        backend_etag: String,
        hash_value: Vec<u8>,
        size: i64,
    ) -> Result<(), DomainError> {
        let upload_id = claims.multipart.upload_id;
        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // A foreign session is reported as "not found", indistinguishable from a missing one.
        if session.file_id != claims.file_id || session.version_id != claims.version_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        if session.state != MultipartUploadState::InProgress {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        let part_number = i32::try_from(claims.multipart.part_number)
            .map_err(|_| DomainError::validation("part_number", "part_number overflows i32"))?;

        // The callback is anonymous and token-authenticated, so the reported `size` must equal
        // the planned per-part size in the token; the claimed value is what gets persisted.
        let claimed_size = i64::try_from(claims.multipart.size)
            .map_err(|_| DomainError::validation("size", "size overflows i64"))?;
        if size != claimed_size {
            return Err(DomainError::validation(
                "size",
                "reported part size does not match the planned size for this part",
            ));
        }

        self.store
            .upsert_multipart_part(
                upload_id,
                part_number,
                &backend_etag,
                hash_value,
                claimed_size,
                OffsetDateTime::now_utc(),
            )
            .await
    }

    /// `POST /files/{id}/multipart/{upload_id}/complete`: assemble all parts and finalize.
    #[tracing::instrument(skip_all)]
    pub async fn complete_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // Optional `If-Match`: unlike `bind`, omission stays unconditional because `complete`
        // is keyed by `upload_id`. `*` matches always; otherwise the file's content ETag.
        if let Some(m) = if_match {
            let m = m.trim();
            if m != "*" {
                let current_etag = etag::etag_for(&file);
                if Some(m) != current_etag.as_deref() {
                    return Err(DomainError::precondition_failed(
                        "If-Match does not match the current content ETag",
                    ));
                }
            }
        }

        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // The session is loaded by `upload_id` alone: bind it to the authorized `file_id`, and
        // report a foreign one as "not found" so it is indistinguishable from a missing one.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        if session.state != MultipartUploadState::InProgress {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // The session can still read `in_progress` after `expires_at` because nothing flips it
        // until `CleanupEngine::run_sweep` runs; reject expired sessions explicitly.
        if session.expires_at <= OffsetDateTime::now_utc() {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id, "expired",
            ));
        }

        let parts = self.store.list_multipart_parts(upload_id).await?;

        let version = self.store.get_version(file_id, session.version_id).await?;
        let backend_id = version.as_ref().map_or_else(
            || self.backends.default_id().to_owned(),
            |v| v.backend_id.clone(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = Self::backend_path(file_id, session.version_id);

        // Report the specific missing part numbers rather than an opaque size mismatch.
        let missing = missing_part_numbers(&session, &parts);
        if !missing.is_empty() {
            return Err(DomainError::multipart_parts_missing(upload_id, missing));
        }

        // Total size from the parts the sidecar reported.
        let total_size: i64 = parts.iter().map(|p| p.size).sum();

        // Defence-in-depth: the primary size enforcement is the per-part `size` claim at the
        // sidecar; this catches residual mismatches (e.g. a missing or extra part).
        if session.declared_size > 0 {
            let expected = i64::try_from(session.declared_size).unwrap_or(i64::MAX);
            if total_size != expected {
                return Err(DomainError::conflict(format!(
                    "multipart upload {upload_id}: assembled size {total_size} \
                     does not match declared_size {expected}"
                )));
            }
        }

        let policy = self
            .get_effective_policy_internal(ctx.subject_tenant_id(), file.owner_id)
            .await?;
        let effective_max = PolicyResolver::compute_effective_max_bytes(
            &policy,
            &session.declared_mime,
            backend.capabilities().max_size_bytes,
        );
        if let Some(limit) = effective_max
            && total_size > 0
            && total_size.cast_unsigned() > limit
        {
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
        }

        // Backend parts carry each part's offset (running sum of prior sizes) and SHA-256
        // digest. `parts` is ascending by `part_number` and gapless (checked above), which for
        // a valid plan is also ascending offset order, so no sort is needed.
        let mut backend_parts: Vec<(u32, u64, [u8; 32], String)> = Vec::with_capacity(parts.len());
        let mut running_offset: u64 = 0;
        for p in &parts {
            let digest: [u8; 32] = p.part_hash.clone().try_into().map_err(|_| {
                DomainError::validation(
                    "part_hash",
                    format!(
                        "part {} hash is not a 32-byte SHA-256 digest",
                        p.part_number
                    ),
                )
            })?;
            backend_parts.push((
                p.part_number,
                running_offset,
                digest,
                p.backend_etag.clone(),
            ));
            running_offset += u64::try_from(p.size).unwrap_or(0);
        }

        // The backend builds the offset manifest and its `root` from the per-part digests;
        // the assembled object is not re-read. `root` becomes the version's `hash_value` and
        // the manifest is persisted with the version row below.
        let (manifest, root) = backend
            .complete_multipart(
                &backend_path,
                &session.backend_upload_handle,
                &backend_parts,
            )
            .await?;
        let content_hash = root.to_vec();
        let manifest_text = manifest.to_wire_string();
        let part_count = i32::try_from(parts.len())
            .map_err(|_| DomainError::validation("part_count", "part count overflows i32"))?;

        // Sniff the assembled object's prefix and validate it against `session.declared_mime`,
        // so a MIME policy cannot be bypassed by declaring an allowed type and uploading other
        // bytes. Runs post-assembly because only the backend can read the whole object. An
        // empty object has nothing to sniff, so the declared type is accepted as-is.
        let mime_sniff_prefix = if total_size == 0 {
            Vec::new()
        } else {
            let sniff_len = u64::try_from(MIME_SNIFF_PREFIX_BYTES).unwrap_or(u64::MAX);
            let end = sniff_len
                .saturating_sub(1)
                .min(total_size.cast_unsigned().saturating_sub(1));
            backend
                .get_range(&backend_path, ByteRange::Inclusive { start: 0, end })
                .await?
                .to_vec()
        };
        // A mismatch fails before any DB finalize. The assembled blob is left for the cleanup
        // sweep, like the `!finalized` / `!completed` branches below: the backend object may
        // always outlive a failed finalize.
        let validated_mime = validate_and_resolve_mime(&session.declared_mime, &mime_sniff_prefix)?;
        enforce_size_ceiling_for_validated_mime(
            &policy,
            &session.declared_mime,
            &validated_mime,
            backend.capabilities().max_size_bytes,
            total_size,
        )?;

        // The `MultipartComplete` audit row below covers this finalize.
        let finalize_audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": session.version_id, "upload_id": upload_id, "size": total_size }),
        );
        let finalized = self
            .store
            .finalize_version(
                file_id,
                session.version_id,
                total_size,
                content_hash.clone(),
                crate::infra::content::hash_mode::HashMode::MultipartCompositeSha256,
                Some(part_count),
                Some(manifest_text.clone()),
                Some(validated_mime),
                finalize_audit,
            )
            .await?;
        if !finalized {
            // The pending row vanished (concurrent abort or cleanup) after assembly; fail
            // rather than report success with no bound version.
            return Err(DomainError::conflict(format!(
                "multipart upload {upload_id}: version row was removed before completion"
            )));
        }

        // Session CAS `in_progress -> completed`, plus the main audit row.
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::MultipartComplete,
            serde_json::json!({ "upload_id": upload_id, "version_id": session.version_id }),
        );
        let completed = self
            .store
            .complete_multipart_upload(upload_id, audit)
            .await?;
        if !completed {
            // A concurrent complete/abort already moved the session out of `in_progress`.
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // Credit the assembled bytes; `file_count_delta` is `0` because the file was already
        // counted at `create_file` time.
        self.report_usage(UsageDelta {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            bytes_delta: total_size,
            file_count_delta: 0,
        });

        self.metrics
            .record_operation("complete_multipart_upload", "ok");
        Ok(CompletedMultipartUpload {
            version_id: session.version_id,
            size: total_size,
            hash_algorithm: crate::infra::content::hash::ALGORITHM,
            content_hash,
            hash_mode: crate::infra::content::hash_mode::HashMode::MultipartCompositeSha256,
            part_count,
            manifest: manifest_text,
        })
    }

    /// `GET /files/{id}/multipart/{upload_id}`: session state, received parts and missing
    /// parts. Only a live (`in_progress`, unexpired) session gets resume URLs for the
    /// missing parts.
    ///
    /// Authorized on `actions::WRITE`, not `READ`, because it hands out live upload URLs.
    #[tracing::instrument(skip_all)]
    pub async fn introspect_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
    ) -> Result<MultipartUploadStatus, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // Same foreign-session masking as `complete_multipart_upload`.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        let parts = self.store.list_multipart_parts(upload_id).await?;
        let missing_numbers = missing_part_numbers(&session, &parts);

        let now = OffsetDateTime::now_utc();
        let can_resume =
            session.state == MultipartUploadState::InProgress && session.expires_at > now;

        // Skip the version lookup when no URL will be minted.
        let backend_id = if can_resume {
            let version = self.store.get_version(file_id, session.version_id).await?;
            version.map_or_else(|| self.backends.default_id().to_owned(), |v| v.backend_id)
        } else {
            String::new()
        };

        // Resume tokens expire with the session, never a fresh full TTL.
        let exp = session.expires_at.unix_timestamp();
        let request_id = Uuid::now_v7().to_string();
        let backend_path = Self::backend_path(file_id, session.version_id);

        let mut missing = Vec::with_capacity(missing_numbers.len());
        for part_number in missing_numbers {
            let (offset, size) = part_bounds(&session, part_number);
            let upload_url = if can_resume {
                Some(self.mint_part_url(
                    file_id,
                    session.version_id,
                    &backend_id,
                    &backend_path,
                    upload_id,
                    &session.backend_upload_handle,
                    part_number,
                    offset,
                    size,
                    exp,
                    &request_id,
                    now,
                )?)
            } else {
                None
            };
            missing.push(MissingPart {
                part_number,
                offset,
                size,
                upload_url,
            });
        }

        let received = parts
            .into_iter()
            .map(|p| ReceivedPart {
                part_number: p.part_number,
                size: p.size,
                uploaded_at: p.uploaded_at,
            })
            .collect();

        self.metrics
            .record_operation("introspect_multipart_upload", "ok");
        Ok(MultipartUploadStatus {
            upload_id,
            version_id: session.version_id,
            state: session.state,
            declared_mime: session.declared_mime,
            declared_size: session.declared_size,
            part_size: session.part_size,
            created_at: session.created_at,
            expires_at: session.expires_at,
            received,
            missing,
        })
    }

    /// `DELETE /files/{id}/multipart/{upload_id}`: abort a multipart upload.
    pub async fn abort_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
    ) -> Result<(), DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // Same foreign-session masking as `complete_multipart_upload`.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        if session.state != MultipartUploadState::InProgress {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        let version = self.store.get_version(file_id, session.version_id).await?;
        let backend_id = version.as_ref().map_or_else(
            || self.backends.default_id().to_owned(),
            |v| v.backend_id.clone(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = Self::backend_path(file_id, session.version_id);

        backend
            .abort_multipart(&backend_path, &session.backend_upload_handle)
            .await?;

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::MultipartAbort,
            serde_json::json!({ "upload_id": upload_id, "version_id": session.version_id }),
        );

        // Session CAS `in_progress -> aborted`.
        let aborted = self.store.abort_multipart_upload(upload_id, audit).await?;
        if !aborted {
            // Lost the race. Must not fall through to the delete below: after a concurrent
            // *complete* that version is bound and deleting it would corrupt the upload.
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // DB errors propagate; an already-missing row (`false`) is the desired end state.
        self.store
            .delete_version(
                file_id,
                session.version_id,
                Self::audit_ok(
                    ctx,
                    Some(file_id),
                    AuditOperation::DeleteVersion,
                    serde_json::json!({ "version_id": session.version_id, "reason": "multipart_abort" }),
                ),
            )
            .await?;

        Ok(())
    }
}

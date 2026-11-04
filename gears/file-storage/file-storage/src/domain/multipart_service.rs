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

use crate::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::{DomainError, MULTIPART_SESSION_RECLAIMED_BY_CLEANUP_MESSAGE};
use crate::domain::etag;
use crate::domain::multipart::{
    BindDecision, CompletedMultipartUpload, DEFAULT_MIN_PART_SIZE, MAX_PART_SIZE, MissingPart,
    MultipartCompleteOutcome, MultipartPart, MultipartPartPlan, MultipartPlan,
    MultipartUploadSession, MultipartUploadState, MultipartUploadStatus, RawPartEntry,
    ReceivedPart, StoredCompleteResult, compute_plan,
};
use crate::domain::policy::{PolicyResolver, PolicyScope};
use crate::domain::ports::{
    AutoBindOnFinalize, FileStorageMetricsPort, MultipartFinishSnapshot, MultipartStore,
};
use crate::infra::backend::{BackendRegistry, StorageBackend};
use crate::infra::content::mime::{
    MIME_SNIFF_PREFIX_BYTES, enforce_size_ceiling_for_validated_mime, validate_and_resolve_mime,
};
use crate::infra::external_clients::{QuotaClient, QuotaDecision, UsageDelta, UsageReporter};
use crate::infra::metrics::NoopMetrics;
use crate::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage_sdk::ByteRange;

/// `Retry-After` hint (seconds) on a `202 completing` answer.
const COMPLETE_POLL_RETRY_SECS: u64 = 2;

/// Quota metric name (same platform metric as in `service.rs`).
const QUOTA_METRIC_NAME: &str = gts_id!("cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1");

/// A multipart plan that passed [`MultipartService::validate_multipart_plan`]: the chosen part
/// size and the `(part_number, offset, size)` entries, ready to open a session with.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct ValidatedMultipartPlan {
    chosen_part_size: u64,
    raw_parts: Vec<RawPartEntry>,
}

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
    /// Signed-URL TTL (seconds) for per-part upload URLs; short to bound the stale-permission window.
    url_ttl_secs: i64,
    /// Session lifetime (seconds); defaults to `url_ttl_secs`, `gear.rs` sets the longer value.
    session_ttl_secs: i64,
    /// Seconds a `complete` may hold `completing` before another can take it over.
    complete_lease_secs: i64,
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
            session_ttl_secs: url_ttl_secs,
            complete_lease_secs: 120,
            metrics: Arc::new(NoopMetrics),
            usage_reporter: None,
        }
    }

    /// Install a completion-lease duration.
    #[must_use]
    pub fn with_complete_lease_secs(mut self, complete_lease_secs: i64) -> Self {
        self.complete_lease_secs = complete_lease_secs;
        self
    }

    /// Install a session lifetime decoupled from the URL TTL.
    #[must_use]
    pub fn with_session_ttl_secs(mut self, session_ttl_secs: i64) -> Self {
        self.session_ttl_secs = session_ttl_secs;
        self
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

    /// Remove a pending version that will never get a session (failed session open, user abort),
    /// and then the file itself if that left it without versions and without content (the
    /// composite-create case; a file that has other versions or a bound content stays). The file
    /// goes through the guarded in-transaction orphan delete and gets the `file.deleted` event,
    /// an audit row and usage `-1`, the same as deleting the only version of a file.
    ///
    /// `Err` only for the version delete (a DB error the caller may propagate or log); a failing
    /// file delete is logged, since the version row is already gone and the file is unusable.
    #[allow(clippy::too_many_arguments)]
    async fn discard_version_and_orphan_file(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        owner_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        reason: &str,
    ) -> Result<(), DomainError> {
        self.store
            .delete_version(
                file_id,
                version_id,
                Self::audit_ok(
                    ctx,
                    Some(file_id),
                    AuditOperation::DeleteVersion,
                    serde_json::json!({ "version_id": version_id, "reason": reason }),
                ),
            )
            .await?;

        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::OrphanReconcile,
            serde_json::json!({ "reason": reason }),
        );
        let event = Some(FileEvent {
            tenant_id,
            owner_id,
            file_id,
            event_type: "file.deleted".to_owned(),
            payload: serde_json::json!({ "reason": reason }),
        });
        match self
            .store
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
        {
            Ok(true) => self.report_usage(UsageDelta {
                tenant_id,
                owner_id,
                bytes_delta: 0,
                file_count_delta: -1,
            }),
            // The guard declined (other versions or bound content) or the file is already gone.
            Ok(false) => {}
            Err(err) => tracing::warn!(
                ?err,
                %file_id,
                "failed to delete the file left without versions; it stays until the cleanup \
                 job for version-less files exists"
            ),
        }
        Ok(())
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
            bind_on_finalize: false,
        };
        let token = self.issuer.issue(claims, now)?;
        Ok(format!(
            "{}/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/{part_number}?fs-token={token}",
            self.sidecar_base_url
        ))
    }

    /// Checks of a multipart plan that do not depend on a file row: backend `multipart_native`,
    /// the `preferred_part_size` range, the allowed MIME, the declared size against the effective
    /// limit, the plan itself and the storage quota for the declared size. Runs before the first
    /// write on both entry points. The caller authorizes the request (it knows whether a file
    /// exists yet).
    ///
    /// Gates: MIME not allowed `415`, declared size over the effective max `413`, storage
    /// quota `507`. The complete-time total-size check remains as defence-in-depth.
    pub(crate) async fn validate_multipart_plan(
        &self,
        ctx: &SecurityContext,
        owner_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        preferred_part_size: Option<u64>,
    ) -> Result<ValidatedMultipartPlan, DomainError> {
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
            .get_effective_policy_internal(tenant_id, owner_id)
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
        self.check_quota_bytes(tenant_id, owner_id, declared_size)
            .await?;

        // `BackendCapabilities` exposes no minimum part size, so `DEFAULT_MIN_PART_SIZE` applies.
        let (chosen_part_size, raw_parts) = compute_plan(declared_size, preferred_part_size, None)?;

        Ok(ValidatedMultipartPlan {
            chosen_part_size,
            raw_parts,
        })
    }

    /// `POST /files/{id}/multipart`: validate, pre-register a `pending` version, create the
    /// backend session and return the exact parts plan with one signed sidecar URL per part.
    /// See [`Self::validate_multipart_plan`] for the gates.
    ///
    /// `auto_bind`: `complete` binds the finalized version itself (only the merged `POST /files` path).
    #[tracing::instrument(skip_all)]
    #[allow(clippy::too_many_arguments)]
    pub async fn initiate_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        preferred_part_size: Option<u64>,
        auto_bind: bool,
    ) -> Result<MultipartPlan, DomainError> {
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        let validated = self
            .validate_multipart_plan(
                ctx,
                file.owner_id,
                declared_mime,
                declared_size,
                preferred_part_size,
            )
            .await?;

        let now = OffsetDateTime::now_utc();
        let version_id = Uuid::now_v7();
        let backend = self.backends.default_backend();
        self.store
            .insert_pending_version(
                file_id,
                version_id,
                declared_mime,
                backend.id(),
                &Self::backend_path(file_id, version_id),
                now,
            )
            .await?;

        self.open_session_or_discard(
            ctx,
            file.tenant_id,
            file.owner_id,
            file_id,
            version_id,
            declared_mime,
            declared_size,
            validated,
            auto_bind,
            now,
        )
        .await
    }

    /// Composite `POST /files`, second half: the file and its pending version were committed
    /// together; open the session for that version. Any failure removes the pending version and
    /// the file (see [`Self::discard_version_and_orphan_file`]), so no version-less file stays.
    #[tracing::instrument(skip_all)]
    #[allow(clippy::too_many_arguments)]
    pub async fn open_session_for_created_file(
        &self,
        ctx: &SecurityContext,
        owner_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        validated: ValidatedMultipartPlan,
        auto_bind: bool,
    ) -> Result<MultipartPlan, DomainError> {
        self.open_session_or_discard(
            ctx,
            ctx.subject_tenant_id(),
            owner_id,
            file_id,
            version_id,
            declared_mime,
            declared_size,
            validated,
            auto_bind,
            OffsetDateTime::now_utc(),
        )
        .await
    }

    /// [`Self::open_session`], discarding the pending version (and an orphaned file) on failure.
    #[allow(clippy::too_many_arguments)]
    async fn open_session_or_discard(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        owner_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        validated: ValidatedMultipartPlan,
        auto_bind: bool,
        now: OffsetDateTime,
    ) -> Result<MultipartPlan, DomainError> {
        match self
            .open_session(
                file_id,
                version_id,
                declared_mime,
                declared_size,
                validated,
                auto_bind,
                now,
            )
            .await
        {
            Ok(plan) => Ok(plan),
            Err(err) => {
                if let Err(del_err) = self
                    .discard_version_and_orphan_file(
                        ctx,
                        tenant_id,
                        owner_id,
                        file_id,
                        version_id,
                        "multipart_session_create_failed",
                    )
                    .await
                {
                    tracing::warn!(
                        ?del_err,
                        %file_id,
                        %version_id,
                        "best-effort pending-version delete failed after multipart initiate error"
                    );
                }
                Err(err)
            }
        }
    }

    /// Open the backend session for an existing `pending` version and persist it. If the session
    /// row cannot be written, the backend handle is aborted (best effort); the pending version is
    /// the caller's to discard.
    #[allow(clippy::too_many_arguments)]
    async fn open_session(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        validated: ValidatedMultipartPlan,
        auto_bind: bool,
        now: OffsetDateTime,
    ) -> Result<MultipartPlan, DomainError> {
        let ValidatedMultipartPlan {
            chosen_part_size,
            raw_parts,
        } = validated;
        let backend = self.backends.default_backend();
        let upload_id = Uuid::now_v7();
        let backend_path = Self::backend_path(file_id, version_id);
        let backend_id = backend.id().to_owned();

        let session_expires_at = now
            .checked_add(time::Duration::seconds(self.session_ttl_secs.max(1)))
            .ok_or_else(|| {
                DomainError::database(
                    "multipart session TTL overflowed computing the session expiry",
                )
            })?;
        let url_expires_at = now
            .checked_add(time::Duration::seconds(self.url_ttl_secs.max(1)))
            .ok_or_else(|| {
                DomainError::database("signed-URL TTL overflowed computing the part URL expiry")
            })?;

        let backend_handle = backend.initiate_multipart(&backend_path).await?;

        // One signed URL per part, each carrying the exact `size` claim the sidecar enforces.
        // All parts share one request id so the sidecar's report-part callbacks correlate.
        let exp = url_expires_at.unix_timestamp();
        let request_id = Uuid::now_v7().to_string();
        let mut parts = Vec::new();
        for (part_number, offset, size) in raw_parts {
            let upload_url = match self.mint_part_url(
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
            ) {
                Ok(url) => url,
                Err(err) => {
                    self.abort_fresh_handle(
                        backend.as_ref(),
                        &backend_path,
                        &backend_handle,
                        upload_id,
                    )
                    .await;
                    return Err(err);
                }
            };
            parts.push(MultipartPartPlan {
                part_number,
                offset,
                size,
                upload_url,
            });
        }

        if let Err(err) = self
            .store
            .create_multipart_upload(
                upload_id,
                file_id,
                version_id,
                &backend_handle,
                &backend_id,
                &backend_path,
                declared_mime,
                declared_size,
                chosen_part_size,
                auto_bind,
                session_expires_at,
                now,
            )
            .await
        {
            self.abort_fresh_handle(backend.as_ref(), &backend_path, &backend_handle, upload_id)
                .await;
            return Err(err);
        }

        self.metrics
            .record_operation("initiate_multipart_upload", "ok");
        Ok(MultipartPlan {
            upload_id,
            version_id,
            part_hash_algorithm: "SHA-256".to_owned(),
            part_size: chosen_part_size,
            parts,
            expires_at: url_expires_at,
        })
    }

    /// Best-effort abort of a backend handle opened by `open_session` that will not be used.
    async fn abort_fresh_handle(
        &self,
        backend: &dyn StorageBackend,
        backend_path: &str,
        backend_handle: &str,
        upload_id: Uuid,
    ) {
        if let Err(abort_err) = backend.abort_multipart(backend_path, backend_handle).await {
            self.metrics
                .record_backend_error(backend.id(), "abort_multipart");
            tracing::warn!(
                ?abort_err,
                %upload_id,
                "best-effort backend abort failed after multipart session setup error"
            );
        }
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

    /// `POST /files/{id}/multipart/{upload_id}/complete`: finalize all parts.
    ///
    /// No DB lock or transaction is held across backend assembly; the session moves through
    /// conditional-UPDATE transitions `in_progress -> completing(lease) -> completed(snapshot)`.
    /// The lease winner assembles in a detached task; losers answer `Completing` (202), and a
    /// `completed` session replays its snapshot.
    #[tracing::instrument(skip_all)]
    pub async fn complete_multipart_upload(
        self: &Arc<Self>,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<MultipartCompleteOutcome, DomainError> {
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

        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        // Replay is checked BEFORE `If-Match`: the earlier success already moved the ETag, so an honest
        // retry must get the persisted 200, not 412/409.
        if session.state == MultipartUploadState::Completed {
            return self.replayed_outcome(file_id, &session).await;
        }
        if session.state == MultipartUploadState::Aborted {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // `None` stays unconditional (keyed by `upload_id`); for `auto_bind` it is also the bind's precondition.
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

        // Fast-path expiry check; the authoritative one is the lease CAS's `expires_at > now`.
        if session.expires_at <= OffsetDateTime::now_utc() {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id, "expired",
            ));
        }

        let now = OffsetDateTime::now_utc();
        let lease_owner = Uuid::now_v7().to_string();
        let lease_until = now
            .checked_add(time::Duration::seconds(self.complete_lease_secs.max(1)))
            .ok_or_else(|| {
                DomainError::database("complete lease TTL overflowed computing the lease expiry")
            })?;
        let acquired = self
            .store
            .acquire_multipart_complete_lease(upload_id, &lease_owner, lease_until, now)
            .await?;
        if !acquired {
            let fresh = self
                .store
                .get_multipart_upload(upload_id)
                .await?
                .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;
            return match fresh.state {
                MultipartUploadState::Completed => self.replayed_outcome(file_id, &fresh).await,
                // Losing the CAS can mean the session expired since load; report that, not a 202 forever.
                MultipartUploadState::InProgress | MultipartUploadState::Completing
                    if fresh.expires_at <= now =>
                {
                    Err(DomainError::multipart_upload_not_in_progress(
                        upload_id, "expired",
                    ))
                }
                MultipartUploadState::Completing => Ok(MultipartCompleteOutcome::Completing {
                    retry_after_secs: COMPLETE_POLL_RETRY_SECS,
                }),
                _ => Err(DomainError::multipart_upload_not_in_progress(
                    upload_id,
                    fresh.state.as_str(),
                )),
            };
        }

        // Whether ANY `If-Match` was supplied (already validated), so the embedded bind CAS may target
        // the observed pointer; with none, complete must not get an unconditional bind either.
        let if_match_was_supplied = if_match.is_some();

        let svc = Arc::clone(self);
        let ctx = ctx.clone();
        let handle = tokio::spawn(async move {
            svc.assemble_and_finish(&ctx, file, session, lease_owner, if_match_was_supplied)
                .await
        });
        match handle.await {
            Ok(result) => result.map(MultipartCompleteOutcome::Completed),
            Err(join_err) => {
                tracing::error!(%upload_id, error = %join_err, "complete task panicked");
                Err(DomainError::InternalError)
            }
        }
    }

    /// `complete` on an already-`completed` session: the replayed response, counted as `replayed`.
    async fn replayed_outcome(
        &self,
        file_id: Uuid,
        session: &MultipartUploadSession,
    ) -> Result<MultipartCompleteOutcome, DomainError> {
        self.metrics
            .record_operation("complete_multipart_upload", "replayed");
        Ok(MultipartCompleteOutcome::Completed(
            self.replay_completed(file_id, session).await?,
        ))
    }

    /// Rebuild the response for a `completed` session from the persisted snapshot, falling back to
    /// the version row. The manifest is re-read after confirming the version still exists (404 otherwise).
    async fn replay_completed(
        &self,
        file_id: Uuid,
        session: &MultipartUploadSession,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        if let Some(json) = &session.complete_result
            && let Ok(stored) = serde_json::from_str::<StoredCompleteResult>(json)
        {
            if self
                .store
                .get_version(file_id, stored.version_id)
                .await?
                .is_none()
            {
                return Err(DomainError::version_not_found(file_id, stored.version_id));
            }
            let manifest = self.store.get_version_manifest(stored.version_id).await?;
            if let Some(completed) = stored.into_completed(manifest) {
                return Ok(completed);
            }
            tracing::warn!(
                upload_id = %session.upload_id,
                "multipart complete_result snapshot failed to parse or is internally \
                 inconsistent (unrecognized hash_mode/bind_state spelling, or a field \
                 combination replay_bind_state/complete_multipart_upload never actually \
                 produces); falling back to rebuilding the response from the version row"
            );
        }
        let file = self
            .store
            .require_file(&AccessScope::allow_all(), file_id)
            .await?;
        let version = self
            .store
            .get_version(file_id, session.version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, session.version_id))?;
        if version.status != file_storage_sdk::VersionStatus::Available {
            return Err(DomainError::multipart_upload_not_in_progress(
                session.upload_id,
                session.state.as_str(),
            ));
        }
        let manifest = self.store.get_version_manifest(session.version_id).await?;
        let hash_mode = crate::infra::content::hash_mode::HashMode::parse(&version.hash_mode)
            .ok_or_else(|| {
                DomainError::database(format!(
                    "invalid hash_mode in DB for version {}: {}",
                    session.version_id, version.hash_mode
                ))
            })?;
        let (bind_state, bind_etag, current_etag) = crate::domain::multipart::replay_bind_state(
            file.file_id,
            file.content_id,
            session.version_id,
            version.bound_on_finalize,
            session.auto_bind,
        );
        Ok(CompletedMultipartUpload {
            version_id: session.version_id,
            size: version.size,
            hash_algorithm: crate::infra::content::hash::ALGORITHM,
            content_hash: version.hash_value,
            hash_mode,
            part_count: version.part_count.unwrap_or(1),
            manifest,
            bind_state,
            etag: bind_etag,
            current_etag,
        })
    }

    /// The lease-holder's assembly + finish path (detached task); on error releases the lease.
    async fn assemble_and_finish(
        &self,
        ctx: &SecurityContext,
        file: file_storage_sdk::File,
        session: MultipartUploadSession,
        lease_owner: String,
        if_match_was_supplied: bool,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        let upload_id = session.upload_id;
        let result = self
            .assemble_and_finish_inner(ctx, &file, &session, &lease_owner, if_match_was_supplied)
            .await;
        if result.is_err()
            && let Err(release_err) = self
                .store
                .release_multipart_complete_lease(upload_id, &lease_owner)
                .await
        {
            tracing::warn!(%upload_id, error = %release_err, "failed to release completion lease");
        }
        result
    }

    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    async fn assemble_and_finish_inner(
        &self,
        ctx: &SecurityContext,
        file: &file_storage_sdk::File,
        session: &MultipartUploadSession,
        lease_owner: &str,
        if_match_was_supplied: bool,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        let file_id = file.file_id;
        let upload_id = session.upload_id;

        // Already-finalized fast path (a completer may have died after the finalize committed);
        // re-assembling would hit a consumed backend handle.
        if let Some(v) = self.store.get_version(file_id, session.version_id).await?
            && v.status == file_storage_sdk::VersionStatus::Available
        {
            return self
                .replay_finished_version(ctx, file_id, session, lease_owner)
                .await;
        }

        let parts = self.store.list_multipart_parts(upload_id).await?;

        let version = self.store.get_version(file_id, session.version_id).await?;
        let backend_id = session.resolve_backend_id(
            version.as_ref().map(|v| v.backend_id.as_str()),
            self.backends.default_id(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = session.backend_path.clone();

        let missing = missing_part_numbers(session, &parts);
        if !missing.is_empty() {
            return Err(DomainError::multipart_parts_missing(upload_id, missing));
        }

        let total_size: i64 = parts.iter().map(|p| p.size).sum();

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

        let mut backend_parts: Vec<(u32, u64, [u8; 32], String)> = Vec::new();
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

        let (manifest, root) = match backend
            .complete_multipart(
                &backend_path,
                &session.backend_upload_handle,
                &backend_parts,
            )
            .await
        {
            Ok(assembled) => assembled,
            Err(assemble_err) => {
                // The assembly may already have succeeded in an earlier attempt (a lost response, or
                // a failure after it that released the lease), which consumed the backend handle.
                // The path is unique per version and the part rows fix the manifest, so an existing
                // object is that assembly: derive (manifest, root) from the part rows.
                let object_exists = backend.exists(&backend_path).await.unwrap_or(false);
                if !object_exists {
                    return Err(assemble_err);
                }
                let entries = backend_parts
                    .iter()
                    .map(
                        |(_, offset, digest, _)| crate::infra::content::hash_mode::ManifestEntry {
                            offset: *offset,
                            digest: *digest,
                        },
                    )
                    .collect();
                let manifest = crate::infra::content::hash_mode::Manifest::new(entries)?;
                let root = manifest.root();
                (manifest, root)
            }
        };
        // ADR-0006: a one-part plan is `whole-sha256` with no manifest row and `part_count` NULL.
        let single_part = parts.len() == 1;
        let (hash_mode, content_hash, manifest_text) = if single_part {
            (
                crate::infra::content::hash_mode::HashMode::WholeSha256,
                backend_parts[0].2.to_vec(),
                None,
            )
        } else {
            (
                crate::infra::content::hash_mode::HashMode::MultipartCompositeSha256,
                root.to_vec(),
                Some(manifest.to_wire_string()),
            )
        };
        let part_count = i32::try_from(parts.len())
            .map_err(|_| DomainError::validation("part_count", "part count overflows i32"))?;

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
        let validated_mime = validate_and_resolve_mime(&session.declared_mime, &mime_sniff_prefix)?;
        enforce_size_ceiling_for_validated_mime(
            &policy,
            &session.declared_mime,
            &validated_mime,
            backend.capabilities().max_size_bytes,
            total_size,
        )?;

        let finalize_audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": session.version_id, "upload_id": upload_id, "size": total_size }),
        );
        // An `auto_bind` session binds inside this transaction. `file` may be stale, so with `If-Match`
        // the observed pointer is the CAS target; otherwise it requires `content_id IS NULL`. A lost CAS
        // reports `BindState::Conflict`, not an error.
        let auto_bind_target = if if_match_was_supplied {
            file.content_id
        } else {
            None
        };
        let auto_bind = session.auto_bind.then(|| AutoBindOnFinalize {
            expected_content_id: auto_bind_target,
            audit: Self::audit_ok(
                ctx,
                Some(file_id),
                AuditOperation::PatchContent,
                serde_json::json!({
                    "version_id": session.version_id,
                    "upload_id": upload_id,
                    "auto_bind": true,
                }),
            ),
            event: Some(FileEvent {
                tenant_id: file.tenant_id,
                owner_id: file.owner_id,
                file_id,
                event_type: "file.content_updated".to_owned(),
                payload: serde_json::json!({ "version_id": session.version_id }),
            }),
        });

        // `completing -> completed` is written in the SAME transaction, so a crash cannot land between.
        let session_audit = Self::multipart_complete_audit(ctx, session);
        let finalize_outcome = match self
            .store
            .finalize_multipart_version(
                file_id,
                manifest_text.clone(),
                Some(validated_mime),
                finalize_audit,
                auto_bind,
                MultipartFinishSnapshot {
                    upload_id,
                    version_id: session.version_id,
                    size: total_size,
                    content_hash: content_hash.clone(),
                    hash_mode,
                    part_count: (!single_part).then_some(part_count),
                    session_audit,
                },
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                // Cleanup reclaimed the session, so no row will reference the object; delete it (the sweep only finds rows).
                if matches!(
                    &e,
                    DomainError::Conflict { message }
                        if *message == MULTIPART_SESSION_RECLAIMED_BY_CLEANUP_MESSAGE
                ) {
                    self.best_effort_blob_delete(&backend_id, &backend_path)
                        .await;
                }
                return Err(e);
            }
        };
        let finalized = finalize_outcome.updated;
        let bound = finalize_outcome.bound;
        if !finalized {
            return self
                .converge_or_error_after_lost_finalize_cas(
                    ctx,
                    file_id,
                    session,
                    lease_owner,
                    upload_id,
                )
                .await;
        }
        // The bind decision and `current_etag` come from the finalize transaction, not a post-commit
        // read that could see a later rebind.
        let decision = if bound {
            BindDecision::Bound
        } else if session.auto_bind {
            BindDecision::Conflict {
                current_etag: finalize_outcome.current_etag,
            }
        } else {
            BindDecision::Manual
        };
        let (bind_state, bind_etag, current_etag) = decision.outcome(file_id, session.version_id);

        let result = CompletedMultipartUpload {
            version_id: session.version_id,
            size: total_size,
            hash_algorithm: crate::infra::content::hash::ALGORITHM,
            content_hash,
            hash_mode,
            part_count,
            manifest: manifest_text,
            bind_state,
            etag: bind_etag,
            current_etag,
        };

        if !finalize_outcome.session_completed {
            // Terminal CAS lost (lease taken over): close with THIS snapshot, not `replay_completed`
            // (which would re-derive `bind_state` from a possibly rebound pointer).
            self.finish_session(ctx, session, lease_owner, &result)
                .await?;
        }

        self.report_usage(UsageDelta {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            bytes_delta: total_size,
            file_count_delta: 0,
        });

        self.metrics
            .record_operation("complete_multipart_upload", "ok");
        Ok(result)
    }

    /// The version is already `Available`: a finalize committed earlier. Serve the winner's
    /// snapshot and close the session under this lease.
    async fn replay_finished_version(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        session: &MultipartUploadSession,
        lease_owner: &str,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        // The winner wrote `complete_result` in the transaction that made the version available;
        // re-read the session so the replay serves that snapshot verbatim.
        let fresh = self
            .store
            .get_multipart_upload(session.upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(session.upload_id))?;
        let completed = self.replay_completed(file_id, &fresh).await?;
        self.finish_session(ctx, session, lease_owner, &completed)
            .await?;
        Ok(completed)
    }

    /// Handle a lost finalize CAS (fenced by `status = 'pending'`, not lease ownership). If the
    /// version is `Available`, a taken-over completer won: converge by finishing the session.
    /// Otherwise the pending row is gone (abort or cleanup): a real error.
    async fn converge_or_error_after_lost_finalize_cas(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        session: &MultipartUploadSession,
        lease_owner: &str,
        upload_id: Uuid,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        let converged_version = self.store.get_version(file_id, session.version_id).await?;
        if let Some(v) = converged_version
            && v.status == file_storage_sdk::VersionStatus::Available
        {
            return self
                .replay_finished_version(ctx, file_id, session, lease_owner)
                .await;
        }
        Err(DomainError::conflict(format!(
            "multipart upload {upload_id}: version row was removed before completion"
        )))
    }

    /// The `MultipartComplete` audit row, shared by `finish_session` and the main path.
    fn multipart_complete_audit(
        ctx: &SecurityContext,
        session: &MultipartUploadSession,
    ) -> AuditEntry {
        Self::audit_ok(
            ctx,
            Some(session.file_id),
            AuditOperation::MultipartComplete,
            serde_json::json!({ "upload_id": session.upload_id, "version_id": session.version_id }),
        )
    }

    /// Terminal `completing -> completed` for the paths that find the version already finalized
    /// (replay, converge) or lose the terminal CAS inside the finalize transaction; the main path
    /// does this inside `finalize_multipart_version`.
    async fn finish_session(
        &self,
        ctx: &SecurityContext,
        session: &MultipartUploadSession,
        lease_owner: &str,
        result: &CompletedMultipartUpload,
    ) -> Result<(), DomainError> {
        let upload_id = session.upload_id;
        let audit = Self::multipart_complete_audit(ctx, session);
        let result_json = serde_json::to_string(&StoredCompleteResult::from_completed(result))
            .map_err(|_| DomainError::database("failed to serialize complete result"))?;
        let finished = self
            .store
            .complete_multipart_upload(upload_id, lease_owner, &result_json, audit)
            .await?;
        if !finished {
            let fresh = self
                .store
                .get_multipart_upload(upload_id)
                .await?
                .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;
            if fresh.state != MultipartUploadState::Completed {
                return Err(DomainError::multipart_upload_not_in_progress(
                    upload_id,
                    fresh.state.as_str(),
                ));
            }
        }
        Ok(())
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
            session.resolve_backend_id(
                version.as_ref().map(|v| v.backend_id.as_str()),
                self.backends.default_id(),
            )
        } else {
            String::new()
        };

        // Resume tokens expire at `min(session expiry, now + url_ttl_secs)`, not the long session expiry.
        let url_ttl_cap = now
            .checked_add(time::Duration::seconds(self.url_ttl_secs.max(1)))
            .ok_or_else(|| {
                DomainError::database("signed-URL TTL overflowed computing the resume URL cap")
            })?;
        let exp = session.expires_at.min(url_ttl_cap).unix_timestamp();
        let request_id = Uuid::now_v7().to_string();
        let backend_path = session.backend_path.clone();

        let mut missing = Vec::new();
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
        let backend_id = session.resolve_backend_id(
            version.as_ref().map(|v| v.backend_id.as_str()),
            self.backends.default_id(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = session.backend_path.clone();

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

        // DB errors propagate; an already-missing row (`false`) is the desired end state. The file
        // goes too when this was its only version (composite create) and it has no content; the
        // guard keeps a file with other versions.
        self.discard_version_and_orphan_file(
            ctx,
            file.tenant_id,
            file.owner_id,
            file_id,
            session.version_id,
            "multipart_abort",
        )
        .await?;

        Ok(())
    }

    /// Delete a backend blob, logging on error; a failed delete leaves an orphan for cleanup.
    async fn best_effort_blob_delete(&self, backend_id: &str, path: &str) {
        crate::domain::service::best_effort_blob_delete(
            &self.backends,
            self.metrics.as_ref(),
            backend_id,
            path,
        )
        .await;
    }
}

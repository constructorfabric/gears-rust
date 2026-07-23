//! `MultipartService` — multipart upload control-plane logic.
//!
//! Owns the P2-M3 / multipart-coordinator flows: initiate (server-authoritative
//! plan + per-part signed URLs), complete, and abort.
//!
//! The control-plane byte route (`upload_multipart_part`) has been removed as
//! part of the multipart-coordinator feature — bytes now flow exclusively to
//! the sidecar via the per-part signed URLs returned by `initiate_multipart_upload`
//! (DESIGN §4.6, ADR-0003, FEATURE §8 migration).
//!
//! Holds its own copies of the shared dependencies (`Store`, `BackendRegistry`,
//! `Authorizer`, `QuotaClient`, `Issuer`) so it does NOT reference `FileService`
//! — that keeps the fan-in graph clean and avoids raising the HK score of
//! `FileService`.

// Domain terms (ETag, If-Match, FileStorage, GET/PUT, BLAKE3) appear in the docs.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use crate::domain::authz::{Authorizer, actions};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::{
    BindState, CompletedMultipartUpload, DEFAULT_MIN_PART_SIZE, MAX_PART_SIZE, MissingPart,
    MultipartCompleteOutcome, MultipartPart, MultipartPartPlan, MultipartPlan,
    MultipartUploadSession, MultipartUploadState, MultipartUploadStatus, ReceivedPart,
    StoredCompleteResult, compute_plan,
};

/// `Retry-After` hint (seconds) returned with a `202 completing` answer — the
/// polling client re-issues the same idempotent `complete` after this delay.
const COMPLETE_POLL_RETRY_SECS: u64 = 2;
use crate::domain::policy::{PolicyResolver, PolicyScope};
use crate::domain::ports::{AutoBindOnFinalize, FileStorageMetricsPort, MultipartStore};
use crate::infra::backend::BackendRegistry;
use crate::infra::content::mime::{
    MIME_SNIFF_PREFIX_BYTES, enforce_size_ceiling_for_validated_mime, validate_and_resolve_mime,
};
use crate::infra::external_clients::{QuotaClient, QuotaDecision, UsageDelta, UsageReporter};
use crate::infra::metrics::NoopMetrics;
use crate::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage_sdk::ByteRange;

/// Quota metric name (duplicated from service.rs; both refer to the same
/// platform metric — no abstraction needed here).
const QUOTA_METRIC_NAME: &str = gts_id!("cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1");

/// Diff the plan's expected part numbers against the parts actually reported,
/// returning the missing ones in ascending order.
///
/// `expected_count = ceil(declared_size / part_size)` mirrors
/// [`compute_plan`]'s part count exactly, including its `declared_size == 0`
/// special case (a single, zero-byte part) so a zero-byte upload's one
/// expected part is never spuriously reported as "missing".
///
/// Item 3.3 (multipart `complete`'s richer contract) is the first caller;
/// item 3.4 (introspect/resume) reuses this same helper rather than
/// recompute the diff.
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

/// Recompute a single part's `(offset, size)` from the session's
/// deterministic `(declared_size, part_size)` columns — the same per-part
/// math [`compute_plan`] applies when building the initiate-time plan, just
/// evaluated for one `part_number` instead of materializing the whole plan
/// (item 3.4 — introspect/resume reconstructs only the missing parts'
/// bounds). `declared_size == 0` mirrors `compute_plan`'s single zero-byte
/// part special case.
///
/// Uses saturating arithmetic as defense-in-depth against a corrupted
/// session row, mirroring `compute_plan`'s own overflow guards; a
/// `part_number` outside `[1, expected_count]` is never passed in practice
/// (callers only invoke this for numbers `missing_part_numbers` returned).
pub(crate) fn part_bounds(session: &MultipartUploadSession, part_number: u32) -> (u64, u64) {
    if session.declared_size == 0 {
        return (0, 0);
    }
    let part_size = session.part_size.max(1);
    let offset = u64::from(part_number.saturating_sub(1)).saturating_mul(part_size);
    let size = part_size.min(session.declared_size.saturating_sub(offset));
    (offset, size)
}

/// The multipart-upload service (multipart-coordinator feature).
///
/// Extracted from `FileService` to reduce its Henry-Kafura coupling score.
/// All multipart control-plane operations live here; the struct is wired
/// alongside `FileService` in `gear.rs` and served under the same REST prefix.
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
    /// Signed-URL TTL in seconds, applied to every per-part upload URL
    /// (`default_url_ttl_secs` -- kept short to bound the stale-permission
    /// window, DESIGN §4.5). Independent of [`Self::session_ttl_secs`] since
    /// the `multipart-session-ttl` remediation: a large upload's session
    /// lifetime must not be capped at the same short window used for
    /// individual signed URLs -- see [`Self::session_ttl_secs`]'s own doc.
    url_ttl_secs: i64,
    /// Lifetime (seconds) of the multipart session itself -- i.e. how long
    /// `expires_at` on the `multipart_uploads` row is set to at initiate
    /// time. Defaults to [`Self::url_ttl_secs`] in [`Self::new`] (preserving
    /// the pre-remediation behavior for callers that don't opt in), but
    /// `gear.rs` overrides it via [`Self::with_session_ttl_secs`] to a much
    /// longer, dedicated `multipart_session_ttl_secs` config value. Before
    /// this remediation the session shared `url_ttl_secs` (a short,
    /// stale-permission bound meant for individual signed URLs, DESIGN
    /// §4.5), which capped every multi-GB multipart upload's *total* time
    /// budget at 15 minutes by default -- self-defeating for the very
    /// large-upload use case multipart exists for, and for the
    /// introspect/resume feature (`cpt-cf-file-storage-flow-
    /// multipart-introspect`), whose resume-token expiry is capped at the
    /// session's own `expires_at` and so could never extend a session past
    /// that same 15 minutes.
    session_ttl_secs: i64,
    /// Completion-lease duration (seconds) — how long a `complete` may hold
    /// the `completing` state before another `complete` can take it over
    /// (upload-flow redesign). Sized to the backend-assembly budget; config
    /// knob `multipart_complete_lease_secs` (default 120), threaded by
    /// `gear.rs` via [`Self::with_complete_lease_secs`].
    complete_lease_secs: i64,
    /// Metrics port (P2 1.8 remediation). Defaults to a no-op implementation
    /// (see [`Self::new`]); `gear.rs` opts into the real OTel-backed meter via
    /// [`Self::with_metrics`].
    metrics: Arc<dyn FileStorageMetricsPort>,
    /// Usage-reporting sink (P2 1.12 remediation). `None` disables reporting
    /// (fire-and-forget no-op); `gear.rs` opts in via
    /// [`Self::with_usage_reporter`] once a Usage Collector client is wired.
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
            // Defaults to url_ttl_secs so existing `MultipartService::new(...)`
            // call sites across the integration-test suite keep compiling and
            // behaving unchanged; `gear.rs` opts into a real, decoupled value
            // via `with_session_ttl_secs`.
            session_ttl_secs: url_ttl_secs,
            complete_lease_secs: 120,
            metrics: Arc::new(NoopMetrics),
            usage_reporter: None,
        }
    }

    /// Install a dedicated completion-lease duration (upload-flow redesign).
    /// Same builder shape as [`Self::with_metrics`] — existing
    /// `MultipartService::new(...)` call sites keep compiling unchanged.
    #[must_use]
    pub fn with_complete_lease_secs(mut self, complete_lease_secs: i64) -> Self {
        self.complete_lease_secs = complete_lease_secs;
        self
    }

    /// Install a dedicated multipart-session lifetime, decoupled from the
    /// per-part signed-URL TTL (`multipart-session-ttl` remediation). Same
    /// builder shape as [`Self::with_metrics`] -- existing
    /// `MultipartService::new(...)` call sites keep compiling unchanged.
    #[must_use]
    pub fn with_session_ttl_secs(mut self, session_ttl_secs: i64) -> Self {
        self.session_ttl_secs = session_ttl_secs;
        self
    }

    /// Install a real metrics port (P2 1.8 remediation). Kept as a builder
    /// step rather than a `new()` parameter so existing
    /// `MultipartService::new(...)` call sites across the integration-test
    /// suite keep compiling unchanged; only `gear.rs` needs to opt in.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn FileStorageMetricsPort>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Install a usage-reporting sink (P2 1.12 remediation). Same builder
    /// shape as [`Self::with_metrics`] -- existing `MultipartService::new(...)`
    /// call sites keep compiling unchanged.
    #[must_use]
    pub fn with_usage_reporter(mut self, usage_reporter: Option<Arc<dyn UsageReporter>>) -> Self {
        self.usage_reporter = usage_reporter;
        self
    }

    /// Fire-and-forget usage delta report. Failures are logged but never
    /// propagated -- a failing usage reporter must not block file operations.
    ///
    /// Mirrors `FileService::report_usage` (kept private/independent per this
    /// service's fan-in-isolation design -- see the module doc).
    ///
    /// @cpt-cf-file-storage-fr-usage-reporting
    fn report_usage(&self, delta: UsageDelta) {
        if let Some(reporter) = self.usage_reporter.clone() {
            tokio::spawn(async move {
                reporter.report(delta).await;
            });
        }
    }

    // ── private helpers ──────────────────────────────────────────────────────

    fn tenant_scope(ctx: &SecurityContext) -> AccessScope {
        AccessScope::for_tenant(ctx.subject_tenant_id())
    }

    fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
        format!("/{file_id}/{version_id}")
    }

    fn actor_kind(ctx: &SecurityContext) -> &'static str {
        match ctx.subject_type() {
            Some("app") => "app",
            _ => "user",
        }
    }

    /// Build a success audit entry for a file-scoped write operation.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
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
    ///
    /// @cpt-cf-file-storage-fr-allowed-types-policy
    /// @cpt-cf-file-storage-fr-size-limits-policy
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

    /// Run a quota preflight check for `additional_bytes` of new storage.
    ///
    /// At multipart initiate time this is called with the declared total size,
    /// giving the quota service a precise figure rather than a pessimistic ceiling.
    ///
    /// **Fail-closed**: a failing quota client denies the request.
    ///
    /// @cpt-cf-file-storage-fr-storage-quota
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

    /// Best-effort compensation when session persistence fails after the backend
    /// handle was already created and the pending version row was already inserted.
    ///
    /// Aborts the backend multipart handle and removes the pending version row so
    /// they are not left as orphans. Both steps are best-effort: errors are logged
    /// but not propagated — the caller's original error is returned instead, and
    /// any remaining orphans are reclaimed by the orphan-reconciliation sweep.
    async fn compensate_failed_session_create(
        &self,
        ctx: &SecurityContext,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_path: &str,
        backend_handle: &str,
    ) {
        // Best-effort: abort the backend handle.
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
        // Best-effort: remove the pending version row.
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

    /// Mint one signed per-part upload URL (FEATURE §4). Shared by
    /// [`Self::initiate_multipart_upload`] (fresh full-TTL tokens for every
    /// planned part) and [`Self::introspect_multipart_upload`] (item 3.4:
    /// resume tokens for the still-missing parts, with `exp` passed in by
    /// the caller so it can be capped at the session's remaining
    /// `expires_at` rather than a fresh full TTL).
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
            // P2 1.11: content_type/etag are GET-only claims; a
            // multipart-part token is always `op = multipart_part`.
            content_type: String::new(),
            etag: String::new(),
            // Multipart binds via `complete` (session auto_bind), never via
            // the per-part token.
            bind_on_finalize: false,
        };
        let token = self.issuer.issue(claims, now)?;
        Ok(format!(
            "{}/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/{part_number}?fs-token={token}",
            self.sidecar_base_url
        ))
    }

    // ── multipart upload (multipart-coordinator feature) ─────────────────────

    /// `POST /files/{id}/multipart`: initiate a multipart upload session.
    ///
    /// Server-authoritative: validates the intent, pre-registers a `pending`
    /// version, creates the backend session, computes the **exact parts plan**,
    /// and returns **one signed URL per part** pointing at the sidecar
    /// (FEATURE §2, §3, §4; DESIGN §4.6).
    ///
    /// Policy/quota gates (FEATURE §7):
    /// - Allowed MIME: `415`
    /// - Declared size ≤ effective max: `413`
    /// - Storage quota: `507`
    ///
    /// The complete-time total-size check is kept as defence-in-depth.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-size-limits-policy
    /// @cpt-cf-file-storage-fr-storage-quota
    #[tracing::instrument(skip_all)]
    /// `auto_bind` (upload-flow redesign): when `true`, `complete` will bind
    /// the finalized version itself (recorded on the session row). Only the
    /// merged `POST /files` create+plan path passes `true`; the standalone
    /// `POST /files/{id}/multipart` route keeps the staged (manual-bind)
    /// behaviour with `false`.
    #[allow(clippy::too_many_arguments)]
    pub async fn initiate_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        declared_mime: &str,
        declared_size: u64,
        preferred_part_size: Option<u64>,
        _concurrency: Option<u32>,
        auto_bind: bool,
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

        // Validate the client-supplied `preferred_part_size` hint against a
        // sane range *before* it can reach `compute_plan` (P2 remediation
        // 2.11). Left unchecked, a near-`u64::MAX` value risks an arithmetic
        // overflow in `compute_plan`/`round_up_to` and, on backends that
        // don't hit that overflow, a huge `Vec::with_capacity` allocation.
        // Rejecting is preferred over silently clamping so the client gets
        // an explicit, actionable error.
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

        // Policy checks: allowed mime type and size (at initiate, against the
        // declared total size — DESIGN §4.6 server-authoritative gate).
        //
        // @cpt-cf-file-storage-fr-size-limits-policy
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

        // Gate: reject if the declared total size exceeds the effective limit.
        // This is the DESIGN-aligned fix for CodeRabbit F2: validate up front at
        // initiate time rather than deferring to complete time.
        //
        // @cpt-cf-file-storage-fr-size-limits-policy
        if let Some(limit) = effective_max
            && declared_size > limit
        {
            return Err(DomainError::policy_size_exceeded(
                limit,
                "policy size limit",
            ));
        }

        // Quota check against the declared size (not the pessimistic effective_max).
        // PRD §5.4: "check before accepting any operation that increases storage
        // consumption" — the declared size is our best estimate at this stage.
        //
        // @cpt-cf-file-storage-fr-storage-quota
        self.check_quota_bytes(tenant_id, file.owner_id, declared_size)
            .await?;

        let now = OffsetDateTime::now_utc();
        let upload_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let backend_path = Self::backend_path(file_id, version_id);
        let backend_id = backend.id().to_owned();

        // Compute the server-authoritative parts plan (FEATURE §3).
        // `backend_min_part_size` is not yet exposed by the BackendCapabilities
        // API so we fall back to the `DEFAULT_MIN_PART_SIZE` constant.
        //
        // `compute_plan` enforces the `MAX_PART_COUNT` ceiling itself (widening
        // `part_size` where possible, rejecting where even the max part size
        // cannot fit `declared_size` within the ceiling) *before* allocating
        // the parts vector, so an unbounded/adversarial `declared_size` never
        // drives an allocation proportional to it here.
        let (chosen_part_size, raw_parts) = compute_plan(declared_size, preferred_part_size, None)?;

        // Pre-register the pending file_versions row.
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

        // Initiate the multipart upload on the backend.
        let backend_handle = backend.initiate_multipart(&backend_path).await?;

        // The session's own lifetime (`multipart_session_ttl_secs`, e.g. 24h)
        // is a dedicated, much longer-lived budget than the per-part signed
        // URLs' TTL (`default_url_ttl_secs`, e.g. 15 min) -- a multi-GB
        // upload needs real wall-clock time to complete, while any one
        // signed URL should stay short-lived to bound the stale-permission
        // window (DESIGN §4.5). A session that outlives its own URLs is the
        // whole point of the introspect/resume flow: the client re-fetches
        // fresh part URLs (capped at the session's own `expires_at`, see
        // `introspect_multipart_upload`) as earlier ones expire.
        let session_expires_at = now + time::Duration::seconds(self.session_ttl_secs.max(1));
        let url_expires_at = now + time::Duration::seconds(self.url_ttl_secs.max(1));

        // Persist the session row. On failure, best-effort compensate to avoid
        // orphaning the backend handle and the pending version row.
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
                auto_bind,
                session_expires_at,
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

        // Mint one signed URL per part (FEATURE §4).
        // Each token carries the exact `size` claim the sidecar will enforce.
        // P2 1.8: every part of the same upload shares one correlation id, so
        // the sidecar's report-part callbacks for this upload all echo back
        // the same `x-request-id`.
        let exp = url_expires_at.unix_timestamp();
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
            expires_at: url_expires_at,
        })
    }

    /// `POST /files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report`:
    /// token-authenticated callback used by the sidecar to record a
    /// successfully-written part (P2 0.2 group B — the "report part" fix).
    ///
    /// Before this existed, nothing ever called
    /// `MultipartStore::upsert_multipart_part` in a real deployment, so
    /// `complete_multipart_upload`'s `list_multipart_parts` was always
    /// structurally empty. `claims` has already been verified by the caller
    /// (mirrors `finalize_version`'s handler-level token verification) and
    /// `claims.op == Op::MultipartPart` has already been asserted there; this
    /// method re-validates the claims against the session so a valid token for
    /// a *different* (or no-longer-`in_progress`) session cannot poison
    /// another upload's part list. It also rejects a caller-supplied `size`
    /// that does not match `claims.multipart.size` (the authoritative
    /// per-part size minted into the token at initiate time) so a holder of
    /// the signed token cannot forge a part's size and corrupt the summed
    /// `version.size` computed by `complete_multipart_upload`.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
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

        // Bind the report to the exact (file_id, version_id) the token
        // authorizes — a foreign session is reported as "not found" rather
        // than distinguishable, mirroring `complete_multipart_upload`'s
        // same-shaped guard.
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

        // Security: this callback is `.public()` + token-authenticated, so a
        // holder of the signed part token could otherwise report an arbitrary
        // `size` that `complete_multipart_upload` later sums into
        // `version.size` unchecked. `claims.multipart.size` is the exact
        // per-part size computed by `compute_plan` at initiate time (uniform
        // for all parts except the last, which is legitimately smaller — see
        // `compute_plan` in `multipart.rs`), so it is always the exact size
        // this specific part must have; reject a mismatch rather than trust
        // the caller-supplied value, and persist the authoritative claimed
        // size instead of the (already-verified-equal) caller value.
        let claimed_size = i64::try_from(claims.multipart.size)
            .map_err(|_| DomainError::validation("size", "size overflows i64"))?;
        if size != claimed_size {
            return Err(DomainError::validation(
                "size",
                "reported part size does not match the planned size for this part",
            ));
        }

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-upload-part:p1:inst-part-db-upsert
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
        // @cpt-end:cpt-cf-file-storage-flow-multipart-upload-part:p1:inst-part-db-upsert
    }

    /// `POST /files/{id}/multipart/{upload_id}/complete`: finalize all parts.
    ///
    /// Upload-flow redesign — completion state machine. NO DB lock or open
    /// transaction is ever held across the backend assembly I/O; instead the
    /// session moves through instant conditional-UPDATE transitions:
    /// `in_progress → completing(lease_owner, lease_until) →
    /// completed(complete_result)` (or `aborted`). Concretely:
    ///
    /// * The caller that wins the lease CAS runs the assembly in a
    ///   **detached task** (client disconnect cannot cancel it) and, when
    ///   done, commits one fast transaction: version `available` (+hash/
    ///   manifest, + auto-bind CAS for `bind: "auto"` sessions), then flips
    ///   the session to `completed` persisting the response snapshot.
    /// * A concurrent `complete` that loses the CAS answers
    ///   [`MultipartCompleteOutcome::Completing`] (HTTP 202) — the client
    ///   polls by re-issuing the same idempotent `complete`.
    /// * A re-complete of a `completed` session replays the persisted
    ///   snapshot — success, never 409, including "already bound".
    /// * A completer that died mid-assembly leaves `completing` behind; once
    ///   `lease_until` passes, the next `complete` takes the lease over,
    ///   checks what actually landed (version already `available` → just
    ///   finish; otherwise re-assemble), and finishes the job. Sessions
    ///   stuck in `completing` past `expires_at` are backstopped by the
    ///   cleanup engine's abandoned-session sweep.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-dod:cpt-cf-file-storage-dod-multipart-complete:p1
    /// @cpt-dod:cpt-cf-file-storage-dod-content-hash-modes-multipart-composite:p2
    #[tracing::instrument(skip_all)]
    pub async fn complete_multipart_upload(
        self: &Arc<Self>,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<MultipartCompleteOutcome, DomainError> {
        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-request
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;

        // Optional If-Match precondition (item 3.3): unlike `bind`, `None`
        // stays unconditional here (backward compatible with the pre-3.3
        // contract) rather than requiring the header once content exists —
        // `complete` is keyed by `upload_id`, not by a rebind of already-bound
        // content, so there is no equivalent "must supply it to rebind"
        // invariant to enforce. `*` (or omission) matches unconditionally; a
        // concrete value must match the file's current content ETag. For an
        // `auto_bind` session this doubles as the PRD §5.10 bind
        // precondition: the embedded bind's CAS target is the same
        // `content_id` this check just validated.
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
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-request

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-load-session
        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // Bind the session to the authorized path `file_id`. Authorization above
        // checks the path file, but the session is loaded by `upload_id` alone —
        // without this a caller could drive another file's upload (and corrupt
        // state via a recomputed backend path). Reported as "not found" so a
        // foreign `upload_id` is not distinguishable from a missing one.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        // Idempotent re-complete: replay the persisted snapshot (or rebuild
        // from the version row for pre-snapshot sessions). Never a 409 on an
        // honest retry.
        if session.state == MultipartUploadState::Completed {
            self.metrics
                .record_operation("complete_multipart_upload", "replayed");
            return Ok(MultipartCompleteOutcome::Completed(
                self.replay_completed(file_id, &session).await?,
            ));
        }
        if session.state == MultipartUploadState::Aborted {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // Defence-in-depth (P2 0.3 step 3): the session may still read as
        // live here even though `expires_at` has already passed, if the
        // background sweep has not yet ticked. Reject explicitly rather
        // than racing ahead of the next sweep and finalizing content that
        // should have been aborted.
        if session.expires_at <= OffsetDateTime::now_utc() {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id, "expired",
            ));
        }

        // Acquire the completion lease: one conditional UPDATE covering both
        // the fresh `in_progress` acquire and the expired-`completing`
        // takeover. Losing it is not an error — the holder is (or was)
        // completing; answer 202 or replay accordingly.
        let now = OffsetDateTime::now_utc();
        let lease_owner = Uuid::now_v7().to_string();
        let lease_until = now + time::Duration::seconds(self.complete_lease_secs.max(1));
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
                MultipartUploadState::Completed => {
                    self.metrics
                        .record_operation("complete_multipart_upload", "replayed");
                    Ok(MultipartCompleteOutcome::Completed(
                        self.replay_completed(file_id, &fresh).await?,
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
        let takeover = session.state == MultipartUploadState::Completing;

        // Winner: run the assembly in a DETACHED task — a client that
        // disconnects (or a request future that is dropped) cannot cancel
        // the work; the result is persisted and any later `complete`
        // (F5, another tab) replays it. The current request still awaits
        // the task's handle so the ordinary path returns 200 directly.
        let svc = Arc::clone(self);
        let ctx = ctx.clone();
        let handle = tokio::spawn(async move {
            svc.assemble_and_finish(&ctx, file, session, lease_owner, takeover)
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

    /// Rebuild the response for an already-`completed` session: prefer the
    /// persisted snapshot; fall back to the version row (pre-snapshot rows).
    async fn replay_completed(
        &self,
        file_id: Uuid,
        session: &MultipartUploadSession,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        if let Some(json) = &session.complete_result
            && let Ok(stored) = serde_json::from_str::<StoredCompleteResult>(json)
            && let Some(completed) = stored.into_completed()
        {
            return Ok(completed);
        }
        // Fallback: rebuild from the version row. Re-read the file for a
        // fresh content pointer (the caller's snapshot may be stale).
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
        let (bind_state, bind_etag, current_etag) =
            Self::bind_state_for(&file, session, session.version_id);
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

    /// Derive the ONE shared bind-state model (see [`BindState`]) from the
    /// file's current pointer: bound to this version → `Bound` (+new ETag);
    /// auto-bind session pointing elsewhere → `Conflict` (+the CURRENT ETag a
    /// manual rebind's If-Match needs); manual session → `Manual`.
    fn bind_state_for(
        file: &file_storage_sdk::File,
        session: &MultipartUploadSession,
        version_id: Uuid,
    ) -> (BindState, Option<String>, Option<String>) {
        if file.content_id == Some(version_id) {
            (
                BindState::Bound,
                Some(etag::content_etag(file.file_id, version_id)),
                None,
            )
        } else if session.auto_bind {
            (BindState::Conflict, None, etag::etag_for(file))
        } else {
            (BindState::Manual, None, None)
        }
    }

    /// The lease-holder's assembly + finish path (upload-flow redesign) —
    /// runs in a detached task. On error, best-effort releases the lease so
    /// the next `complete` retries immediately instead of waiting out
    /// `lease_until`.
    async fn assemble_and_finish(
        &self,
        ctx: &SecurityContext,
        file: file_storage_sdk::File,
        session: MultipartUploadSession,
        lease_owner: String,
        takeover: bool,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        let upload_id = session.upload_id;
        let result = self
            .assemble_and_finish_inner(ctx, &file, &session, takeover)
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

    #[allow(clippy::too_many_lines)]
    async fn assemble_and_finish_inner(
        &self,
        ctx: &SecurityContext,
        file: &file_storage_sdk::File,
        session: &MultipartUploadSession,
        takeover: bool,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        let file_id = file.file_id;
        let upload_id = session.upload_id;

        // Takeover fast-path: the previous completer may have died AFTER the
        // finalize transaction (version available, bind decided) but BEFORE
        // flipping the session to `completed`. Nothing is left to assemble —
        // just finish the state machine and persist the snapshot.
        if takeover
            && let Some(v) = self.store.get_version(file_id, session.version_id).await?
            && v.status == file_storage_sdk::VersionStatus::Available
        {
            let completed = self.replay_completed(file_id, session).await?;
            self.finish_session(ctx, session, &completed).await?;
            return Ok(completed);
        }
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-load-session

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-load-parts
        let parts = self.store.list_multipart_parts(upload_id).await?;
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-load-parts

        // Fetch the backend from the version row.
        let version = self.store.get_version(file_id, session.version_id).await?;
        let backend_id = version.as_ref().map_or_else(
            || self.backends.default_id().to_owned(),
            |v| v.backend_id.clone(),
        );
        let backend = self.backends.get(&backend_id)?;
        let backend_path = Self::backend_path(file_id, session.version_id);

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-missing-parts
        // Reject with the specific missing part numbers before falling through
        // to the coarser residual size check below (item 3.3) — a caller
        // debugging a stalled upload gets an actionable list instead of an
        // opaque size mismatch.
        let missing = missing_part_numbers(&session, &parts);
        if !missing.is_empty() {
            return Err(DomainError::multipart_parts_missing(upload_id, missing));
        }
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-missing-parts

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-size-verify
        // Compute total assembled size from the parts that the sidecar wrote.
        let total_size: i64 = parts.iter().map(|p| p.size).sum();

        // Defence-in-depth: verify the assembled size matches `declared_size`
        // (FEATURE §6, §7 — "Total assembled size = declared_size").
        //
        // The primary enforcement is per-part at the sidecar (the `size` claim
        // in each token); this check catches residual mismatches (e.g. a
        // missing/extra part).
        if session.declared_size > 0 {
            let expected = i64::try_from(session.declared_size).unwrap_or(i64::MAX);
            if total_size != expected {
                return Err(DomainError::conflict(format!(
                    "multipart upload {upload_id}: assembled size {total_size} \
                     does not match declared_size {expected}"
                )));
            }
        }
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-size-verify

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-policy-check
        // Policy size check.
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
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-policy-check

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-assemble
        // Build the parts list for the backend, threading each part's byte
        // offset and its already-computed SHA-256 digest (ADR-0006) — these
        // are no longer discarded. The offset is the running sum of prior
        // parts' sizes; parts are listed in ascending part-number order (the
        // repo's `list_parts` `ORDER BY part_number`), which for any valid
        // plan is identical to ascending offset order.
        // @cpt-begin:cpt-cf-file-storage-algo-combine-part-hashes:p1:inst-combine-sort
        // `parts` is already in ascending part_number order (`list_parts`'s
        // `ORDER BY part_number`, verified gapless by the missing-parts diff
        // above), which for any valid plan is identical to ascending offset
        // order -- no separate sort step is needed here.
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
        // @cpt-end:cpt-cf-file-storage-algo-combine-part-hashes:p1:inst-combine-sort

        // Assemble on the backend, which builds the offset-manifest and its
        // `root` from the per-part digests+offsets above — **no re-read of the
        // assembled object** (ADR-0006). `root` becomes the version's
        // `hash_value`; the manifest text is persisted in
        // `version_hash_manifest` transactionally with the version row below.
        // @cpt-begin:cpt-cf-file-storage-algo-combine-part-hashes:p1:inst-combine-sha256
        let (manifest, root) = match backend
            .complete_multipart(
                &backend_path,
                &session.backend_upload_handle,
                &backend_parts,
            )
            .await
        {
            Ok(assembled) => assembled,
            Err(assemble_err) if takeover => {
                // Takeover recovery: the crashed completer may have already
                // consumed the backend's multipart handle (assembled object
                // exists, `CompleteMultipartUpload` no longer replayable).
                // If the object is really there, derive the same
                // (manifest, root) locally from the persisted part rows —
                // deterministic, byte-identical to what the backend built.
                let object_exists = backend
                    .get_range(&backend_path, ByteRange::Inclusive { start: 0, end: 0 })
                    .await
                    .is_ok();
                if !object_exists {
                    return Err(assemble_err);
                }
                let entries = backend_parts
                    .iter()
                    .map(|(_, offset, digest, _)| {
                        crate::infra::content::hash_mode::ManifestEntry {
                            offset: *offset,
                            digest: *digest,
                        }
                    })
                    .collect();
                let manifest = crate::infra::content::hash_mode::Manifest::new(entries)?;
                let root = manifest.root();
                (manifest, root)
            }
            Err(e) => return Err(e),
        };
        // @cpt-end:cpt-cf-file-storage-algo-combine-part-hashes:p1:inst-combine-sha256
        // @cpt-begin:cpt-cf-file-storage-algo-combine-part-hashes:p1:inst-combine-return
        // ADR-0006 single-part amendment: a **one-part plan degenerates to
        // `whole-sha256`** — the single part's streaming digest IS
        // `sha256(whole object bytes)` (part 1 spans the entire object), so
        // the composite wrapping (`root = sha256("v1,0:<h>")`) adds nothing
        // and would only make the same content hash differently depending on
        // how it was uploaded. No manifest row is persisted, `part_count`
        // stays NULL on the version row (the schema's `whole-sha256`
        // convention), and no re-read is needed — the digest was computed on
        // the part's streaming write. Plans of ≥2 parts keep the composite
        // mode unchanged.
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
        // @cpt-end:cpt-cf-file-storage-algo-combine-part-hashes:p1:inst-combine-return
        let part_count = i32::try_from(parts.len())
            .map_err(|_| DomainError::validation("part_count", "part count overflows i32"))?;
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-assemble

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-mime-validate
        // Sniff the assembled object's leading bytes and validate against
        // `session.declared_mime` -- the single-part finalize paths
        // (`write.rs::finalize_upload`/`finalize_upload_by_token`) already do
        // this; multipart-complete was the one finalize path that let a
        // policy restricting MIME types be bypassed by declaring an allowed
        // type at initiate and multipart-uploading arbitrary bytes (P2
        // remediation item 1.10). Validation runs post-assembly (S3 part
        // objects are not independently readable pre-complete, so the backend
        // is the only place the *whole* assembled object can be sniffed).
        //
        // A zero-byte object has no bytes to sniff -- `mime::detect` on an
        // empty slice never recognizes a signature, so the declared type is
        // always accepted as-is for an empty upload, exactly like the
        // single-part path's read-back handles empty content.
        //
        // @cpt-cf-file-storage-fr-content-type-validation
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
        // On mismatch this fails **before** any DB finalize -- the assembled
        // blob at `backend_path` becomes an orphan reclaimed by the
        // orphan-reconciliation sweep, the same recovery story as the
        // `!finalized`/`!completed` branches below (the backend object is
        // always allowed to outlive a failed finalize; the sweep is the sole
        // cleanup path for it).
        let validated_mime = validate_and_resolve_mime(&session.declared_mime, &mime_sniff_prefix)?;
        enforce_size_ceiling_for_validated_mime(
            &policy,
            &session.declared_mime,
            &validated_mime,
            backend.capabilities().max_size_bytes,
            total_size,
        )?;
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-mime-validate

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-finalize-version
        // Finalize the version row (no separate audit row — complete below covers it).
        let finalize_audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::FinalizeVersion,
            serde_json::json!({ "version_id": session.version_id, "upload_id": upload_id, "size": total_size }),
        );
        // Upload-flow redesign: an `auto_bind` session (merged create+plan
        // with `bind: "auto"`) binds inside this same finalize transaction.
        // The CAS precondition is the `content_id` observed above — already
        // validated against the caller's optional `If-Match` (PRD §5.10's
        // per-bind CAS is preserved; for a brand-new file this is the
        // `content_id IS NULL` first-content case). A lost CAS is not an
        // error: complete still succeeds, `bound: false` reports it, and a
        // manual rebind needs no re-upload.
        let auto_bind = session.auto_bind.then(|| AutoBindOnFinalize {
            expected_content_id: file.content_id,
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

        let finalize_outcome = self
            .store
            .finalize_version(
                file_id,
                session.version_id,
                total_size,
                content_hash.clone(),
                hash_mode,
                // NULL for the degenerate one-part plan — matches the schema
                // convention that `whole-sha256` versions carry no part_count.
                (!single_part).then_some(part_count),
                manifest_text.clone(),
                Some(validated_mime),
                finalize_audit,
                auto_bind,
            )
            .await?;
        let finalized = finalize_outcome.updated;
        let bound = finalize_outcome.bound;
        if !finalized {
            // The pending version row disappeared (concurrent abort or cleanup)
            // after the backend assembled the object. Fail loudly instead of
            // reporting success with no bound version; the now-orphaned blob at
            // `backend_path` is reclaimed by the orphan-reconciliation sweep.
            return Err(DomainError::conflict(format!(
                "multipart upload {upload_id}: version row was removed before completion"
            )));
        }
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-finalize-version

        // ONE shared bind-state model (see `BindState`): bound (+new ETag) /
        // conflict (+the file's CURRENT ETag for a manual rebind) / manual.
        let (bind_state, bind_etag, current_etag) = if bound {
            (
                BindState::Bound,
                Some(etag::content_etag(file_id, session.version_id)),
                None,
            )
        } else if session.auto_bind {
            // Lost CAS — re-read the file for the pointer that won.
            let fresh = self
                .store
                .require_file(&AccessScope::allow_all(), file_id)
                .await?;
            (BindState::Conflict, None, etag::etag_for(&fresh))
        } else {
            (BindState::Manual, None, None)
        };

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

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-db-session
        // Terminal state transition + response snapshot + audit (fast tx).
        self.finish_session(ctx, session, &result).await?;
        // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-db-session

        // @cpt-cf-file-storage-fr-usage-reporting
        // Credit the assembled object's total bytes. Multipart finalize does
        // not go through `FileService::finalize_upload`, so it needs its own
        // credit call; `file_count_delta` is `0` because the file itself was
        // already reported `+1` at `create_file` time (multipart uploads
        // always target an existing file -- `initiate_multipart_upload`
        // requires `file_id` to already resolve via `require_file`).
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

    /// Terminal `completing → completed` transition, persisting the response
    /// snapshot + the main audit row in one fast transaction.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    async fn finish_session(
        &self,
        ctx: &SecurityContext,
        session: &MultipartUploadSession,
        result: &CompletedMultipartUpload,
    ) -> Result<(), DomainError> {
        let upload_id = session.upload_id;
        let audit = Self::audit_ok(
            ctx,
            Some(session.file_id),
            AuditOperation::MultipartComplete,
            serde_json::json!({ "upload_id": upload_id, "version_id": session.version_id }),
        );
        let result_json = serde_json::to_string(&StoredCompleteResult::from_completed(result))
            .map_err(|_| DomainError::database("failed to serialize complete result"))?;
        let finished = self
            .store
            .complete_multipart_upload(upload_id, &result_json, audit)
            .await?;
        if !finished {
            // Our lease expired mid-flight and someone else moved the session
            // on. If they finished it, the outcome converges (same parts, same
            // deterministic result) — succeed; anything else is a real conflict.
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

    /// `GET /files/{id}/multipart/{upload_id}`: introspect a multipart
    /// upload session (item 3.4 — ship variant).
    ///
    /// Returns the session's current state, the parts already reported, and
    /// the parts still missing. For a still-live session (`in_progress` and
    /// not yet `expires_at`), each missing part also gets a freshly-minted
    /// resume URL so a client can continue an interrupted upload without
    /// re-initiating; a terminal (`completed`/`aborted`) or expired session
    /// reports state/part-accounting only, with no URLs to act on.
    ///
    /// Authorized on `actions::WRITE`, not `READ`: introspect exists to let
    /// the caller *resume* an upload (it hands out live upload URLs), so it
    /// is gated the same as initiate/complete/abort rather than opened to a
    /// read-capable-but-not-write principal.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    #[tracing::instrument(skip_all)]
    pub async fn introspect_multipart_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        upload_id: Uuid,
    ) -> Result<MultipartUploadStatus, DomainError> {
        // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-authz
        let prefetch = Self::tenant_scope(ctx);
        let file = self.store.require_file(&prefetch, file_id).await?;
        let _scope = self
            .authorizer
            .authorize(ctx, actions::WRITE, &file.gts_file_type, Some(file_id))
            .await?;
        // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-authz

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-load-session
        let session = self
            .store
            .get_multipart_upload(upload_id)
            .await?
            .ok_or_else(|| DomainError::multipart_upload_not_found(upload_id))?;

        // Bind the session to the authorized path `file_id` -- same masking
        // guard `complete_multipart_upload` uses: a foreign `upload_id` is
        // reported as "not found" rather than distinguishable.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }
        // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-load-session

        // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-load-parts
        let parts = self.store.list_multipart_parts(upload_id).await?;
        // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-load-parts
        // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-diff
        let missing_numbers = missing_part_numbers(&session, &parts);
        // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-diff

        let now = OffsetDateTime::now_utc();
        let can_resume =
            session.state == MultipartUploadState::InProgress && session.expires_at > now;

        // Only look up the backend when a resume URL might actually be
        // minted -- an expired/terminal session skips this DB round trip
        // entirely, mirroring the `can_resume`-gated cost elsewhere below.
        let backend_id = if can_resume {
            let version = self.store.get_version(file_id, session.version_id).await?;
            version.map_or_else(|| self.backends.default_id().to_owned(), |v| v.backend_id)
        } else {
            String::new()
        };

        // Resume tokens get the same short URL TTL as any freshly-minted
        // part URL, capped so they never outlive the session -- NOT the
        // session's own (long-lived, `multipart_session_ttl_secs`) expiry.
        // The session may legitimately outlive any one URL's TTL (that is
        // the whole point of resume/introspect); minting a resume URL with
        // `exp = session.expires_at` would let an early resume URL stay
        // valid for the session's full lifetime (e.g. ~24h), defeating the
        // short-URL-TTL design (DESIGN §4.5). So: `exp = min(session
        // expiry, now + url_ttl_secs)`.
        let url_ttl_cap = now + time::Duration::seconds(self.url_ttl_secs.max(1));
        let exp = session.expires_at.min(url_ttl_cap).unix_timestamp();
        let request_id = Uuid::now_v7().to_string();
        let backend_path = Self::backend_path(file_id, session.version_id);

        let mut missing = Vec::with_capacity(missing_numbers.len());
        for part_number in missing_numbers {
            // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-recompute-bounds
            let (offset, size) = part_bounds(&session, part_number);
            // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-recompute-bounds
            let upload_url = if can_resume {
                // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-mint-urls
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
                // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-mint-urls
            } else {
                // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-no-urls
                None
                // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-no-urls
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
        // @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-return
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
        // @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-return
    }

    /// `DELETE /files/{id}/multipart/{upload_id}`: abort a multipart upload.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
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

        // Bind the session to the authorized path `file_id`. Authorization above
        // checks the path file, but the session is loaded by `upload_id` alone —
        // without this a caller could drive another file's upload (and corrupt
        // state via a recomputed backend path). Reported as "not found" so a
        // foreign `upload_id` is not distinguishable from a missing one.
        if session.file_id != file_id {
            return Err(DomainError::multipart_upload_not_found(upload_id));
        }

        if session.state != MultipartUploadState::InProgress {
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // Fetch the backend from the version row.
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

        // @cpt-cf-file-storage-fr-audit-trail
        let audit = Self::audit_ok(
            ctx,
            Some(file_id),
            AuditOperation::MultipartAbort,
            serde_json::json!({ "upload_id": upload_id, "version_id": session.version_id }),
        );

        // Mark the session aborted (CAS: in_progress → aborted).
        let aborted = self.store.abort_multipart_upload(upload_id, audit).await?;
        if !aborted {
            // A concurrent complete/abort transitioned the session out of
            // `in_progress` between our snapshot read and this CAS. Surface a
            // conflict and STOP — critically, we must not fall through to the
            // pending-version delete below: had the race been a concurrent
            // *complete*, that version is now finalized/bound and deleting it
            // would corrupt the completed upload.
            return Err(DomainError::multipart_upload_not_in_progress(
                upload_id,
                session.state.as_str(),
            ));
        }

        // Delete the pending version row (no audit row — a pending version is
        // an implementation detail, not a distinct audited file version). A
        // DB error must not be swallowed; a missing row (`false`) is acceptable
        // for an abort, since the pending version being already gone is the
        // desired end state.
        self.store
            .delete_version(
                file_id,
                session.version_id,
                // Deleted as part of abort — record as delete_version for completeness.
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

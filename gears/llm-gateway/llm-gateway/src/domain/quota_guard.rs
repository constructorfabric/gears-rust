//! Quota Guard (DESIGN §3.2, `cpt-cf-llm-gateway-component-quota-guard-v1`).
//!
//! Reserves the upper-bound AI credit cost of a request with a Quota
//! Enforcement lease before dispatch, settles it at the terminal state in the
//! background under the gear's service identity, and then hands the usage to
//! the Usage Reporter ([`UsageSink`]).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use llm_gateway_sdk::{CreateResponseBody, EmbeddingRequest, LlmGatewayError, Usage};
use model_registry_sdk::ModelInfoV1;
use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, CommitLeaseRequest, DecisionResult,
    EvaluationAttribution, LeaseToken, NO_APPLICABLE_QUOTA, QuotaEnforcementClientV1,
    QuotaEnforcementError, ReleaseLeaseRequest, SCOPE_USER, SubjectClaim, TenantId,
};
use rand::RngExt as _;
use tokio_util::task::TaskTracker;
use toolkit_canonical_errors::{CanonicalError, context::InvalidArgumentV1};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

use super::ports::{IdentityContext, ServiceIdentity, UsageReport, UsageSink};
use super::pricing::{
    Pricing, UsageEstimator, credits, estimate_embedding_tokens, estimate_input_tokens,
    max_output_tokens, to_units,
};
use super::provider_rules::ProviderRules;
use crate::config::{QuotaConfig, TimeoutsConfig};

/// Reason tokens of Quota Enforcement the mapping acts on.
mod token {
    pub const LEASE_INFLIGHT_LIMIT_EXCEEDED: &str = "LEASE_INFLIGHT_LIMIT_EXCEEDED";
    pub const LEASE_CONTENTION_TIMEOUT: &str = "LEASE_CONTENTION_TIMEOUT";
}

/// Which pricing, TTL and pool an operation uses.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Sync create-response: needs input and output rates; TTL from
    /// `timeouts.provider_sync`.
    Response,
    /// Streaming create-response: needs input and output rates; TTL from
    /// `timeouts.stream_max_duration`.
    Stream,
    /// Embeddings: needs the input rate only; TTL from
    /// `timeouts.provider_sync`.
    Embedding,
}

/// Identity of the request for attribution, logs and the usage report.
#[domain_model]
#[derive(Debug, Clone)]
pub struct RequestMeta {
    /// Response (or embedding request) `id`.
    pub response_id: String,
    /// Model identifier as requested.
    pub model: String,
    /// Provider type (`gts_type` of the model).
    pub provider_type: String,
}

/// The upper-bound estimate of one request.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quote {
    /// Rates the estimate was priced with.
    pub pricing: Pricing,
    /// Estimated input tokens.
    pub input_tokens: u64,
    /// Upper-bound amount in metric units; 0 means no lease.
    pub reserved_units: i64,
}

/// Quota Enforcement reason token of `err` (where each category carries it:
/// precondition violation type, field violation reason, `Aborted` /
/// `PermissionDenied` reason, quota violation description).
#[must_use]
pub fn reason_token(err: &QuotaEnforcementError) -> Option<String> {
    match err {
        CanonicalError::FailedPrecondition { ctx, .. } => {
            ctx.violations.first().map(|v| v.type_.clone())
        }
        CanonicalError::InvalidArgument {
            ctx: InvalidArgumentV1::FieldViolations { field_violations },
            ..
        } => field_violations.first().map(|v| v.reason.clone()),
        CanonicalError::Aborted { ctx, .. } => Some(ctx.reason.clone()),
        CanonicalError::PermissionDenied { ctx, .. } => Some(ctx.reason.clone()),
        CanonicalError::ResourceExhausted { ctx, .. } => {
            ctx.violations.first().map(|v| v.description.clone())
        }
        _ => None,
    }
}

/// Map a failed `acquire_lease` to the gateway error (DESIGN "Outcomes and
/// errors"), by reason token, never by category alone.
#[must_use]
pub fn map_acquire_error(err: &QuotaEnforcementError) -> LlmGatewayError {
    match err {
        CanonicalError::ServiceUnavailable { .. } | CanonicalError::DeadlineExceeded { .. } => {
            LlmGatewayError::QuotaCheckUnavailable
        }
        CanonicalError::PermissionDenied { .. } => LlmGatewayError::PermissionDenied,
        _ => match reason_token(err).as_deref() {
            Some(token::LEASE_INFLIGHT_LIMIT_EXCEEDED | token::LEASE_CONTENTION_TIMEOUT) => {
                LlmGatewayError::QuotaCheckUnavailable
            }
            // Contract or configuration faults and every unlisted error.
            _ => LlmGatewayError::internal("quota admission failed"),
        },
    }
}

/// Settle error classes.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleErrorClass {
    /// Retry with backoff until the lease expires.
    Transient,
    /// The service identity was refused: re-exchange it, then retry at once.
    Unauthenticated,
    /// Log and abandon.
    Permanent,
}

/// Classify a failed `commit_lease` / `release_lease`.
#[must_use]
pub fn classify_settle_error(err: &QuotaEnforcementError) -> SettleErrorClass {
    match err {
        CanonicalError::ServiceUnavailable { .. } | CanonicalError::DeadlineExceeded { .. } => {
            SettleErrorClass::Transient
        }
        CanonicalError::Unauthenticated { .. } => SettleErrorClass::Unauthenticated,
        _ if reason_token(err).as_deref() == Some(token::LEASE_CONTENTION_TIMEOUT) => {
            SettleErrorClass::Transient
        }
        _ => SettleErrorClass::Permanent,
    }
}

/// Settle attempts stop this long before the lease deadline: a call that late
/// would only meet an expired lease (`LEASE_NOT_ACTIVE`). Quota Enforcement
/// releases the lease on its own.
pub(crate) const SETTLE_SAFETY_LEAD: Duration = Duration::from_secs(2);

/// Default jitter of the settle retry backoff (+-20%).
const DEFAULT_BACKOFF_JITTER: f64 = 0.2;

struct Inner {
    client: Arc<dyn QuotaEnforcementClientV1>,
    identity: Arc<dyn ServiceIdentity>,
    sink: Arc<dyn UsageSink>,
    metric: String,
    units_per_credit: u64,
    lease_ttl_margin: Duration,
    fallback_max_output_tokens: u64,
    settle_retry_backoff: Duration,
    /// Uniform jitter of the backoff, as a fraction (`0.2` = +-20%).
    backoff_jitter: f64,
    provider_sync: Duration,
    stream_max_duration: Duration,
    /// `timeouts.dependency.quota_enforcement`: bound of one `acquire_lease`
    /// (`quota_check_unavailable`), `commit_lease` or `release_lease`
    /// (transient: retried with backoff).
    call_timeout: Duration,
    /// Every detached settle/release job, so shutdown can wait for them.
    jobs: TaskTracker,
}

/// The Quota Guard. Cheap to clone.
#[domain_model]
#[derive(Clone)]
pub struct QuotaGuard {
    inner: Arc<Inner>,
}

impl QuotaGuard {
    /// Build the guard from its ports and configuration.
    #[must_use]
    pub fn new(
        client: Arc<dyn QuotaEnforcementClientV1>,
        identity: Arc<dyn ServiceIdentity>,
        sink: Arc<dyn UsageSink>,
        quota: &QuotaConfig,
        timeouts: &TimeoutsConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                identity,
                sink,
                metric: quota.metric.clone(),
                units_per_credit: quota.units_per_credit,
                lease_ttl_margin: quota.lease_ttl_margin,
                fallback_max_output_tokens: quota.fallback_max_output_tokens,
                settle_retry_backoff: quota.settle_retry_backoff,
                backoff_jitter: DEFAULT_BACKOFF_JITTER,
                provider_sync: timeouts.provider_sync,
                stream_max_duration: timeouts.stream_max_duration,
                call_timeout: timeouts.dependency.quota_enforcement,
                jobs: TaskTracker::new(),
            }),
        }
    }

    /// Replace the backoff jitter fraction (`0.0` disables it). Only before
    /// the guard is cloned.
    #[must_use]
    pub fn with_backoff_jitter(mut self, fraction: f64) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.backoff_jitter = fraction.clamp(0.0, 1.0);
        }
        self
    }

    /// The settle retry backoff with its jitter applied.
    fn backoff(&self) -> Duration {
        let base = self.inner.settle_retry_backoff;
        let fraction = self.inner.backoff_jitter;
        if fraction <= 0.0 {
            return base;
        }
        let factor = rand::rng().random_range((1.0 - fraction)..=(1.0 + fraction));
        base.mul_f64(factor)
    }

    /// Pricing pre-check: the rates `op` requires, from the `pricing(model)`
    /// hook. Fail-closed: a missing or unparseable required rate is
    /// `internal` (logged with the model and provider type).
    ///
    /// # Errors
    ///
    /// `internal` when a required rate is missing.
    pub fn check_pricing(
        rules: &dyn ProviderRules,
        model: &ModelInfoV1,
        op: Operation,
    ) -> Result<Pricing, LlmGatewayError> {
        let rates = rules.pricing(model);
        let priced = match op {
            Operation::Response | Operation::Stream => rates
                .input_per_1k_micro
                .zip(rates.output_per_1k_micro)
                .map(|(input, output)| Pricing {
                    input_per_1k_micro: input,
                    output_per_1k_micro: output,
                }),
            // The output rate is not part of embeddings.
            Operation::Embedding => rates.input_per_1k_micro.map(|input| Pricing {
                input_per_1k_micro: input,
                output_per_1k_micro: 0,
            }),
        };
        priced.ok_or_else(|| {
            tracing::error!(
                model = %model.provider_model_id,
                provider_type = %model.gts_type.as_ref(),
                operation = ?op,
                "model pricing is missing or unparseable (provider_settings.cost): \
                 model configuration error"
            );
            LlmGatewayError::internal("model pricing is not configured")
        })
    }

    /// Upper-bound quote of a create-response request.
    #[must_use]
    pub fn response_quote(
        &self,
        pricing: Pricing,
        model: &ModelInfoV1,
        body: &CreateResponseBody,
    ) -> Quote {
        let input_tokens = estimate_input_tokens(body);
        let output_tokens = max_output_tokens(body, model, self.inner.fallback_max_output_tokens);
        self.quote(pricing, input_tokens, output_tokens)
    }

    /// Upper-bound quote of an embeddings request (input only).
    #[must_use]
    pub fn embedding_quote(&self, pricing: Pricing, req: &EmbeddingRequest) -> Quote {
        self.quote(pricing, estimate_embedding_tokens(req), 0)
    }

    fn quote(&self, pricing: Pricing, input_tokens: u64, output_tokens: u64) -> Quote {
        let cost = credits(&pricing, input_tokens, output_tokens);
        Quote {
            pricing,
            input_tokens,
            reserved_units: to_units(cost, self.inner.units_per_credit),
        }
    }

    /// Quota admission: acquire the lease of `quote` under the caller's
    /// `ctx`. A zero quote, or `NO_APPLICABLE_QUOTA`, admits without a lease.
    ///
    /// # Errors
    ///
    /// `quota_exceeded`, `quota_check_unavailable`, `permission_denied` or
    /// `internal` per the outcome mapping; the provider must not be called.
    pub async fn admit(
        &self,
        ctx: &SecurityContext,
        meta: RequestMeta,
        op: Operation,
        quote: &Quote,
    ) -> Result<RequestQuota, LlmGatewayError> {
        let owner = Owner {
            tenant_id: ctx.subject_tenant_id(),
            user_id: ctx.subject_id(),
        };
        if quote.reserved_units <= 0 {
            return Ok(RequestQuota::new(self.clone(), meta, owner, *quote, None));
        }
        let base = match op {
            Operation::Response | Operation::Embedding => self.inner.provider_sync,
            Operation::Stream => self.inner.stream_max_duration,
        };
        let ttl = base.saturating_add(self.inner.lease_ttl_margin);
        let metadata = serde_json::json!({
            "model": meta.model,
            "provider_type": meta.provider_type,
        });
        let request = AcquireLeaseRequest {
            attribution: EvaluationAttribution {
                tenant_id: TenantId::from(owner.tenant_id),
                metric: self.inner.metric.clone(),
                subjects: vec![SubjectClaim {
                    kind: SCOPE_USER.to_owned(),
                    id: owner.user_id.to_string(),
                }],
                metadata: metadata.as_object().cloned(),
                resource: None,
            },
            amount: quote.reserved_units,
            ttl_secs: Some(ttl.as_secs()),
            idempotency_key: format!("{}:acquire", meta.response_id),
        };
        let outcome = self.acquire(ctx, request, &meta.response_id).await?;
        let lease = match outcome {
            AcquireLeaseOutcome::Acquired { token, expires_at } => {
                let remaining = expires_at - time::OffsetDateTime::now_utc();
                let remaining = Duration::try_from(remaining).unwrap_or(Duration::ZERO);
                Some(Lease {
                    token,
                    deadline: tokio::time::Instant::now() + remaining,
                    reserved_units: quote.reserved_units,
                })
            }
            AcquireLeaseOutcome::Denied { decision } => {
                let DecisionResult::Denied {
                    violated_quota_ids,
                    reason,
                } = &decision.result
                else {
                    tracing::error!(
                        response_id = %meta.response_id,
                        "quota admission failed: lease denied without a denial decision"
                    );
                    return Err(LlmGatewayError::internal("quota admission failed"));
                };
                if reason == NO_APPLICABLE_QUOTA {
                    // Design choice: no quota applies, nothing to hold.
                    None
                } else {
                    let ids: Vec<String> =
                        violated_quota_ids.iter().map(ToString::to_string).collect();
                    tracing::info!(
                        response_id = %meta.response_id,
                        reason = %reason,
                        violated_quota_ids = ?ids,
                        "quota admission denied"
                    );
                    return Err(LlmGatewayError::QuotaExceeded);
                }
            }
        };
        Ok(RequestQuota::new(self.clone(), meta, owner, *quote, lease))
    }

    /// One `acquire_lease` call, bounded by `timeouts.dependency.quota_enforcement`
    /// (expiry: `quota_check_unavailable`, fail closed).
    async fn acquire(
        &self,
        ctx: &SecurityContext,
        request: AcquireLeaseRequest,
        response_id: &str,
    ) -> Result<AcquireLeaseOutcome, LlmGatewayError> {
        let call = self.inner.client.acquire_lease(ctx, request);
        match tokio::time::timeout(self.inner.call_timeout, call).await {
            Ok(result) => result.map_err(|err| {
                let mapped = map_acquire_error(&err);
                log_acquire_error(response_id, &err, &mapped);
                mapped
            }),
            Err(_elapsed) => {
                tracing::warn!(
                    response_id,
                    timeout_ms = self.inner.call_timeout.as_millis(),
                    "quota admission failed: acquire_lease timed out"
                );
                Err(LlmGatewayError::QuotaCheckUnavailable)
            }
        }
    }

    /// Hand a settle (or a release) to a background task. Never blocks the
    /// caller; without a runtime the work cannot run and is logged.
    fn spawn(&self, job: Job) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                response_id = %job.meta.response_id,
                "quota settle skipped: no async runtime (an unsettled lease expires on its own)"
            );
            return;
        };
        let guard = self.clone();
        let span = tracing::Span::current();
        runtime.spawn(
            self.inner
                .jobs
                .track_future(async move { guard.run(job).await }.instrument(span)),
        );
    }

    /// Resolves once every settle/release job handed off so far has finished
    /// (its usage was passed to the sink). Call it once the producers
    /// (requests in flight) are done: jobs spawned meanwhile are awaited too,
    /// jobs spawned after it returns are not.
    pub async fn settled(&self) {
        let jobs = &self.inner.jobs;
        jobs.close();
        jobs.wait().await;
        jobs.reopen();
    }

    async fn run(&self, job: Job) {
        let Job {
            meta,
            owner,
            lease,
            usage,
            terminal_at,
            provider_request_id,
        } = job;
        let actual_units = usage
            .as_ref()
            .map_or(0, |usage| to_units(usage.cost, self.inner.units_per_credit));
        if let Some(lease) = lease {
            // No usage: the request never dispatched, so the lease goes back.
            let amount = usage.as_ref().map(|_| actual_units);
            self.settle_lease(&meta, owner, lease, amount).await;
        }
        if let Some(usage) = usage {
            self.inner
                .sink
                .record(UsageReport {
                    response_id: meta.response_id,
                    tenant_id: owner.tenant_id,
                    user_id: owner.user_id,
                    model: meta.model,
                    provider_type: meta.provider_type,
                    usage,
                    quantity_micro_credits: actual_units,
                    terminal_at,
                    provider_request_id,
                })
                .await;
        }
    }

    /// Commit (`amount > 0`) or release (`amount` 0 or `None`) `lease`,
    /// retrying transient failures until shortly before the lease expires
    /// ([`SETTLE_SAFETY_LEAD`]).
    async fn settle_lease(
        &self,
        meta: &RequestMeta,
        owner: Owner,
        lease: Lease,
        amount: Option<i64>,
    ) {
        let response_id = meta.response_id.as_str();
        let commit_amount = amount.filter(|units| *units > 0);
        if let Some(actual) = commit_amount {
            log_overrun(response_id, lease.reserved_units, actual);
        }
        let attempt = SettleAttempt {
            response_id,
            tenant_id: TenantId::from(owner.tenant_id),
            lease,
            commit_amount,
            key: format!("{response_id}:settle"),
        };
        let mut reauthenticated = false;
        loop {
            if tokio::time::Instant::now() + SETTLE_SAFETY_LEAD >= lease.deadline {
                tracing::warn!(
                    response_id,
                    "quota settle abandoned: the lease expired or is about to expire; Quota \
                     Enforcement releases it on its own"
                );
                return;
            }
            match self.settle_once(&attempt, &mut reauthenticated).await {
                Next::Stop => return,
                Next::RetryNow => {}
                Next::RetryAfterBackoff => {
                    tokio::time::sleep(self.backoff()).await;
                }
            }
        }
    }

    /// One settle call under the service identity.
    async fn settle_once(&self, attempt: &SettleAttempt<'_>, reauthenticated: &mut bool) -> Next {
        let response_id = attempt.response_id;
        let IdentityContext { ctx, generation } = match self.inner.identity.context().await {
            Ok(identity) => identity,
            Err(err) => {
                tracing::warn!(
                    response_id,
                    error = %format!("{err:#}"),
                    "credentials exchange failed; quota settle will be retried"
                );
                return Next::RetryAfterBackoff;
            }
        };
        let client = &self.inner.client;
        let call = async {
            match attempt.commit_amount {
                Some(actual) => client
                    .commit_lease(
                        &ctx,
                        CommitLeaseRequest {
                            tenant_id: attempt.tenant_id,
                            token: attempt.lease.token,
                            actual_amount: Some(actual.min(attempt.lease.reserved_units)),
                            idempotency_key: attempt.key.clone(),
                        },
                    )
                    .await
                    .map(|_decision| ()),
                None => client
                    .release_lease(
                        &ctx,
                        ReleaseLeaseRequest {
                            tenant_id: attempt.tenant_id,
                            token: attempt.lease.token,
                            idempotency_key: attempt.key.clone(),
                        },
                    )
                    .await
                    .map(|_decision| ()),
            }
        };
        let result = match tokio::time::timeout(self.inner.call_timeout, call).await {
            Ok(result) => result,
            Err(_elapsed) => {
                // Transient: the same amount and key are retried.
                tracing::warn!(
                    response_id,
                    timeout_ms = self.inner.call_timeout.as_millis(),
                    "quota settle call timed out; retrying with backoff"
                );
                return Next::RetryAfterBackoff;
            }
        };
        match result {
            Ok(()) => Next::Stop,
            Err(err) => {
                self.on_settle_error(response_id, &err, generation, reauthenticated)
                    .await
            }
        }
    }

    async fn on_settle_error(
        &self,
        response_id: &str,
        err: &QuotaEnforcementError,
        generation: u64,
        reauthenticated: &mut bool,
    ) -> Next {
        match classify_settle_error(err) {
            SettleErrorClass::Transient => {
                log_settle_retry(response_id, err);
                Next::RetryAfterBackoff
            }
            SettleErrorClass::Unauthenticated => {
                // Only the generation that was refused is dropped: a newer
                // exchange (another settle's) is kept. The adapter also
                // rate-limits exchanges, so a persistent refusal cannot
                // hammer the `IdP`.
                self.inner.identity.invalidate(generation).await;
                // The first refusal retries at once with a fresh context; a
                // refusal of a fresh context backs off.
                if std::mem::replace(reauthenticated, true) {
                    Next::RetryAfterBackoff
                } else {
                    Next::RetryNow
                }
            }
            SettleErrorClass::Permanent => {
                log_settle_abandoned(response_id, err);
                Next::Stop
            }
        }
    }
}

fn log_settle_retry(response_id: &str, err: &QuotaEnforcementError) {
    tracing::warn!(
        response_id,
        reason = ?reason_token(err),
        "quota settle failed transiently; retrying with backoff"
    );
}

fn log_settle_abandoned(response_id: &str, err: &QuotaEnforcementError) {
    tracing::error!(
        response_id,
        reason = ?reason_token(err),
        error = %err.detail(),
        "quota settle failed permanently; abandoned (the lease expires on its own)"
    );
}

/// What to do after a settle call.
enum Next {
    Stop,
    RetryNow,
    RetryAfterBackoff,
}

/// The fixed parts of one settle: same key and amount on every retry.
struct SettleAttempt<'a> {
    response_id: &'a str,
    tenant_id: TenantId,
    lease: Lease,
    commit_amount: Option<i64>,
    key: String,
}

fn log_overrun(response_id: &str, reserved_units: i64, actual_units: i64) {
    if actual_units > reserved_units {
        tracing::warn!(
            response_id,
            reserved_units,
            actual_units,
            "overrun: the actual cost exceeds the reserved amount; committing the reserved amount"
        );
    }
}

fn log_acquire_error(response_id: &str, err: &QuotaEnforcementError, mapped: &LlmGatewayError) {
    let reason = reason_token(err);
    if matches!(mapped, LlmGatewayError::Internal { .. }) {
        tracing::error!(
            response_id,
            reason = ?reason,
            error = %err.detail(),
            "quota admission failed: configuration or contract fault"
        );
    } else {
        tracing::warn!(
            response_id,
            reason = ?reason,
            error = %err.detail(),
            code = mapped.code(),
            "quota admission failed"
        );
    }
}

/// Tenant and user of the caller, for attribution and the usage report.
#[domain_model]
#[derive(Debug, Clone, Copy)]
struct Owner {
    tenant_id: Uuid,
    user_id: Uuid,
}

/// One unit of background work: settle the lease (if any) and report.
struct Job {
    meta: RequestMeta,
    owner: Owner,
    lease: Option<Lease>,
    /// `Some`: dispatched, settle for this usage and report it. `None`: not
    /// dispatched, release the lease and report nothing.
    usage: Option<Usage>,
    terminal_at: SystemTime,
    /// Provider request id of the plugin outcome, for the usage report.
    provider_request_id: Option<String>,
}

/// An acquired lease.
#[domain_model]
#[derive(Debug, Clone, Copy)]
struct Lease {
    token: LeaseToken,
    /// When the hold lapses, on the tokio clock.
    deadline: tokio::time::Instant,
    reserved_units: i64,
}

/// Per-request quota state: the optional lease plus what settle needs.
///
/// Dropping it without [`RequestQuota::settle`] settles it anyway: before
/// dispatch the lease is released, after dispatch it is settled with the
/// input-only estimate (the request was cut, e.g. consumer disconnect).
#[domain_model]
pub struct RequestQuota {
    guard: QuotaGuard,
    meta: RequestMeta,
    owner: Owner,
    quote: Quote,
    lease: Option<Lease>,
    dispatched: bool,
    /// Settle or release already handed to the background.
    done: bool,
    /// Provider request id captured from the plugin outcome, for the usage
    /// report.
    provider_request_id: Option<String>,
}

impl RequestQuota {
    fn new(
        guard: QuotaGuard,
        meta: RequestMeta,
        owner: Owner,
        quote: Quote,
        lease: Option<Lease>,
    ) -> Self {
        Self {
            guard,
            meta,
            owner,
            quote,
            lease,
            dispatched: false,
            done: false,
            provider_request_id: None,
        }
    }

    /// Record the provider request id of the plugin outcome (response,
    /// terminal snapshot or provider error) so the usage report carries it.
    pub fn set_provider_request_id(&mut self, provider_request_id: Option<String>) {
        self.provider_request_id = provider_request_id;
    }

    /// Whether admission acquired a lease.
    #[must_use]
    pub fn has_lease(&self) -> bool {
        self.lease.is_some()
    }

    /// The estimator for requests that end without final provider usage.
    #[must_use]
    pub fn estimator(&self) -> UsageEstimator {
        UsageEstimator {
            input_tokens: self.quote.input_tokens,
            pricing: self.quote.pricing,
        }
    }

    /// Mark the request as dispatched to the plugin.
    pub fn mark_dispatched(&mut self) {
        self.dispatched = true;
    }

    /// Settle at a terminal state of a dispatched request, in the background.
    pub fn settle(mut self, usage: Usage) {
        self.hand_over(Some(usage));
    }

    /// Release the lease of a request that failed before dispatch, in the
    /// background. No usage is reported.
    pub fn release(mut self) {
        self.hand_over(None);
    }

    fn hand_over(&mut self, usage: Option<Usage>) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        if usage.is_none() && self.lease.is_none() {
            return;
        }
        self.guard.spawn(Job {
            meta: self.meta.clone(),
            owner: self.owner,
            lease: self.lease,
            usage,
            terminal_at: SystemTime::now(),
            provider_request_id: self.provider_request_id.clone(),
        });
    }
}

impl Drop for RequestQuota {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let usage = self.dispatched.then(|| self.estimator().estimate(0, None));
        self.hand_over(usage);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "quota_guard_tests.rs"]
mod quota_guard_tests;

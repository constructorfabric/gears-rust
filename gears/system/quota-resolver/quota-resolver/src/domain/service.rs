//! Domain service of the quota resolver gear.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use quota_resolver_sdk::models::{
    DenialReason, LeaseOutcome, MetricItem, PlatformAcquireLeaseRequest,
    PlatformCommitLeaseRequest, PlatformDebitRequest, PlatformQuotaStateRequest,
    PlatformReleaseLeaseRequest, PlatformReturnRequest, PluginAcquireLeaseRequest,
    PluginDebitRequest, PluginQuotaStateRequest, PrincipalSubject, QuotaDecision, QuotaState,
    QuotaStateMetric, QuotaStateRequest, QuotaSubject, SubjectInput, caller_tenant_id,
};
use quota_resolver_sdk::{
    QUOTA_TENANT_SUBJECT_TYPE_ID, QuotaResolverPluginClient, QuotaResolverPluginSpecV1,
};
use toolkit::Page;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{
    ChoosePluginError, GtsPluginSelector, choose_registered_plugin_instance, is_discovery_outage,
};
use toolkit::telemetry::ThrottledLog;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_macros::domain_model;
use toolkit_security::{PlatformIdentity, PlatformSecurityContext, SecurityContext};
use tracing::{info, warn};
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};
use uuid::Uuid;

use super::mapper::PrincipalMapper;
use super::ports::{Operation, Outcome, Plane, QuotaMetrics};

const MAX_STATE_METRICS: usize = 100;
const WARNING_THROTTLE: Duration = Duration::from_secs(10);

#[resource_error(gts_id!("cf.core.quota.request.v1~"))]
struct RequestError;

fn invalid(field: &str, description: &str, reason: &str) -> CanonicalError {
    RequestError::invalid_argument()
        .with_field_violation(field, description, reason)
        .create()
}

/// Bounds the plugin selection, including the wait of the calls that queue
/// behind the one that resolves it. Expiry is a retryable outage and caches
/// nothing.
const DISCOVERY_BUDGET: Duration = Duration::from_secs(30);

/// A types-registry error as the resolver's caller sees it: an outage is a
/// retryable `ServiceUnavailable`; a refused credential or a malformed answer
/// is a fault that a retry does not pass.
pub(super) fn registry_failure(error: &CanonicalError) -> CanonicalError {
    if is_discovery_outage(error) {
        unavailable(format!("types-registry is not available: {error}"))
    } else {
        CanonicalError::internal(format!("types-registry: {error}")).create()
    }
}

fn unavailable(detail: impl Into<String>) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(detail)
        .create()
}

/// Subjects of a request after mapping, with the first principal that was mapped.
struct Mapped {
    subjects: Vec<QuotaSubject>,
    principal: Option<MappedPrincipal>,
}

struct MappedPrincipal {
    principal_type: String,
    subject_type: String,
}

type Plugin = Arc<dyn QuotaResolverPluginClient>;

/// Quota resolver service: maps subjects, selects the backend plugin by vendor
/// and priority, and delegates. It never builds an allowed result.
#[domain_model]
pub struct Service {
    hub: Arc<ClientHub>,
    vendor: String,
    mapper: PrincipalMapper,
    metrics: Arc<dyn QuotaMetrics>,
    selector: GtsPluginSelector,
    mapping_warning: ThrottledLog,
    /// A deployment without a backend fails every call: one line per period.
    plugin_warning: ThrottledLog,
}

impl Service {
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        vendor: String,
        principal_subject_types: BTreeMap<String, String>,
        metrics: Arc<dyn QuotaMetrics>,
    ) -> Self {
        Self {
            hub,
            vendor,
            mapper: PrincipalMapper::new(principal_subject_types),
            metrics,
            selector: GtsPluginSelector::new(),
            mapping_warning: ThrottledLog::new(WARNING_THROTTLE),
            plugin_warning: ThrottledLog::new(WARNING_THROTTLE),
        }
    }

    /// Charge the metric items against the subjects.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a malformed request, `ServiceUnavailable` when no
    /// plugin or no types-registry answers, otherwise the plugin's error.
    pub async fn debit(
        &self,
        request: PlatformDebitRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<QuotaDecision, CanonicalError> {
        let started = Instant::now();
        let result = async {
            check_charge(&request.subjects, &request.items, request.tenant_id)?;
            let Some(mapped) = self.map_subjects(request.subjects).await? else {
                self.metrics.record_overhead_ms(elapsed_ms(started));
                return Ok((
                    QuotaDecision::denied(DenialReason::PrincipalNotMapped),
                    Outcome::PrincipalNotMapped,
                ));
            };
            let plugin = self.select_plugin().await?;
            self.metrics.record_overhead_ms(elapsed_ms(started));
            let result = plugin
                .debit(PluginDebitRequest {
                    tenant_id: request.tenant_id,
                    subjects: mapped.subjects,
                    items: request.items,
                    idempotency_key: request.idempotency_key,
                    caller,
                })
                .await;
            self.warn_if_rejected(&result, mapped.principal.as_ref());
            result.map(|decision| {
                let outcome = decision_outcome(&decision);
                (decision, outcome)
            })
        }
        .await;
        self.finish(Operation::Debit, Plane::Platform, result)
    }

    /// Hold capacity for a bounded time.
    ///
    /// # Errors
    ///
    /// As [`Self::debit`].
    pub async fn acquire_lease(
        &self,
        request: PlatformAcquireLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<LeaseOutcome, CanonicalError> {
        let started = Instant::now();
        let result = async {
            check_charge(&request.subjects, &request.items, request.tenant_id)?;
            let Some(mapped) = self.map_subjects(request.subjects).await? else {
                self.metrics.record_overhead_ms(elapsed_ms(started));
                return Ok((
                    LeaseOutcome::Denied {
                        decision: QuotaDecision::denied(DenialReason::PrincipalNotMapped),
                    },
                    Outcome::PrincipalNotMapped,
                ));
            };
            let plugin = self.select_plugin().await?;
            self.metrics.record_overhead_ms(elapsed_ms(started));
            let result = plugin
                .acquire_lease(PluginAcquireLeaseRequest {
                    tenant_id: request.tenant_id,
                    subjects: mapped.subjects,
                    items: request.items,
                    ttl_secs: request.ttl_secs,
                    idempotency_key: request.idempotency_key,
                    caller,
                })
                .await;
            self.warn_if_rejected(&result, mapped.principal.as_ref());
            result.map(|outcome| {
                let label = match &outcome {
                    LeaseOutcome::Acquired { .. } => Outcome::Allowed,
                    _ => Outcome::Denied,
                };
                (outcome, label)
            })
        }
        .await;
        self.finish(Operation::AcquireLease, Plane::Platform, result)
    }

    /// Settle a lease for the amounts actually used.
    ///
    /// # Errors
    ///
    /// `ServiceUnavailable` when no plugin or no types-registry answers,
    /// otherwise the plugin's error.
    pub async fn commit_lease(
        &self,
        request: PlatformCommitLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        let started = Instant::now();
        let result = async {
            let plugin = self.select_plugin().await?;
            self.metrics.record_overhead_ms(elapsed_ms(started));
            plugin
                .commit_lease(request, caller)
                .await
                .map(|()| ((), Outcome::Allowed))
        }
        .await;
        self.finish(Operation::CommitLease, Plane::Platform, result)
    }

    /// Release a lease without charging.
    ///
    /// # Errors
    ///
    /// As [`Self::commit_lease`].
    pub async fn release_lease(
        &self,
        request: PlatformReleaseLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        let started = Instant::now();
        let result = async {
            let plugin = self.select_plugin().await?;
            self.metrics.record_overhead_ms(elapsed_ms(started));
            plugin
                .release_lease(request, caller)
                .await
                .map(|()| ((), Outcome::Allowed))
        }
        .await;
        self.finish(Operation::ReleaseLease, Plane::Platform, result)
    }

    /// Give back amounts of an earlier charge.
    ///
    /// # Errors
    ///
    /// As [`Self::commit_lease`].
    pub async fn return_amount(
        &self,
        request: PlatformReturnRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        let started = Instant::now();
        let result = async {
            let plugin = self.select_plugin().await?;
            self.metrics.record_overhead_ms(elapsed_ms(started));
            plugin
                .return_amount(request, caller)
                .await
                .map(|()| ((), Outcome::Allowed))
        }
        .await;
        self.finish(Operation::ReturnAmount, Plane::Platform, result)
    }

    /// Read the quota state of one subject in a tenant.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a malformed request, `ServiceUnavailable` when no
    /// plugin or no types-registry answers, otherwise the plugin's error.
    pub async fn quota_state(
        &self,
        request: PlatformQuotaStateRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        let started = Instant::now();
        let result = async {
            check_state_metrics(&request.metrics)?;
            self.read_state(request, caller, started).await
        }
        .await;
        self.finish(Operation::QuotaState, Plane::Platform, result)
    }

    /// Read the quota state of the caller's principal in the caller's tenant.
    ///
    /// # Errors
    ///
    /// As [`Self::quota_state`].
    pub async fn tenant_quota_state(
        &self,
        ctx: &SecurityContext,
        request: QuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        let started = Instant::now();
        let result = async {
            check_state_metrics(&request.metrics)?;
            let tenant_id = caller_tenant_id(ctx);
            match PrincipalSubject::from_security_context(ctx) {
                Some(principal) if !tenant_id.is_nil() => {
                    let request = PlatformQuotaStateRequest {
                        tenant_id,
                        subject: SubjectInput::Principal(principal),
                        metrics: request.metrics,
                        limit: request.limit,
                        cursor: request.cursor,
                    };
                    self.read_state(request, None, started).await
                }
                _ => {
                    self.metrics.record_overhead_ms(elapsed_ms(started));
                    Ok((empty_page(request.limit), Outcome::PrincipalNotMapped))
                }
            }
        }
        .await;
        self.finish(Operation::QuotaState, Plane::Tenant, result)
    }

    async fn read_state(
        &self,
        request: PlatformQuotaStateRequest,
        caller: Option<PlatformIdentity>,
        started: Instant,
    ) -> Result<(Page<QuotaState>, Outcome), CanonicalError> {
        check_subject(&request.subject, request.tenant_id, "subject.id")?;
        let Some(mut mapped) = self.map_subjects(vec![request.subject]).await? else {
            self.metrics.record_overhead_ms(elapsed_ms(started));
            return Ok((empty_page(request.limit), Outcome::PrincipalNotMapped));
        };
        let plugin = self.select_plugin().await?;
        self.metrics.record_overhead_ms(elapsed_ms(started));
        let page = plugin
            .quota_state(PluginQuotaStateRequest {
                tenant_id: request.tenant_id,
                subject: mapped.subjects.remove(0),
                metrics: request.metrics,
                limit: request.limit,
                cursor: request.cursor,
                caller,
            })
            .await?;
        Ok((page, Outcome::Allowed))
    }

    fn finish<T>(
        &self,
        operation: Operation,
        plane: Plane,
        result: Result<(T, Outcome), CanonicalError>,
    ) -> Result<T, CanonicalError> {
        let (value, outcome) = match result {
            Ok((value, outcome)) => (Ok(value), outcome),
            Err(err) => (Err(err), Outcome::Error),
        };
        self.metrics.record_operation(operation, plane, outcome);
        value
    }

    /// Map every subject; `None` when a principal has no quota subject type.
    async fn map_subjects(
        &self,
        inputs: Vec<SubjectInput>,
    ) -> Result<Option<Mapped>, CanonicalError> {
        let mut subjects = Vec::with_capacity(inputs.len());
        let mut principal = None;
        for input in inputs {
            match input {
                SubjectInput::Quota(subject) => subjects.push(subject),
                SubjectInput::Principal(p) => {
                    if p.id.is_nil() {
                        return Ok(None);
                    }
                    let Some(subject_type) = self
                        .mapper
                        .map(&self.hub, Some(p.subject_type.as_str()))
                        .await?
                    else {
                        return Ok(None);
                    };
                    subjects.push(QuotaSubject {
                        subject_type: subject_type.clone(),
                        id: p.id.to_string(),
                    });
                    principal.get_or_insert(MappedPrincipal {
                        principal_type: p.subject_type,
                        subject_type,
                    });
                }
            }
        }
        Ok(Some(Mapped {
            subjects,
            principal,
        }))
    }

    fn warn_if_rejected<T>(
        &self,
        result: &Result<T, CanonicalError>,
        principal: Option<&MappedPrincipal>,
    ) {
        if let (Err(error), Some(p)) = (result, principal)
            && names_subjects(error)
            && self.mapping_warning.should_log()
        {
            warn!(
                principal_type = %p.principal_type,
                quota_subject_type = %p.subject_type,
                "the backend rejected a request with a mapped principal; the quota subject type \
                 may not be admitted by the metric"
            );
        }
    }

    async fn select_plugin(&self) -> Result<Plugin, CanonicalError> {
        let instance_id = self
            .selector
            .get_or_init_with_timeout(DISCOVERY_BUDGET, || self.resolve_plugin(), unavailable)
            .await
            .inspect_err(|e| {
                if self.plugin_warning.should_log() {
                    warn!(vendor = %self.vendor, error = %e, "no quota plugin was resolved");
                }
            })?;
        self.hub
            .try_get_scoped::<dyn QuotaResolverPluginClient>(&ClientScope::gts_id(
                instance_id.as_ref(),
            ))
            .ok_or_else(|| {
                if self.plugin_warning.should_log() {
                    warn!(plugin_gts_id = %instance_id, "the selected quota plugin is not registered");
                }
                unavailable(format!("quota plugin `{instance_id}` is not registered"))
            })
    }

    async fn resolve_plugin(&self) -> Result<String, CanonicalError> {
        let registry = self
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| unavailable(format!("types-registry is not available: {e}")))?;
        let spec_id = QuotaResolverPluginSpecV1::gts_type_id();
        let pattern = GtsIdPattern::try_new(spec_id.as_ref())
            .map_err(|e| unavailable(format!("quota plugin type pattern: {e}")))?;
        let instances = registry
            .list_instances(
                &PlatformSecurityContext::outbound_marker(),
                ListEntitiesRequest::matching(pattern),
            )
            .await
            .map_err(|e| registry_failure(&e))?;
        choose_registered_plugin_instance::<QuotaResolverPluginSpecV1, dyn QuotaResolverPluginClient>(
            &self.hub,
            &self.vendor,
            instances
                .iter()
                .map(|instance| (instance.id.as_ref(), instance.content.as_ref())),
        )
        .inspect(|gts_id| info!(plugin_gts_id = %gts_id, "selected quota plugin instance"))
        .map_err(|e| match e {
            ChoosePluginError::PluginNotFound { .. } => unavailable(format!("no quota plugin: {e}")),
            ChoosePluginError::InvalidPluginInstance { .. } => {
                CanonicalError::internal(format!("quota plugin: {e}")).create()
            }
        })
    }
}

/// Whether `error` refuses the request for its subjects: the only part of it
/// that the mapping produced, so the only rejection that can point at a
/// mapping. An invalid amount or metric says nothing about it.
fn names_subjects(error: &CanonicalError) -> bool {
    matches!(
        error,
        CanonicalError::InvalidArgument {
            ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
            ..
        } if field_violations.iter().any(|v| v.field.starts_with("subjects"))
    )
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn empty_page(limit: Option<u32>) -> Page<QuotaState> {
    Page::empty(limit.map_or(0, u64::from))
}

fn decision_outcome(decision: &QuotaDecision) -> Outcome {
    if decision.allowed {
        Outcome::Allowed
    } else {
        Outcome::Denied
    }
}

fn check_charge(
    subjects: &[SubjectInput],
    items: &[MetricItem],
    tenant_id: Uuid,
) -> Result<(), CanonicalError> {
    if subjects.is_empty() {
        return Err(invalid(
            "subjects",
            "at least one subject is required",
            "NO_SUBJECT",
        ));
    }
    if items.is_empty() {
        return Err(invalid(
            "items",
            "at least one metric item is required",
            "NO_METRIC_ITEM",
        ));
    }
    subjects.iter().enumerate().try_for_each(|(at, subject)| {
        check_subject(subject, tenant_id, &format!("subjects[{at}].id"))
    })
}

fn check_state_metrics(metrics: &[QuotaStateMetric]) -> Result<(), CanonicalError> {
    if metrics.is_empty() {
        return Err(invalid(
            "metrics",
            "a quota state read names at least one metric",
            "NO_METRIC",
        ));
    }
    if metrics.len() > MAX_STATE_METRICS {
        return Err(invalid(
            "metrics",
            "at most 100 metrics can be read at once",
            "TOO_MANY_METRICS",
        ));
    }
    Ok(())
}

/// `field` is the subject's id as the caller sent it: `subjects[i].id` in a
/// charge, `subject.id` in a quota state read.
fn check_subject(
    subject: &SubjectInput,
    tenant_id: Uuid,
    field: &str,
) -> Result<(), CanonicalError> {
    let SubjectInput::Quota(quota) = subject else {
        return Ok(());
    };
    if quota.id.is_empty() {
        return Err(invalid(
            field,
            "a quota subject id must not be empty",
            "EMPTY_SUBJECT_ID",
        ));
    }
    if quota.subject_type == QUOTA_TENANT_SUBJECT_TYPE_ID && quota.id != tenant_id.to_string() {
        return Err(invalid(
            field,
            "a tenant subject must name the tenant of the request",
            "TENANT_SUBJECT_MISMATCH",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

//! The quota resolver plugin: `QuotaResolverPluginClient` over the trusted
//! operations (ADR-0009). The resolver fixes the tenant and the quota
//! subjects and runs no policy check; the plugin converts its requests and
//! results and passes every QE error back unchanged. It has no
//! `SecurityContext` and builds none.

use crate::domain::operations::ReturnRequest;
use std::sync::Arc;

use async_trait::async_trait;
use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, CommitLeaseRequest, DebitRequest, Decision,
    DecisionResult, EvaluationAttribution, LeaseToken, MetricRef, NO_APPLICABLE_QUOTA,
    QuotaSnapshot, ReleaseLeaseRequest, RollbackableOperation, SnapshotRequest, SnapshotSubject,
    SubjectClaim, TenantId,
};
use quota_resolver_sdk::QuotaResolverPluginClient;
use quota_resolver_sdk::models::{
    ChargeOperation, DenialReason, LeaseOutcome, MetricAmount, MetricItem, MetricObject,
    PlatformCommitLeaseRequest, PlatformReleaseLeaseRequest, PlatformReturnRequest,
    PluginAcquireLeaseRequest, PluginDebitRequest, PluginQuotaStateRequest, QuotaDecision,
    QuotaPeriod, QuotaState, QuotaStateMetric, QuotaSubject,
};
use toolkit::{Page, PageInfo};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformIdentity;
use uuid::Uuid;

use crate::domain::catalog::MetricCatalog;
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::trusted::TrustedOperations;
use crate::domain::{Service, tokens};
use crate::infra::canonical_mapping::{OperationResource, QuotaResource};

/// Diagnostic key of QE's own denial reason.
pub const DIAGNOSTIC_REASON: &str = "reason";
/// Diagnostic key of the ids of the violated Quotas.
pub const DIAGNOSTIC_VIOLATED_QUOTA_IDS: &str = "violated_quota_ids";

/// QE's quota resolver plugin.
pub(crate) struct ResolverPlugin {
    ops: TrustedOperations,
    page_size: u32,
}

impl ResolverPlugin {
    /// The plugin over the gear's service; `page_size` is the snapshot page
    /// size a quota state read gets when it names none.
    pub(crate) fn new(service: Arc<Service>, page_size: u32) -> Self {
        Self {
            ops: TrustedOperations::new(service),
            page_size,
        }
    }
}

// @cpt-flow:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-resolver-plugin:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-resolver-quota-state:p1
#[async_trait]
impl QuotaResolverPluginClient for ResolverPlugin {
    async fn debit(&self, request: PluginDebitRequest) -> Result<QuotaDecision, CanonicalError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-request
        let PluginDebitRequest {
            tenant_id,
            subjects,
            items,
            idempotency_key,
            caller,
        } = request;
        // @cpt-end:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-convert
        let sent = Sent::items(ITEM_FIELDS, &subjects);
        let (attribution, amount) = attribution_of(tenant_id, subjects, items)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-convert
        // @cpt-begin:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-run
        let decision = self
            .ops
            .debit(
                TenantId::new(tenant_id),
                caller,
                DebitRequest {
                    attribution,
                    amount,
                    idempotency_key,
                },
            )
            .await
            .map_err(|error| sent.name(error))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-run
        // @cpt-begin:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-result
        // @cpt-begin:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-return
        Ok(quota_decision(decision))
        // @cpt-end:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-return
        // @cpt-end:cpt-cf-quota-enforcement-flow-resolver-plugin-call:p1:inst-rpl-result
    }

    async fn acquire_lease(
        &self,
        request: PluginAcquireLeaseRequest,
    ) -> Result<LeaseOutcome, CanonicalError> {
        let PluginAcquireLeaseRequest {
            tenant_id,
            subjects,
            items,
            ttl_secs,
            idempotency_key,
            caller,
        } = request;
        let sent = Sent::items(ACQUIRE_FIELDS, &subjects);
        let (attribution, amount) = attribution_of(tenant_id, subjects, items)?;
        let outcome = self
            .ops
            .acquire_lease(
                TenantId::new(tenant_id),
                caller,
                AcquireLeaseRequest {
                    attribution,
                    amount,
                    ttl_secs: Some(ttl_secs),
                    idempotency_key,
                },
            )
            .await
            .map_err(|error| sent.name(error))?;
        lease_outcome(outcome)
    }

    async fn commit_lease(
        &self,
        request: PlatformCommitLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        let token = lease_token(&request.token)?;
        let amount = request.amounts.map(single_amount).transpose()?;
        self.ops
            .commit_lease(
                TenantId::new(request.tenant_id),
                caller,
                CommitLeaseRequest {
                    tenant_id: TenantId::new(request.tenant_id),
                    token,
                    actual_amount: amount.as_ref().map(|amount| amount.amount),
                    metric_type: amount.map(|amount| amount.metric_type),
                    idempotency_key: request.idempotency_key,
                },
            )
            .await
            .map_err(|error| Sent::amounts(COMMIT_FIELDS).name(error))?;
        Ok(())
    }

    async fn release_lease(
        &self,
        request: PlatformReleaseLeaseRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        let token = lease_token(&request.token)?;
        self.ops
            .release_lease(
                TenantId::new(request.tenant_id),
                caller,
                ReleaseLeaseRequest {
                    tenant_id: TenantId::new(request.tenant_id),
                    token,
                    idempotency_key: request.idempotency_key,
                },
            )
            .await?;
        Ok(())
    }

    async fn return_amount(
        &self,
        request: PlatformReturnRequest,
        caller: Option<PlatformIdentity>,
    ) -> Result<(), CanonicalError> {
        let amount = single_amount(request.amounts)?;
        let metric = MetricCatalog::parse_metric(&amount.metric_type).ok_or(
            DomainError::InvalidArgument {
                field: "amounts[0].metric_type",
                reason: tokens::METRIC_INVALID,
            },
        )?;
        self.ops
            .return_amount(
                TenantId::new(request.tenant_id),
                caller,
                ReturnRequest {
                    tenant_id: TenantId::new(request.tenant_id),
                    operation: match request.charge.operation {
                        ChargeOperation::Debit => RollbackableOperation::Debit,
                        ChargeOperation::LeaseCommit => RollbackableOperation::LeaseCommit,
                    },
                    charge_key: request.charge.idempotency_key,
                    metric,
                    amount: amount.amount,
                    idempotency_key: request.idempotency_key,
                },
            )
            .await
            .map_err(|error| Sent::amounts(RETURN_FIELDS).name(error))?;
        Ok(())
    }

    async fn quota_state(
        &self,
        request: PluginQuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        let limit = request.limit.unwrap_or(self.page_size);
        let sent: Vec<String> = request
            .metrics
            .iter()
            .map(|metric| metric.metric_type.clone())
            .collect();
        // @cpt-begin:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-read
        let page = self
            .ops
            .snapshot(
                TenantId::new(request.tenant_id),
                request.caller,
                snapshot_request(
                    request.tenant_id,
                    &request.subject,
                    request.metrics,
                    request.limit,
                    request.cursor,
                ),
            )
            .await
            .map_err(|error| state_error(error, &sent))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-read
        // @cpt-begin:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-return
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(quota_state)
                .collect::<Result<_, _>>()?,
            page_info: PageInfo {
                next_cursor: page.next_cursor,
                prev_cursor: None,
                limit: u64::from(limit),
            },
        })
        // @cpt-end:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-return
    }
}

/// QE names a field of its own request; the caller sent the resolver's. Each
/// pair maps a QE field to the resolver field the plugin converted it from
/// one to one, so an error names what the caller sent.
type FieldNames = &'static [(&'static str, &'static str)];

const ITEM_FIELDS: FieldNames = &[
    ("subject.type", "subjects"),
    ("amount", "items[0].amount"),
    ("metric", "items[0].metric"),
    ("metric.type", "items[0].metric.type"),
    ("metric.id", "items[0].metric.id"),
    ("metric.metadata", "items[0].metric.metadata"),
];

const ACQUIRE_FIELDS: FieldNames = &[
    ("subject.type", "subjects"),
    ("amount", "items[0].amount"),
    ("metric", "items[0].metric"),
    ("metric.type", "items[0].metric.type"),
    ("metric.id", "items[0].metric.id"),
    ("metric.metadata", "items[0].metric.metadata"),
    ("ttl", "ttl_secs"),
];

const COMMIT_FIELDS: FieldNames = &[
    ("actual_amount", "amounts[0].amount"),
    ("metric_type", "amounts[0].metric_type"),
];

const RETURN_FIELDS: FieldNames = &[
    ("amount", "amounts[0].amount"),
    ("charge_key", "charge.idempotency_key"),
];

/// `error` with its field named as in the resolver's request.
fn renamed(fields: FieldNames) -> impl Fn(DomainError) -> DomainError {
    move |error| match error {
        DomainError::InvalidArgument { field, reason } => DomainError::InvalidArgument {
            field: fields
                .iter()
                .find(|(qe, _)| *qe == field)
                .map_or(field, |(_, resolver)| *resolver),
            reason,
        },
        other => other,
    }
}

/// Where the resolver's request carries what an error of QE names. Errors
/// whose field QE fixes only when it lifts them are named here, so that they
/// too say what the resolver sent; a rejection for a subject names a field
/// under `subjects`, which is what the resolver looks for.
struct Sent {
    fields: FieldNames,
    /// The field that carries the metric type.
    metric: &'static str,
    /// The subject types, in the order the resolver sent them.
    subject_types: Vec<String>,
}

impl Sent {
    fn items(fields: FieldNames, subjects: &[QuotaSubject]) -> Self {
        Self {
            fields,
            metric: "items[0].metric.type",
            subject_types: subjects
                .iter()
                .map(|subject| subject.subject_type.clone())
                .collect(),
        }
    }

    fn amounts(fields: FieldNames) -> Self {
        Self {
            fields,
            metric: "amounts[0].metric_type",
            subject_types: Vec::new(),
        }
    }

    fn name(&self, error: DomainError) -> CanonicalError {
        match renamed(self.fields)(error) {
            DomainError::MetricNotRegistered { metric } => QuotaResource::invalid_argument()
                .with_field_violation(
                    self.metric,
                    format!("metric type {metric} is not registered"),
                    "METRIC_NOT_REGISTERED",
                )
                .with_resource(metric)
                .create(),
            DomainError::MetricTypeMismatch { requested, charged } => {
                OperationResource::invalid_argument()
                    .with_field_violation(
                        self.metric,
                        format!("metric type {requested} is not the charge's {charged}"),
                        "METRIC_TYPE_MISMATCH",
                    )
                    .create()
            }
            DomainError::ProjectionNotRegistered { projection } => {
                let field = self
                    .subject_types
                    .iter()
                    .position(|sent| *sent == projection)
                    .map_or_else(
                        || "subjects".to_owned(),
                        |at| format!("subjects[{at}].type"),
                    );
                QuotaResource::invalid_argument()
                    .with_field_violation(
                        field,
                        format!("subject type {projection} is not registered"),
                        "PROJECTION_NOT_REGISTERED",
                    )
                    .with_resource(projection)
                    .create()
            }
            other => other.into(),
        }
    }
}

/// An error of the snapshot read, named as the resolver's quota state request
/// names things: one metric per target, and one subject for all of them.
fn state_error(error: DomainError, metric_types: &[String]) -> CanonicalError {
    let violation = |field: String, reason: &str| {
        QuotaResource::invalid_argument()
            .with_field_violation(
                field.clone(),
                format!("invalid argument {field}: {reason}"),
                reason,
            )
            .create()
    };
    match error {
        DomainError::InvalidSnapshot {
            index,
            field,
            reason,
        } => {
            let field = match (index, field) {
                (Some(at), "metric") => format!("metrics[{at}].type"),
                (Some(at), "metadata") => format!("metrics[{at}].metadata"),
                (Some(_), of_subject) => format!("subject.{of_subject}"),
                (None, "subjects") => "metrics".to_owned(),
                (None, other) => other.to_owned(),
            };
            violation(field, reason)
        }
        DomainError::MetricNotRegistered { metric } => {
            let field = metric_types
                .iter()
                .position(|sent| *sent == metric)
                .map_or_else(|| "metrics".to_owned(), |at| format!("metrics[{at}].type"));
            QuotaResource::invalid_argument()
                .with_field_violation(
                    field,
                    format!("metric type {metric} is not registered"),
                    "METRIC_NOT_REGISTERED",
                )
                .with_resource(metric)
                .create()
        }
        other => other.into(),
    }
}

/// A commit or a return names its amounts in `amounts`, not in `items`.
fn one_amount_required() -> DomainError {
    DomainError::InvalidArgument {
        field: "amounts",
        reason: tokens::ONE_METRIC_ITEM_REQUIRED,
    }
}

fn one_item_required() -> DomainError {
    DomainError::InvalidArgument {
        field: "items",
        reason: tokens::ONE_METRIC_ITEM_REQUIRED,
    }
}

/// The QE attribution and amount of a debit or lease acquisition: the single
/// metric item's metric object and amount, the subjects as they are.
fn attribution_of(
    tenant_id: Uuid,
    subjects: Vec<QuotaSubject>,
    items: Vec<MetricItem>,
) -> Result<(EvaluationAttribution, i64), DomainError> {
    let [item] = <[MetricItem; 1]>::try_from(items).map_err(|_| one_item_required())?;
    let MetricObject {
        metric_type,
        id,
        metadata,
        extra,
    } = item.metric;
    // QE's metric object is closed: a field it does not know is refused, as
    // QE's API refuses it.
    if !extra.is_empty() {
        return Err(DomainError::InvalidArgument {
            field: "items[0].metric",
            reason: tokens::METRIC_INVALID,
        });
    }
    Ok((
        EvaluationAttribution {
            tenant_id: TenantId::new(tenant_id),
            subjects: subjects
                .into_iter()
                .map(|subject| SubjectClaim {
                    subject_type: subject.subject_type,
                    id: subject.id,
                })
                .collect(),
            metric: MetricRef {
                metric_type,
                id,
                metadata: Some(metadata),
            },
        },
        item.amount,
    ))
}

/// The single amount of a commit or return. An empty list is refused, so a commit that
/// sends one is not read as "commit the full held amount".
fn single_amount(amounts: Vec<MetricAmount>) -> Result<MetricAmount, DomainError> {
    let mut amounts = amounts.into_iter();
    match (amounts.next(), amounts.next()) {
        (Some(first), None) => Ok(first),
        _ => Err(one_amount_required()),
    }
}

/// A token that is not a UUID names no lease.
fn lease_token(token: &str) -> Result<LeaseToken, DomainError> {
    Uuid::parse_str(token)
        .map(LeaseToken::new)
        .map_err(|_| DomainError::NotFound {
            kind: ResourceKind::Lease,
            id: token.to_owned(),
        })
}

/// QE's Decision as the resolver's: the closed denial reason, QE's own
/// reason text and the violated Quotas in the diagnostics.
fn quota_decision(decision: Decision) -> QuotaDecision {
    let mut result = match decision.result {
        DecisionResult::Allowed => QuotaDecision::allowed(),
        DecisionResult::Denied {
            violated_quota_ids,
            reason,
        } => {
            let closed = if reason == NO_APPLICABLE_QUOTA {
                DenialReason::NoApplicableQuota
            } else {
                DenialReason::QuotaExceeded
            };
            QuotaDecision::denied(closed)
                .with_diagnostic(DIAGNOSTIC_REASON, reason)
                .with_diagnostic(
                    DIAGNOSTIC_VIOLATED_QUOTA_IDS,
                    violated_quota_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                )
        }
    };
    for (key, value) in decision.diagnostics {
        result.diagnostics.entry(key).or_insert(value);
    }
    result
}

fn lease_outcome(outcome: AcquireLeaseOutcome) -> Result<LeaseOutcome, CanonicalError> {
    Ok(match outcome {
        AcquireLeaseOutcome::Acquired { token, expires_at } => LeaseOutcome::Acquired {
            token: token.to_string(),
            expires_at: chrono_of(expires_at)?,
        },
        AcquireLeaseOutcome::Denied { decision } => LeaseOutcome::Denied {
            decision: quota_decision(decision),
        },
    })
}

fn chrono_of(at: time::OffsetDateTime) -> Result<chrono::DateTime<chrono::Utc>, CanonicalError> {
    chrono::DateTime::from_timestamp(at.unix_timestamp(), at.nanosecond()).ok_or_else(|| {
        CanonicalError::internal(format!("time {at} is out of the resolver's range")).create()
    })
}

/// One snapshot target per metric, all for the request's subject.
fn snapshot_request(
    tenant_id: Uuid,
    subject: &QuotaSubject,
    metrics: Vec<QuotaStateMetric>,
    limit: Option<u32>,
    cursor: Option<String>,
) -> SnapshotRequest {
    SnapshotRequest {
        tenant_id: TenantId::new(tenant_id),
        subjects: metrics
            .into_iter()
            .map(|metric| SnapshotSubject {
                subject_type: subject.subject_type.clone(),
                id: subject.id.clone(),
                metric: metric.metric_type,
                metadata: metric.metadata,
            })
            .collect(),
        limit,
        cursor,
    }
}

/// A Quota Snapshot as the resolver's quota state.
// @cpt-begin:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-map
// @cpt-begin:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-omit
fn quota_state(snapshot: QuotaSnapshot) -> Result<QuotaState, CanonicalError> {
    Ok(QuotaState {
        quota_id: snapshot.quota_id.to_string(),
        subject: QuotaSubject {
            subject_type: snapshot.subject.subject_type.to_string(),
            id: snapshot.subject.subject_id,
        },
        metric_type: snapshot.metric.as_str().to_owned(),
        cap: snapshot.cap,
        consumed: snapshot.consumed,
        remaining: snapshot.remaining,
        period: snapshot
            .period
            .map(|period| {
                Ok::<_, CanonicalError>(QuotaPeriod {
                    start: chrono_of(period.start)?,
                    resets_at: chrono_of(period.next_reset)?,
                })
            })
            .transpose()?,
        metadata: Some(snapshot.metadata).filter(|conditions| !conditions.is_empty()),
    })
}
// @cpt-end:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-omit
// @cpt-end:cpt-cf-quota-enforcement-algo-resolver-quota-state:p1:inst-rqs-map

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "resolver_plugin_tests.rs"]
mod resolver_plugin_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "resolver_plugin_gateway_tests.rs"]
mod resolver_plugin_gateway_tests;

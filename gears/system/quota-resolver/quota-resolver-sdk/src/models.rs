//! Value objects of the quota resolver contracts.
//!
//! [`QuotaSubject`] and [`PrincipalSubject`] reject unknown fields; requests and
//! results ignore them, so an older reader still reads a payload written by a
//! newer SDK.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use toolkit_security::{PlatformIdentity, SecurityContext};
use utoipa::ToSchema;
use uuid::Uuid;

fn non_empty<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        return Err(serde::de::Error::custom("id must not be empty"));
    }
    Ok(value)
}

/// Whom a quota applies to: a quota subject type and an opaque id. The only
/// subject form a plugin receives. Outside a platform request it has no `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaSubject)]
#[serde(deny_unknown_fields)]
pub struct QuotaSubject {
    /// Quota subject type: a GTS type id derived from `gts.cf.core.quota.subj.v1~`.
    #[serde(rename = "type")]
    pub subject_type: String,
    /// Opaque, non-empty subject id.
    #[serde(deserialize_with = "non_empty")]
    #[schema(min_length = 1)]
    pub id: String,
}

/// A subject given as a principal; the resolver replaces it with a
/// [`QuotaSubject`] of the mapped type and the same id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaPrincipalSubject)]
#[serde(deny_unknown_fields)]
pub struct PrincipalSubject {
    /// Principal type, as `SecurityContext::subject_type`.
    #[serde(rename = "type")]
    pub subject_type: String,
    /// Principal id.
    pub id: Uuid,
}

impl PrincipalSubject {
    /// The caller's principal, or `None` when the context has no subject type.
    /// Reads the context; it does not forward it.
    #[must_use]
    pub fn from_security_context(ctx: &SecurityContext) -> Option<Self> {
        Some(Self {
            subject_type: ctx.subject_type()?.to_owned(),
            id: ctx.subject_id(),
        })
    }
}

/// The caller's tenant, for a platform request.
#[must_use]
pub fn caller_tenant_id(ctx: &SecurityContext) -> Uuid {
    ctx.subject_tenant_id()
}

/// A subject in a platform request, told apart by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaSubjectInput)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubjectInput {
    /// A quota subject.
    Quota(QuotaSubject),
    /// A principal, mapped to a quota subject by the resolver.
    Principal(PrincipalSubject),
}

/// What a request charges: a metric type, an optional id, and metadata. Unknown
/// fields are kept and passed to the plugin unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = MetricObject)]
pub struct MetricObject {
    /// Metric type: a GTS type id derived from `gts.cf.core.quota.metric.v1~`.
    #[serde(rename = "type")]
    pub metric_type: String,
    /// The one thing the amount is counted for, for example a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Attributes of the operation that the backend's policies read.
    #[schema(value_type = Object)]
    pub metadata: Map<String, Value>,
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub extra: Map<String, Value>,
}

/// One metric of a debit or a lease acquisition, with its amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = MetricItem)]
pub struct MetricItem {
    /// What is charged.
    pub metric: MetricObject,
    /// Signed so that a non-positive amount reaches the backend, which rejects it.
    pub amount: i64,
}

/// An amount named by its metric type id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = MetricAmount)]
pub struct MetricAmount {
    /// Metric type id.
    pub metric_type: String,
    /// Signed, as [`MetricItem::amount`].
    pub amount: i64,
}

/// The operation that wrote a charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = ChargeOperation)]
#[serde(rename_all = "snake_case")]
pub enum ChargeOperation {
    /// An allowed debit.
    Debit,
    /// A lease commit.
    LeaseCommit,
}

/// A charge, named by its operation and the idempotency key of that call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = ChargeRef)]
pub struct ChargeRef {
    /// Operation that wrote the charge.
    pub operation: ChargeOperation,
    /// Idempotency key of that operation.
    pub idempotency_key: String,
}

/// Charge the metric items against the subjects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = PlatformDebitRequest)]
pub struct PlatformDebitRequest {
    /// The tenant the operation runs in.
    pub tenant_id: Uuid,
    /// At least one subject.
    pub subjects: Vec<SubjectInput>,
    /// At least one metric item; all are applied or none. Until the backend
    /// supports several, a request with more than one item is `InvalidArgument`.
    pub items: Vec<MetricItem>,
    /// Idempotency key of the call.
    pub idempotency_key: String,
}

/// Hold capacity for a bounded time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = PlatformAcquireLeaseRequest)]
pub struct PlatformAcquireLeaseRequest {
    /// The tenant the operation runs in.
    pub tenant_id: Uuid,
    /// At least one subject.
    pub subjects: Vec<SubjectInput>,
    /// At least one metric item.
    pub items: Vec<MetricItem>,
    /// Requested lease duration in seconds.
    pub ttl_secs: u64,
    /// Idempotency key of the call.
    pub idempotency_key: String,
}

/// Settle a lease for the amounts actually used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = PlatformCommitLeaseRequest)]
pub struct PlatformCommitLeaseRequest {
    /// The tenant of the lease.
    pub tenant_id: Uuid,
    /// The lease token.
    pub token: String,
    /// Actual amount of each metric of the lease; absent means the full held amounts. An empty
    /// list is refused, not read as absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amounts: Option<Vec<MetricAmount>>,
    /// Idempotency key of the call.
    pub idempotency_key: String,
}

/// Release a lease without charging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = PlatformReleaseLeaseRequest)]
pub struct PlatformReleaseLeaseRequest {
    /// The tenant of the lease.
    pub tenant_id: Uuid,
    /// The lease token.
    pub token: String,
    /// Idempotency key of the call.
    pub idempotency_key: String,
}

/// Give back amounts of an earlier charge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = PlatformReturnRequest)]
pub struct PlatformReturnRequest {
    /// The tenant of the charge.
    pub tenant_id: Uuid,
    /// The charge to return from.
    pub charge: ChargeRef,
    /// At least one amount to return.
    pub amounts: Vec<MetricAmount>,
    /// Idempotency key of the return.
    pub idempotency_key: String,
}

/// One metric of a quota state read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaStateMetric)]
pub struct QuotaStateMetric {
    /// Metric type id.
    #[serde(rename = "type")]
    pub metric_type: String,
    /// The `metadata` of a request's metric object. When present, the read
    /// returns for this metric only the quotas that apply to a request with
    /// this metadata. When absent, it returns the quotas whatever attributes
    /// of a request they are limited to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Map<String, Value>>,
}

impl QuotaStateMetric {
    /// The metric `metric_type`, with no request metadata.
    #[must_use]
    pub fn of(metric_type: impl Into<String>) -> Self {
        Self {
            metric_type: metric_type.into(),
            metadata: None,
        }
    }
}

/// Read the state of the quotas that apply to one subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = PlatformQuotaStateRequest)]
pub struct PlatformQuotaStateRequest {
    /// The tenant to read in.
    pub tenant_id: Uuid,
    /// Exactly one subject.
    pub subject: SubjectInput,
    /// The metrics to read, at most 100.
    pub metrics: Vec<QuotaStateMetric>,
    /// Page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Cursor of the page to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// The tenant-plane read. It has no subject and no tenant: both come from the
/// caller's `SecurityContext`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaStateRequest)]
pub struct QuotaStateRequest {
    /// The metrics to read, at most 100.
    pub metrics: Vec<QuotaStateMetric>,
    /// Page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Cursor of the page to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Why a debit or lease acquisition was denied. A caller treats a reason it does
/// not know as a denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = DenialReason)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DenialReason {
    /// The backend's quotas deny the request.
    QuotaExceeded,
    /// No quota applies to the request.
    NoApplicableQuota,
    /// A principal subject has no quota subject type. Set by the resolver.
    PrincipalNotMapped,
    /// A reason this SDK version does not know.
    #[serde(other)]
    Unknown,
}

/// The verdict of a debit or a lease acquisition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaDecision)]
#[non_exhaustive]
pub struct QuotaDecision {
    /// Whether the request is allowed.
    pub allowed: bool,
    /// The reason for a denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<DenialReason>,
    /// Non-authoritative diagnostics; never needed to interpret the verdict.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub diagnostics: HashMap<String, Value>,
}

impl QuotaDecision {
    /// An allowed verdict.
    #[must_use]
    pub fn allowed() -> Self {
        Self {
            allowed: true,
            reason: None,
            diagnostics: HashMap::new(),
        }
    }

    /// A denial.
    #[must_use]
    pub fn denied(reason: DenialReason) -> Self {
        Self {
            allowed: false,
            reason: Some(reason),
            diagnostics: HashMap::new(),
        }
    }

    /// Attach one diagnostic.
    #[must_use]
    pub fn with_diagnostic(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.diagnostics.insert(key.into(), value.into());
        self
    }
}

/// The result of a lease acquisition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = LeaseOutcome)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeaseOutcome {
    /// The lease is held until `expires_at`.
    Acquired {
        /// The lease token.
        token: String,
        /// When the lease expires.
        expires_at: DateTime<Utc>,
    },
    /// The acquisition was denied.
    Denied {
        /// The denial.
        decision: QuotaDecision,
    },
    /// An outcome this SDK version does not know. A caller treats it as no
    /// lease.
    #[serde(other)]
    Unknown,
}

/// The current period of a quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaStatePeriod)]
pub struct QuotaPeriod {
    /// Start of the period.
    pub start: DateTime<Utc>,
    /// When the period resets.
    pub resets_at: DateTime<Utc>,
}

/// The state of one quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = QuotaState)]
pub struct QuotaState {
    /// The quota id.
    pub quota_id: String,
    /// Whom the quota applies to.
    pub subject: QuotaSubject,
    /// Metric type id.
    pub metric_type: String,
    /// The cap, or none for an unbounded quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<u64>,
    /// Consumed amount.
    pub consumed: u64,
    /// Remaining amount, or none for an unbounded quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    /// The current period, for a quota with a period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<QuotaPeriod>,
    /// The attributes of a request that the quota is limited to: the quota
    /// applies only to a request whose metric `metadata` has, for each key
    /// here, the value or one of the listed values. Absent for a quota that
    /// applies to every request of its metric.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Map<String, Value>>,
}

/// A debit as the plugin receives it: every subject is a [`QuotaSubject`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDebitRequest {
    /// The tenant the operation runs in.
    pub tenant_id: Uuid,
    /// At least one subject.
    pub subjects: Vec<QuotaSubject>,
    /// At least one metric item.
    pub items: Vec<MetricItem>,
    /// Idempotency key of the call.
    pub idempotency_key: String,
    /// Platform identity of the resolver's caller, for the record only; `None`
    /// when the caller runs in the resolver's process.
    pub caller: Option<PlatformIdentity>,
}

/// A lease acquisition as the plugin receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginAcquireLeaseRequest {
    /// The tenant the operation runs in.
    pub tenant_id: Uuid,
    /// At least one subject.
    pub subjects: Vec<QuotaSubject>,
    /// At least one metric item.
    pub items: Vec<MetricItem>,
    /// Requested lease duration in seconds.
    pub ttl_secs: u64,
    /// Idempotency key of the call.
    pub idempotency_key: String,
    /// As [`PluginDebitRequest::caller`].
    pub caller: Option<PlatformIdentity>,
}

/// A quota state read as the plugin receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginQuotaStateRequest {
    /// The tenant to read in.
    pub tenant_id: Uuid,
    /// The subject.
    pub subject: QuotaSubject,
    /// The metrics to read.
    pub metrics: Vec<QuotaStateMetric>,
    /// Page size.
    pub limit: Option<u32>,
    /// Cursor of the page to read.
    pub cursor: Option<String>,
    /// As [`PluginDebitRequest::caller`].
    pub caller: Option<PlatformIdentity>,
}

mod _api_dto_markers {
    use toolkit::api::api_dto::{RequestApiDto, ResponseApiDto};

    impl RequestApiDto for super::PlatformDebitRequest {}
    impl RequestApiDto for super::PlatformAcquireLeaseRequest {}
    impl RequestApiDto for super::PlatformCommitLeaseRequest {}
    impl RequestApiDto for super::PlatformReleaseLeaseRequest {}
    impl RequestApiDto for super::PlatformReturnRequest {}
    impl RequestApiDto for super::PlatformQuotaStateRequest {}
    impl RequestApiDto for super::QuotaStateRequest {}
    impl ResponseApiDto for super::QuotaDecision {}
    impl ResponseApiDto for super::LeaseOutcome {}
    impl ResponseApiDto for super::QuotaState {}
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;

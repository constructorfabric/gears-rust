//! The snapshot read: the per-Quota state of explicit targets in one tenant.
//!
//! The gear owns the request's shape, its authorization and the subject
//! admission; storage owns the read and the page, and writes nothing. A
//! consuming product serves its end-user views through this same S2S call,
//! naming the user and tenant it authenticated; end users never reach QE
//! directly.
//!
//! A target describes what to read, not an evaluation claim. A subject of any
//! type but the tenant's selects the Quotas bound to that subject or to the
//! request's tenant, the set a debit by that subject is checked against. A
//! tenant-type target is a tenant-only target: its id must be the request's
//! tenant.
//!
//! The PDP sees every target in one call and must authorize each: one target
//! outside the caller's grant refuses the whole request before anything is
//! checked or read. One target whose metric is unavailable fails the whole
//! read. Metrics whose enforcement is `direct` stay readable: only debit,
//! lease acquisition, and preview refuse them.
//!
//! No instrument is added: a refused shape counts as an invalid-argument
//! denial, as on every other surface, and the PDP's outcome is admission's.

use crate::domain::admission::admission_span;
use quota_enforcement_sdk::{
    ApplicableQuotas, MetricId, PageRequest, PageResult, QuotaSnapshot, SnapshotRequest,
    SubjectRef, TenantId, conditions_hold,
};
use serde_json::{Map, Value, json};
use toolkit_macros::domain_model;
use tracing::Instrument as _;
use uuid::Uuid;

use super::service::Operations;
use crate::domain::admission::AdmissionTarget;
use crate::domain::attribution::{canonical, metric_document};
use crate::domain::catalog::MetricCatalog;
use crate::domain::error::DomainError;
use crate::domain::pep::{actions, properties, resources};
use crate::domain::ports::metrics::{DenialReason, ValidationSurface};
use crate::domain::quotas::validation::SUBJECT_ID_MAX_LEN;
use crate::domain::tokens;

/// The snapshot read's bounds (`[quota-enforcement.snapshot]`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLimits {
    /// The most rows one page returns, and the page size a request gets when
    /// it names none.
    pub page_size: u32,
    /// The most subjects one request may name.
    pub max_filters: usize,
}

impl Default for SnapshotLimits {
    /// The platform defaults: 100 rows per page, 100 subjects.
    fn default() -> Self {
        Self {
            page_size: 100,
            max_filters: 100,
        }
    }
}

/// One target after its shape checks.
struct Target {
    metric: MetricId,
    subject: SubjectRef,
    /// The request `metadata` the target's Quotas must apply to, canonical.
    metadata: Option<Map<String, Value>>,
}

// @cpt-flow:cpt-cf-quota-enforcement-flow-snapshot-read:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-end-user-scope:p1
impl Operations<'_> {
    /// Read the per-Quota state of the request's targets: one page of every
    /// active Quota they select, each once, in `quota_id` order.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for no subjects, too many, a limit outside
    /// `1..=page_size`, a nil tenant, a malformed subject or one naming another
    /// tenant (all before the PDP), a subject type its metric does not admit,
    /// or a foreign cursor; `PdpDenied` / `PdpUnavailable` from the PDP; the
    /// catalogue's errors for a metric type or a subject type; the storage
    /// error of the read.
    #[tracing::instrument(name = "quota_enforcement.operations.snapshot", skip_all, fields(tenant_id = %request.tenant_id, subject_count = request.subjects.len()))]
    pub async fn snapshot(
        &self,
        request: SnapshotRequest,
    ) -> Result<PageResult<QuotaSnapshot>, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-request
        let SnapshotRequest {
            tenant_id,
            subjects,
            limit,
            cursor,
        } = request;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-request
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-shape
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-target-shape
        let limit = self.snapshot_limit(tenant_id, subjects.len(), limit)?;
        let targets = self.shape_targets(tenant_id, &subjects)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-target-shape
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-shape
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-authz
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-fix
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-broaden-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-broaden
        // One PDP call for the complete target; any refused target refuses
        // the request before the catalogue or storage is consulted.
        let admitted = self
            .admission
            .admit_with_properties(
                &resources::SNAPSHOT,
                actions::READ,
                AdmissionTarget::tenant(tenant_id),
                filters_of(&targets),
            )
            .instrument(admission_span(targets.len()))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-broaden
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-broaden-if
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-authz
        let pairs = self.pairs_of(tenant_id, &targets).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-fix
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-all
        let mut page = self
            .storage
            .bulk_read_quota_snapshot(
                &admitted.access_scope,
                &pairs,
                PageRequest { limit, cursor },
                self.admission.is_trusted(),
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-all
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-read
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-return
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-shape
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-return
        // A target with `metadata` keeps only the Quotas that apply to it, as
        // a debit would (I15); the read takes no lock, so this is the state
        // at the read, not a reservation.
        if targets.iter().any(|target| target.metadata.is_some()) {
            page.items.retain(|quota| {
                targets.iter().zip(&pairs).any(|(target, pair)| {
                    pair.metric == quota.metric
                        && pair.subjects.contains(&quota.subject)
                        && target.metadata.as_ref().is_none_or(|metadata| {
                            conditions_hold(&quota.metadata, &Value::Object(metadata.clone()))
                        })
                })
            });
        }
        // One per-Quota shape for every caller; no policy attribution and no
        // aggregate figure exist to add.
        Ok(page)
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-return
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-shape
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-return
    }

    /// The request-level checks, in order: subjects present, not too many,
    /// the page size in range, a tenant named. Returns the effective limit.
    fn snapshot_limit(
        &self,
        tenant_id: TenantId,
        subjects: usize,
        limit: Option<u32>,
    ) -> Result<u32, DomainError> {
        if subjects == 0 {
            return Err(self.refuse(None, "subjects", tokens::SNAPSHOT_SUBJECTS_REQUIRED));
        }
        if subjects > self.snapshot.max_filters {
            return Err(self.refuse(None, "subjects", tokens::SNAPSHOT_TOO_MANY_SUBJECTS));
        }
        let limit = match limit {
            None => self.snapshot.page_size,
            Some(limit) if limit == 0 || limit > self.snapshot.page_size => {
                return Err(self.refuse(None, "limit", tokens::SNAPSHOT_LIMIT_OUT_OF_RANGE));
            }
            Some(limit) => limit,
        };
        if tenant_id.as_uuid().is_nil() {
            return Err(self.refuse(None, "tenant_id", tokens::TENANT_ID_REQUIRED));
        }
        Ok(limit)
    }

    /// Each subject's shape, naming the first one at fault: its metric, its
    /// id, its type, and a tenant-type subject naming the request's tenant.
    fn shape_targets(
        &self,
        tenant_id: TenantId,
        subjects: &[quota_enforcement_sdk::SnapshotSubject],
    ) -> Result<Vec<Target>, DomainError> {
        let mut targets = Vec::with_capacity(subjects.len());
        for (index, subject) in subjects.iter().enumerate() {
            let subject_metadata = &subject.metadata;
            let metric = MetricCatalog::parse_metric(&subject.metric)
                .ok_or_else(|| self.refuse(Some(index), "metric", tokens::METRIC_INVALID))?;
            if subject.id.trim().is_empty() {
                return Err(self.refuse(Some(index), "id", tokens::SUBJECT_ID_REQUIRED));
            }
            if subject.id.chars().count() > SUBJECT_ID_MAX_LEN {
                return Err(self.refuse(Some(index), "id", tokens::SUBJECT_ID_TOO_LONG));
            }
            let subject_type = MetricCatalog::parse_subject_type(&subject.subject_type)
                .ok_or_else(|| self.refuse(Some(index), "type", tokens::SUBJECT_TYPE_INVALID))?;
            let mut subject = SubjectRef {
                subject_type,
                subject_id: subject.id.clone(),
            };
            if subject.is_tenant() {
                if Uuid::parse_str(subject.subject_id.trim()).ok() != Some(tenant_id.as_uuid()) {
                    return Err(self.refuse(Some(index), "id", tokens::SNAPSHOT_TENANT_MISMATCH));
                }
                subject = SubjectRef::tenant(tenant_id);
            }
            targets.push(Target {
                metric,
                subject,
                metadata: subject_metadata.clone().map(canonical),
            });
        }
        Ok(targets)
    }

    /// The storage filter of each admitted target: the tenant subject on its
    /// metric, plus the target's own subject when it is not the tenant. Every
    /// metric needs a served catalogue entry. A subject type the metric does
    /// not admit is refused on QE's API; on the resolver plugin path it
    /// narrows the filter to the tenant's Quotas.
    async fn pairs_of(
        &self,
        tenant_id: TenantId,
        targets: &[Target],
    ) -> Result<Vec<ApplicableQuotas>, DomainError> {
        self.catalog
            .load(targets.iter().map(|target| target.metric.as_gts()))
            .await?;
        // Every metric is served before any subject is checked, so an
        // unavailable metric wins over a subject type it would not admit,
        // whatever the targets' order.
        let mut served = Vec::with_capacity(targets.len());
        for target in targets {
            served.push(
                self.catalog
                    .served(&target.metric, ValidationSurface::RequestMetric)
                    .await?,
            );
        }
        let mut pairs = Vec::with_capacity(targets.len());
        let mut admitted = Vec::new();
        for (index, (target, served)) in targets.iter().zip(&served).enumerate() {
            if let Some(metadata) = &target.metadata {
                let document = metric_document(&target.metric, None, Some(metadata));
                let mut fit = served.validate(&document);
                // The owner may have revised the type since the last scheduled read.
                if fit.is_err()
                    && let Some(revised) = self.catalog.reread(&target.metric).await
                {
                    fit = revised.validate(&document);
                }
                if fit.is_err() {
                    return Err(self.refuse(Some(index), "metadata", tokens::CONTRACT_VIOLATION));
                }
            }
            // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-read
            let narrowed = self.admission.is_trusted()
                && !target.subject.is_tenant()
                && !served.admits(&target.subject.subject_type);
            // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-read
            if !target.subject.is_tenant() && !narrowed {
                if !served.admits(&target.subject.subject_type) {
                    self.metrics
                        .record_admitted_metric_violation(ValidationSurface::RequestSubject);
                    return Err(DomainError::InvalidSnapshot {
                        index: Some(index),
                        field: "type",
                        reason: tokens::METRIC_NOT_ADMITTED,
                    });
                }
                admitted.push(index);
            }
            pairs.push(ApplicableQuotas {
                tenant_id,
                subjects: vec![SubjectRef::tenant(tenant_id)],
                metric: target.metric.clone(),
            });
        }
        self.catalog
            .load(
                admitted
                    .iter()
                    .map(|&index| &targets[index].subject.subject_type),
            )
            .await?;
        for index in admitted {
            let subject = &targets[index].subject;
            self.catalog
                .check_subject(subject, ValidationSurface::RequestSubject)
                .await?;
            pairs[index].subjects.push(subject.clone());
        }
        Ok(pairs)
    }

    fn refuse(
        &self,
        index: Option<usize>,
        field: &'static str,
        reason: &'static str,
    ) -> DomainError {
        self.metrics.record_denial(DenialReason::InvalidArgument);
        DomainError::InvalidSnapshot {
            index,
            field,
            reason,
        }
    }
}

/// Every target as the PDP sees it, in the request's order.
fn filters_of(targets: &[Target]) -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert(
        properties::FILTERS.to_owned(),
        Value::Array(
            targets
                .iter()
                .map(|target| {
                    json!({
                        "type": target.subject.subject_type.as_ref(),
                        "id": target.subject.subject_id,
                        "metric": target.metric.as_str(),
                    })
                })
                .collect(),
        ),
    );
    properties
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "snapshot_tests.rs"]
mod tests;

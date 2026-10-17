//! The snapshot read: the per-Quota state of explicit targets in one tenant.
//!
//! The gear owns the request's shape, its authorization and its catalogue
//! mapping; storage owns the read, the page and the I3 exception. A consuming
//! product serves its end-user views through this same S2S call, naming the
//! user and tenant it authenticated; end users never reach QE directly.
//!
//! A target describes what to read, not an evaluation claim. A non-tenant
//! kind selects the Quotas bound to that subject or to the request's tenant,
//! the set a debit by that subject is checked against. The `tenant` kind is a
//! snapshot-only, tenant-only target: its id must be the request's tenant.
//!
//! The PDP sees every target in one call and must authorize each: one target
//! outside the caller's grant refuses the whole request before anything is
//! mapped or read. `Direct`-classified metrics stay readable: only writes and
//! previews refuse them.
//!
//! No instrument is added: a refused shape counts as an invalid-argument
//! denial, as on every other surface, and the PDP's outcome is admission's.

use quota_enforcement_sdk::{
    ApplicableQuotas, MetricId, PageRequest, PageResult, QuotaSnapshot, SnapshotRequest,
    SubjectRef, SubjectScope, TenantId,
};
use serde_json::{Map, Value, json};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::service::Operations;
use crate::domain::admission::AdmissionTarget;
use crate::domain::catalog::{CatalogMiss, parse_metric_under_base};
use crate::domain::error::DomainError;
use crate::domain::pep::{actions, properties, resources};
use crate::domain::ports::metrics::{DenialReason, ValidationSurface};
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
    scope: SubjectScope,
    id: String,
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
    /// tenant (all before the PDP), a metric or kind the catalogue does not
    /// map, or a foreign cursor; `PdpDenied` / `PdpUnavailable` from the PDP;
    /// the storage error of the read.
    pub async fn snapshot(
        &self,
        ctx: &toolkit_security::SecurityContext,
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
                ctx,
                &resources::SNAPSHOT,
                actions::READ,
                AdmissionTarget::tenant(tenant_id),
                filters_of(&targets),
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-broaden
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-broaden-if
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-authz
        let pairs = self.pairs_of(tenant_id, &targets)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-fix
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-all
        let page = self
            .storage
            .bulk_read_quota_snapshot(
                ctx,
                &admitted.access_scope,
                &pairs,
                PageRequest { limit, cursor },
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-all
        // @cpt-end:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-read
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-return
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-shape
        // @cpt-begin:cpt-cf-quota-enforcement-flow-end-user-snapshot:p1:inst-eus-return
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
    /// id, its kind, and a `tenant` kind naming the request's tenant.
    fn shape_targets(
        &self,
        tenant_id: TenantId,
        subjects: &[quota_enforcement_sdk::SnapshotSubject],
    ) -> Result<Vec<Target>, DomainError> {
        let mut targets = Vec::with_capacity(subjects.len());
        for (index, subject) in subjects.iter().enumerate() {
            let metric = parse_metric_under_base(&subject.metric)
                .ok_or_else(|| self.refuse(Some(index), "metric", tokens::METRIC_INVALID))?;
            if subject.id.trim().is_empty() {
                return Err(self.refuse(Some(index), "id", tokens::SUBJECT_ID_REQUIRED));
            }
            let scope = SubjectScope::parse(&subject.kind)
                .map_err(|_| self.refuse(Some(index), "kind", tokens::SUBJECT_KIND_INVALID))?;
            if scope.is_tenant()
                && Uuid::parse_str(subject.id.trim()).ok() != Some(tenant_id.as_uuid())
            {
                return Err(self.refuse(Some(index), "id", tokens::SNAPSHOT_TENANT_MISMATCH));
            }
            targets.push(Target {
                metric,
                scope,
                id: subject.id.clone(),
            });
        }
        Ok(targets)
    }

    /// The storage filter of each authorized target: the tenant's projection
    /// on its metric, plus the subject's own for a non-tenant kind.
    fn pairs_of(
        &self,
        tenant_id: TenantId,
        targets: &[Target],
    ) -> Result<Vec<ApplicableQuotas>, DomainError> {
        let mut pairs = Vec::with_capacity(targets.len());
        for (index, target) in targets.iter().enumerate() {
            let tenant = SubjectRef {
                projection_type: self.map_target(index, &target.metric, &SubjectScope::tenant())?,
                subject_id: tenant_id.to_string(),
            };
            let subjects = if target.scope.is_tenant() {
                vec![tenant]
            } else {
                let own = SubjectRef {
                    projection_type: self.map_target(index, &target.metric, &target.scope)?,
                    subject_id: target.id.clone(),
                };
                vec![tenant, own]
            };
            pairs.push(ApplicableQuotas {
                tenant_id,
                subjects,
                metric: target.metric.clone(),
            });
        }
        Ok(pairs)
    }

    /// `(metric, kind)` through the catalogue, naming the target on a miss.
    fn map_target(
        &self,
        index: usize,
        metric: &MetricId,
        scope: &SubjectScope,
    ) -> Result<gts::GtsTypeId, DomainError> {
        self.catalog
            .map_subject(metric, scope)
            .cloned()
            .map_err(|miss| {
                self.metrics
                    .record_admitted_metric_violation(ValidationSurface::RequestSubject);
                let (field, reason) = match miss {
                    CatalogMiss::MetricNotAdmitted => ("metric", tokens::METRIC_NOT_ADMITTED),
                    CatalogMiss::KindUnknown | CatalogMiss::KindNotAdmitted => {
                        ("kind", tokens::SUBJECT_KIND_NOT_ADMITTED)
                    }
                };
                DomainError::InvalidSnapshot {
                    index: Some(index),
                    field,
                    reason,
                }
            })
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
                        "kind": target.scope.as_str(),
                        "id": target.id,
                        "metric": target.metric.as_gts().as_ref(),
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

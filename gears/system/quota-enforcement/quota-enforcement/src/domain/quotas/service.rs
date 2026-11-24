//! `QuotaManagement`: the Quota lifecycle component (DESIGN section 3.3,
//! `QuotaManagementService`; `features/quota-lifecycle.md`).
//!
//! Every operation runs in the same order: the request shape, the PDP, then
//! the metric catalogue and the check of the metadata against it, then storage.
//! Nothing before the storage call opens a transaction, and nothing after it
//! touches the registry. REST and the in-process client both enter here, so
//! both transports share one authorization boundary.

use crate::domain::admission::admission_span;
use std::collections::HashMap;
use tracing::Instrument as _;

use quota_enforcement_sdk::{
    DeactivateOutcome, MetricId, MetricKind, NotificationEvent, PageRequest, PageResult, Quota,
    QuotaDraft, QuotaEnforcementStoragePluginV1, QuotaFilter, QuotaId, QuotaPatch, QuotaStatus,
    QuotaType, QuotaView, TenantId,
};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;

use super::events::{ChangeKind, quota_changed};
use super::request::{CreateQuotaRequest, ListQuotasRequest, UpdateQuotaRequest};
use super::validation::{
    QuotaLimits, validate_create_shape, validate_list, validate_patched_shape,
    validate_subject_scope, validate_update_shape,
};
use super::window::{view, view_page};
use crate::domain::admission::{AdmissionTarget, Admitted, Gate};
use crate::domain::catalog::{CatalogEntry, MetricCatalog, MetricMode};
use crate::domain::error::{DomainError, ResourceKind};
use crate::domain::pep::{actions, resources};
use crate::domain::ports::metrics::{DenialReason, QeMetrics, ValidationSurface};
use crate::domain::tokens;

const LOG_TARGET: &str = "qe.quotas";

/// The lifecycle component, borrowed from the bound service for one call.
// @cpt-dod:cpt-cf-quota-enforcement-dod-quota-crud:p1
#[domain_model]
pub struct QuotaManagement<'a> {
    pub(super) admission: Gate<'a>,
    catalog: &'a MetricCatalog,
    pub(super) storage: &'a dyn QuotaEnforcementStoragePluginV1,
    pub(super) metrics: &'a dyn QeMetrics,
    pub(super) limits: QuotaLimits,
}

impl<'a> QuotaManagement<'a> {
    /// Assemble the component over the bound dependencies.
    #[must_use]
    pub fn new(
        admission: Gate<'a>,
        catalog: &'a MetricCatalog,
        storage: &'a dyn QuotaEnforcementStoragePluginV1,
        metrics: &'a dyn QeMetrics,
        limits: QuotaLimits,
    ) -> Self {
        Self {
            admission,
            catalog,
            storage,
            metrics,
            limits,
        }
    }

    /// The configured bounds.
    #[must_use]
    pub const fn limits(&self) -> QuotaLimits {
        self.limits
    }

    /// Create a Quota.
    ///
    /// # Errors
    ///
    /// In order: the shape violation, the PDP outcome, the catalogue's error
    /// for the metric type, the subject admission error, the subject-scope
    /// violation, the metadata rejection, then the storage error.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-quota-create:p1
    #[tracing::instrument(name = "quota_enforcement.quotas.create", skip_all, fields(tenant_id = %request.tenant_id, metric = %request.metric))]
    pub async fn create(&self, request: CreateQuotaRequest) -> Result<QuotaView, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-request
        // The request names its target `(subject_type, subject_id)` and tenant
        // explicitly; the PDP authorizes that target below.
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-validate
        let draft = validate_create_shape(request).map_err(|e| self.shape_rejected(e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-validate
        let admitted = self
            .admission
            .admit(
                &resources::QUOTA,
                actions::CREATE,
                AdmissionTarget::tenant(draft.tenant_id),
            )
            .instrument(admission_span(1))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-request

        let PreparedCreate {
            draft: storage_draft,
            events,
            mode,
            kind,
        } = self.prepare_create(draft).await?;

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-outside-tx
        // Everything above ran without a storage call; the transaction below
        // holds its locks for the insert alone.
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-outside-tx

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-persist
        let id = self
            .storage
            .create_quota(
                &admitted.actor,
                &admitted.access_scope,
                storage_draft,
                &events,
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-persist

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-direct-if
        if mode == MetricMode::Direct {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-direct
            // Accepted: a revision of the metric type can change its mode. The
            // Quota is inert until then and counted by the
            // `quota_for_direct_metric_total` gauge.
            tracing::info!(
                target: LOG_TARGET,
                quota_id = %id,
                "quota created on a metric whose enforcement is direct; inert while it is"
            );
            // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-direct
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-direct-if

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-return
        let quota = self
            .read_one(&admitted.access_scope, id)
            .await?
            .ok_or_else(|| DomainError::Internal("created quota is not readable".to_owned()))?;
        Ok(view(quota, Some(kind), OffsetDateTime::now_utc()))
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-return
    }

    /// The checks of a create after its shape and the PDP: the metric type,
    /// the subject's admission, the subject scope and the metadata, then the
    /// storage draft and its `quota-changed` event. No storage write.
    ///
    /// # Errors
    ///
    /// In order: the catalogue's error for the metric type, the subject
    /// admission error, the subject-scope violation, the metadata rejection.
    // @cpt-algo:cpt-cf-quota-enforcement-algo-catalog-membership:p1
    pub(super) async fn prepare_create(
        &self,
        draft: super::validation::ShapedDraft,
    ) -> Result<PreparedCreate, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-metric
        let served = self
            .catalog
            .served(&draft.metric, ValidationSurface::QuotaWrite)
            .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-metric-validation:p1:inst-qmv-classify
        let taken = match served.kind() {
            MetricKind::Counter => {
                matches!(draft.quota_type, QuotaType::Consumption | QuotaType::Rate)
            }
            MetricKind::Gauge => draft.quota_type == QuotaType::Allocation,
        };
        if !taken {
            return Err(DomainError::InvalidArgument {
                field: "quota_type",
                reason: tokens::QUOTA_TYPE_KIND_MISMATCH,
            });
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-metric-validation:p1:inst-qmv-classify
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-metric

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-membership
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-metric-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-metric
        if !served.admits(&draft.subject.subject_type) {
            self.metrics
                .record_admitted_metric_violation(ValidationSurface::QuotaWrite);
            tracing::warn!(
                target: LOG_TARGET,
                metric = %draft.metric,
                subject_type = %draft.subject.subject_type,
                "the metric type does not admit the subject type"
            );
            return Err(DomainError::InvalidArgument {
                field: "subject.type",
                reason: tokens::METRIC_NOT_ADMITTED,
            });
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-metric
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-metric-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-invalid-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-invalid
        if !draft.subject.is_tenant() {
            self.catalog
                .check_subject(&draft.subject, ValidationSurface::QuotaWrite)
                .await?;
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-invalid
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-membership:p1:inst-mem-invalid-if
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-membership

        validate_subject_scope(&draft.subject, draft.tenant_id)
            .map_err(|e| self.shape_rejected(e))?;

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-metadata
        super::metadata::check(
            self.metrics,
            &served,
            &draft.metadata,
            self.limits.metadata_max_bytes,
        )?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-metadata

        let now = OffsetDateTime::now_utc();
        let tenant_id = draft.tenant_id;
        let subject = draft.subject.clone();
        let events = [quota_changed(
            self.admission.actor(),
            tenant_id,
            None,
            Some(subject),
            ChangeKind::Created,
            now,
        )];
        let storage_draft = QuotaDraft {
            tenant_id: draft.tenant_id,
            subject: draft.subject,
            metric: draft.metric,
            quota_type: draft.quota_type,
            period: draft.period,
            enforcement_mode: draft.enforcement_mode,
            cap: draft.cap,
            notification_thresholds: draft.notification_thresholds,
            validity_window: draft.validity_window,
            fail_open_hint: draft.fail_open_hint,
            metadata: draft.metadata,
            source: draft.source,
        };
        Ok(PreparedCreate {
            draft: storage_draft,
            events: events.to_vec(),
            mode: served.mode(),
            kind: served.kind(),
        })
    }

    /// Apply a non-breaking patch.
    ///
    /// # Errors
    ///
    /// In order: the shape violation (`rate` first, then the immutable
    /// fields), the PDP outcome, [`DomainError::NotFound`],
    /// [`DomainError::QuotaDeactivated`], the patched-shape violation, the
    /// catalogue's error for the metric type, the metadata rejection, then the
    /// storage error
    /// ([`DomainError::CapBelowConsumed`] and
    /// [`DomainError::ThresholdsRequireBoundedCap`] decided under the row lock).
    // @cpt-flow:cpt-cf-quota-enforcement-flow-quota-update:p1
    #[tracing::instrument(name = "quota_enforcement.quotas.update", skip_all, fields(quota_id = %quota_id))]
    pub async fn update(
        &self,
        quota_id: QuotaId,
        request: UpdateQuotaRequest,
    ) -> Result<QuotaView, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-request
        let patch = validate_update_shape(request).map_err(|e| self.shape_rejected(e))?;
        let admitted = self
            .admit_resource(actions::UPDATE, quota_id)
            .instrument(admission_span(1))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-request

        let PreparedUpdate {
            patch,
            events,
            kind,
            ..
        } = self
            .prepare_update(&admitted.access_scope, quota_id, patch)
            .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-persist
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-guard-if
        // The cap-versus-consumed guard (I6) and the thresholds-versus-cap rule
        // (I14) are decided by storage on the merged row under its lock, never
        // on the row read above.
        let updated = self
            .storage
            .update_quota(
                &admitted.actor,
                &admitted.access_scope,
                quota_id,
                patch,
                &events,
            )
            .await
            // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-guard
            .map_err(DomainError::from)?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-guard
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-guard-if
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-persist

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-return
        Ok(view(updated, Some(kind), OffsetDateTime::now_utc()))
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-return
    }

    /// The checks of an update after its shape and the PDP: the current row
    /// under `scope`, its status, the patched shape, the metric and the
    /// metadata, then the patch as storage takes it and its `quota-changed`
    /// event. No storage write.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotFound`], [`DomainError::QuotaDeactivated`], the
    /// patched-shape violation, the catalogue's error for the metric type, the
    /// metadata rejection.
    pub(super) async fn prepare_update(
        &self,
        scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
    ) -> Result<PreparedUpdate, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-validate
        let current = self
            .read_one(scope, quota_id)
            .await?
            .ok_or_else(|| not_found(quota_id))?;
        if current.status == QuotaStatus::Deactivated {
            return Err(DomainError::QuotaDeactivated {
                id: quota_id.to_string(),
            });
        }
        validate_patched_shape(&current, &patch).map_err(|e| self.shape_rejected(e))?;
        // Metric identity is revalidated at update time; the subject's
        // admission is not, it was checked at create.
        let served = self
            .catalog
            .served(&current.metric, ValidationSurface::QuotaWrite)
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-validate

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-meta-if
        if let Some(metadata) = &patch.metadata {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-meta
            super::metadata::check(
                self.metrics,
                &served,
                metadata,
                self.limits.metadata_max_bytes,
            )?;
            // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-meta
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-meta-if

        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-event
        let now = OffsetDateTime::now_utc();
        let events = [quota_changed(
            self.admission.actor(),
            current.tenant_id,
            Some(quota_id),
            Some(current.subject.clone()),
            ChangeKind::Updated,
            now,
        )];
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-event
        Ok(PreparedUpdate {
            patch,
            events: events.to_vec(),
            kind: served.kind(),
            tenant_id: current.tenant_id,
        })
    }

    /// Deactivate a Quota. The record stays readable; storage resolves the
    /// active leases in the same transaction and lists them in the outcome.
    ///
    /// # Errors
    ///
    /// The PDP outcome, [`DomainError::NotFound`],
    /// [`DomainError::QuotaDeactivated`] on a second deactivation, or the
    /// storage error.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-quota-deactivate:p1
    #[tracing::instrument(name = "quota_enforcement.quotas.deactivate", skip_all, fields(quota_id = %quota_id))]
    pub async fn deactivate(&self, quota_id: QuotaId) -> Result<DeactivateOutcome, DomainError> {
        let admitted = self
            .admit_resource(actions::DEACTIVATE, quota_id)
            .instrument(admission_span(1))
            .await?;
        let (_, events) = self
            .prepare_deactivate(&admitted.access_scope, quota_id)
            .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-return
        Ok(self
            .storage
            .deactivate_quota(&admitted.actor, &admitted.access_scope, quota_id, &events)
            .await?)
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-return
    }

    /// The row a deactivation targets under `scope` and its `quota-changed`
    /// event: the owning tenant and the event. No storage write.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotFound`] for an unknown or out-of-scope id.
    pub(super) async fn prepare_deactivate(
        &self,
        scope: &AccessScope,
        quota_id: QuotaId,
    ) -> Result<(TenantId, Vec<NotificationEvent>), DomainError> {
        let current = self
            .read_one(scope, quota_id)
            .await?
            .ok_or_else(|| not_found(quota_id))?;
        let events = vec![quota_changed(
            self.admission.actor(),
            current.tenant_id,
            Some(quota_id),
            Some(current.subject),
            ChangeKind::Deactivated,
            OffsetDateTime::now_utc(),
        )];
        Ok((current.tenant_id, events))
    }

    /// Read one Quota.
    ///
    /// # Errors
    ///
    /// The PDP outcome, [`DomainError::NotFound`] for an unknown or
    /// out-of-scope id, or the registry's availability for a metric the
    /// catalogue does not hold.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-quota-read:p1
    #[tracing::instrument(name = "quota_enforcement.quotas.get", skip_all, fields(quota_id = %quota_id))]
    pub async fn get(&self, quota_id: QuotaId) -> Result<QuotaView, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-request
        let admitted = self
            .admit_resource(actions::GET, quota_id)
            .instrument(admission_span(1))
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-request
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-deactivated
        // No status filter: a deactivated Quota is read like any other.
        let quota = self
            .read_one(&admitted.access_scope, quota_id)
            .await?
            .ok_or_else(|| not_found(quota_id))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-deactivated
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-read
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-window
        let kinds = self.kinds_of(std::slice::from_ref(&quota.metric)).await;
        let kind = kinds.get(&quota.metric).copied();
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-window
        // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-return
        Ok(view(quota, kind, OffsetDateTime::now_utc()))
        // @cpt-end:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-return
    }

    /// Read a filtered page of Quotas within the caller's scope.
    ///
    /// # Errors
    ///
    /// The list-shape violation, the PDP outcome, the storage error, or the
    /// registry's availability for a metric the catalogue does not hold.
    #[tracing::instrument(name = "quota_enforcement.quotas.list", skip_all, fields(tenant_id = tracing::field::Empty))]
    pub async fn list(
        &self,
        request: ListQuotasRequest,
    ) -> Result<PageResult<QuotaView>, DomainError> {
        let (mut filter, page) =
            validate_list(request, &self.limits).map_err(|e| self.shape_rejected(e))?;
        let target = self.target_tenant(filter.tenant_id)?;
        tracing::Span::current().record("tenant_id", tracing::field::display(target));
        // A wider grant must not widen an absent tenant to every tenant it covers.
        filter.tenant_id = Some(target);
        let admitted = self
            .admission
            .admit(
                &resources::QUOTA,
                actions::LIST,
                AdmissionTarget::tenant(target),
            )
            .instrument(admission_span(1))
            .await?;
        // Rows outside the scope are absent, not errors; the cursor carries
        // position only and the scope is re-applied on every page.
        let page = self
            .storage
            .read_quotas(&admitted.access_scope, filter, page)
            .await?;
        let metrics: Vec<MetricId> = page.items.iter().map(|q| q.metric.clone()).collect();
        let kinds = self.kinds_of(&metrics).await;
        Ok(view_page(page, &kinds, OffsetDateTime::now_utc()))
    }

    /// The PDP target of an id-addressed operation: the caller's home tenant
    /// and the Quota id. The returned scope decides whether a Quota of another
    /// tenant is visible at all.
    pub(super) async fn admit_resource(
        &self,
        action: &str,
        quota_id: QuotaId,
    ) -> Result<Admitted, DomainError> {
        self.admission
            .admit(
                &resources::QUOTA,
                action,
                AdmissionTarget::resource(self.target_tenant(None)?, quota_id.as_uuid()),
            )
            .await
    }

    /// The named tenant, or else the caller's home tenant. Trusted admission
    /// has no caller tenant, so a target without a tenant is refused there.
    fn target_tenant(&self, named: Option<TenantId>) -> Result<TenantId, DomainError> {
        named
            .or_else(|| self.admission.caller_tenant())
            .ok_or_else(|| {
                self.shape_rejected(DomainError::InvalidArgument {
                    field: "tenant_id",
                    reason: tokens::TENANT_ID_REQUIRED,
                })
            })
    }

    /// The kinds of the distinct metrics of a read, from their served
    /// catalogue entries. A metric with no served entry is logged and left
    /// out; the Quota stays readable.
    async fn kinds_of(&self, metrics: &[MetricId]) -> HashMap<MetricId, MetricKind> {
        // `kind` is a display field: a registry outage must not fail the read.
        let loaded = self
            .catalog
            .load(metrics.iter().map(MetricId::as_gts))
            .await;
        let mut kinds = HashMap::new();
        for metric in metrics {
            if let Some(CatalogEntry::Served(served)) = self.catalog.peek(metric) {
                kinds.insert(metric.clone(), served.kind());
            }
        }
        // A metric type that is removed or unavailable is reported by the
        // lifecycle gauges; only the outage is reported here.
        if let Err(error) = loaded {
            tracing::warn!(
                target: LOG_TARGET,
                error = %error,
                "a quota read leaves out the kind of the metrics this replica does not hold"
            );
        }
        kinds
    }

    pub(super) async fn read_one(
        &self,
        scope: &AccessScope,
        quota_id: QuotaId,
    ) -> Result<Option<Quota>, DomainError> {
        let filter = QuotaFilter {
            ids: vec![quota_id],
            ..QuotaFilter::default()
        };
        let page = self
            .storage
            .read_quotas(scope, filter, PageRequest::first(1))
            .await?;
        Ok(page.items.into_iter().next())
    }

    /// A request rejected before the PDP is an admission denial by invalid
    /// argument; a reserved capability is not.
    pub(super) fn shape_rejected(&self, err: DomainError) -> DomainError {
        if !matches!(err, DomainError::NotYetImplemented { .. }) {
            self.metrics.record_denial(DenialReason::InvalidArgument);
        }
        err
    }
}

/// A create that passed every check before storage.
#[domain_model]
pub(super) struct PreparedCreate {
    /// The draft as storage takes it.
    pub(super) draft: QuotaDraft,
    /// Its `quota-changed` event; storage fills in the id.
    pub(super) events: Vec<NotificationEvent>,
    /// The metric's current mode.
    pub(super) mode: MetricMode,
    /// The metric's kind, for the view.
    pub(super) kind: MetricKind,
}

/// An update that passed every check before storage.
#[domain_model]
pub(super) struct PreparedUpdate {
    /// The patch as storage takes it.
    pub(super) patch: QuotaPatch,
    /// Its `quota-changed` event.
    pub(super) events: Vec<NotificationEvent>,
    /// The metric's kind, for the view.
    pub(super) kind: MetricKind,
    /// The owning tenant of the patched row.
    pub(super) tenant_id: TenantId,
}

fn not_found(quota_id: QuotaId) -> DomainError {
    DomainError::NotFound {
        kind: ResourceKind::Quota,
        id: quota_id.to_string(),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

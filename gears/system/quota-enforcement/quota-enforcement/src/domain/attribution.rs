//! Ingress attribution of a subject-based evaluation request
//! (`features/projection-contracts.md`, "Evaluation Request Ingress Validation"
//! and "Subject Admission").
//!
//! Every debit, lease acquisition, preview, and batch item enters here. The
//! order is fixed and each stage has its own error class, so precedence is
//! observable: the public shape is checked before the PDP is called, the PDP
//! authorizes the complete caller-supplied tuple, and only then does the
//! metric catalogue admit the subjects and validate the metric object. A
//! metric the catalogue holds needs no registry call, and no failure is ever a
//! `Decision::Denied`.

use std::sync::Arc;

use authz_resolver_sdk::pep::ResourceType;
use quota_enforcement_sdk::{
    Actor, AttributionDigest, EvaluationAttribution, MetricId, SubjectClaim, SubjectRef, TenantId,
};
use serde_json::{Map, Value, json};
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::admission::{AdmissionTarget, Gate};
use super::catalog::{MetricCatalog, ServedMetric};
use super::error::DomainError;
use super::pep::properties;
use super::ports::metrics::{DenialReason, QeMetrics, ValidationReason, ValidationSurface};
use super::quotas::validation::{METRIC_ID_MAX_LEN, SUBJECT_ID_MAX_LEN, SUBJECTS_MAX};
use super::tokens;

const LOG_TARGET: &str = "qe.attribution";

/// The admitted attribution of one request.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct MappedAttribution {
    /// The admitted target tenant.
    pub tenant_id: TenantId,
    /// The complete subject set: the tenant subject first, then the other
    /// named subjects ordered by type.
    pub subjects: Vec<SubjectRef>,
    /// The scope exactly as admission returned it.
    pub access_scope: AccessScope,
    /// The cause of the operation, as admission returned it.
    pub actor: Actor,
}

/// A request that passed ingress: what the evaluation pipeline consumes.
#[domain_model]
#[derive(Debug, Clone)]
pub struct AdmittedEvaluation {
    /// Who and what is being charged.
    pub attribution: MappedAttribution,
    /// The served catalogue entry of the request's metric type.
    pub metric: Arc<ServedMetric>,
    /// The `id` of the request's metric object.
    pub metric_id: Option<String>,
    /// The validated `metadata` of the metric object: what a Policy reads as
    /// `request`.
    pub request: Map<String, Value>,
    /// The attribution digest of what the PDP authorized: the tenant, the
    /// subjects, and the metric type and id. The charge of a debit keeps it,
    /// so that a later rollback can prove it names an operation it was itself
    /// admitted for.
    pub authorized: AttributionDigest,
}

/// A rollback's attribution after admission. It names an existing charge, so
/// it makes no catalogue check.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedReversal {
    /// Who the reversed operation charged.
    pub attribution: MappedAttribution,
    /// As [`AdmittedEvaluation::authorized`].
    pub authorized: AttributionDigest,
}

/// The ingress step over the PEP boundary and the metric catalogue.
// @cpt-dod:cpt-cf-quota-enforcement-dod-ingress-validation:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-subject-resolution:p1
#[domain_model]
pub struct Attribution<'a> {
    admission: &'a Gate<'a>,
    catalog: &'a MetricCatalog,
    metrics: &'a dyn QeMetrics,
}

/// The attribution after the shape stage: parsed, not yet authorized.
struct Shaped {
    tenant_id: TenantId,
    metric: MetricId,
    metric_id: Option<String>,
    subjects: Vec<SubjectRef>,
    metadata: Option<Map<String, Value>>,
}

impl<'a> Attribution<'a> {
    /// The step over `admission` and `catalog`.
    #[must_use]
    pub fn new(
        admission: &'a Gate<'a>,
        catalog: &'a MetricCatalog,
        metrics: &'a dyn QeMetrics,
    ) -> Self {
        Self {
            admission,
            catalog,
            metrics,
        }
    }

    /// Admit one subject-based evaluation request.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] with a closed reason token on a
    ///   malformed public shape (before the PDP), on a subject type the metric
    ///   does not admit, or on a metric object that does not fit its type.
    /// - [`DomainError::PdpDenied`] / [`DomainError::PdpUnavailable`] from the
    ///   PDP, before any catalogue lookup.
    /// - The catalogue's errors for the metric type and the subject types.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-ingress-validation:p1
    // @cpt-algo:cpt-cf-quota-enforcement-algo-subject-resolution:p1
    pub async fn admit_evaluation(
        &self,
        resource_type: &ResourceType,
        action: &str,
        attribution: EvaluationAttribution,
    ) -> Result<AdmittedEvaluation, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-request
        let site = Site {
            actor: self.admission.actor(),
            resource: resource_type.name(),
            action,
        };
        // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-request

        // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-shape
        // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-shape
        let shaped = self.shape(&site, attribution)?;
        let Some(metadata) = shaped.metadata.clone().map(canonical) else {
            return Err(self.record_shape(
                &site,
                "metric.metadata",
                tokens::METADATA_REQUIRED,
                "",
                ValidationReason::MetadataMissing,
            ));
        };
        // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-shape
        // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-shape

        let (attribution, authorized) = self.authorize(resource_type, action, &shaped).await?;
        let Shaped {
            metric, metric_id, ..
        } = shaped;

        let mut served = self
            .catalog
            .served(&metric, ValidationSurface::RequestMetric)
            .await?;

        // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-each
        let admitted = |served: &ServedMetric| {
            for subject in &attribution.subjects {
                // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-map
                // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-metric-if
                // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-metric
                // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-invalid-if
                // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-invalid
                if !served.admits(&subject.subject_type) {
                    self.metrics
                        .record_admitted_metric_violation(ValidationSurface::RequestSubject);
                    tracing::warn!(
                        target: LOG_TARGET,
                        actor = %site.actor,
                        resource = site.resource,
                        action = site.action,
                        metric = %metric,
                        subject_type = %subject.subject_type,
                        "the metric type does not admit the subject type"
                    );
                    return Err(DomainError::InvalidArgument {
                        field: "subjects",
                        reason: tokens::METRIC_NOT_ADMITTED,
                    });
                }
                // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-invalid
                // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-invalid-if
                // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-metric
                // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-metric-if
                // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-map
            }
            Ok(())
        };
        admitted(&served)?;
        let registered = attribution
            .subjects
            .iter()
            .filter(|subject| !subject.is_tenant());
        self.catalog
            .load(registered.clone().map(|subject| &subject.subject_type))
            .await?;
        for subject in registered {
            self.catalog
                .check_subject(subject, ValidationSurface::RequestSubject)
                .await?;
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-each

        // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-metadata
        let document = metric_document(&metric, metric_id.as_deref(), Some(&metadata));
        let mut fit = served.validate(&document);
        // The owner may have revised the type since the last scheduled read.
        if fit.is_err()
            && let Some(revised) = self.catalog.reread(&metric).await
        {
            served = revised;
            admitted(&served)?;
            fit = served.validate(&document);
        }
        fit.map_err(|violations| {
            self.metrics.record_contract_validation_failure(
                ValidationSurface::RequestMetric,
                ValidationReason::SchemaViolation,
            );
            tracing::warn!(
                target: LOG_TARGET,
                actor = %site.actor,
                resource = site.resource,
                action = site.action,
                metric = %metric,
                ?violations,
                "the metric object does not fit its metric type"
            );
            DomainError::InvalidArgument {
                field: "metric",
                reason: tokens::CONTRACT_VIOLATION,
            }
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-metadata

        // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-map
        // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-forward
        // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-return
        // @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-return
        Ok(AdmittedEvaluation {
            attribution,
            metric: served,
            metric_id,
            request: metadata,
            authorized,
        })
        // @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-return
        // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-return
        // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-forward
        // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-map
    }

    /// Admit the attribution of an operation a rollback reverses: the public
    /// shape and the PDP, with no catalogue check and no `metadata`.
    ///
    /// # Errors
    ///
    /// As the shape and PDP stages of [`Self::admit_evaluation`].
    pub async fn admit_reversal(
        &self,
        resource_type: &ResourceType,
        action: &str,
        attribution: EvaluationAttribution,
    ) -> Result<AdmittedReversal, DomainError> {
        let site = Site {
            actor: self.admission.actor(),
            resource: resource_type.name(),
            action,
        };
        let shaped = self.shape(&site, attribution)?;
        let (attribution, authorized) = self.authorize(resource_type, action, &shaped).await?;
        Ok(AdmittedReversal {
            attribution,
            authorized,
        })
    }

    /// The admission stage: admit the complete tuple, then digest what was
    /// admitted.
    async fn authorize(
        &self,
        resource_type: &ResourceType,
        action: &str,
        shaped: &Shaped,
    ) -> Result<(MappedAttribution, AttributionDigest), DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-authz
        // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-authz
        let pdp_properties = shaped.pdp_properties();
        let admitted = self
            .admission
            .admit_with_properties(
                resource_type,
                action,
                AdmissionTarget::tenant(shaped.tenant_id),
                pdp_properties,
            )
            .await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-authz
        // @cpt-end:cpt-cf-quota-enforcement-flow-ingress-validation:p1:inst-ing-authz
        let authorized = AttributionDigest::of(
            shaped.tenant_id,
            &shaped.subjects,
            &shaped.metric,
            shaped.metric_id.as_deref(),
        );
        Ok((
            MappedAttribution {
                tenant_id: shaped.tenant_id,
                subjects: shaped.subjects.clone(),
                access_scope: admitted.access_scope,
                actor: admitted.actor,
            },
            authorized,
        ))
    }

    /// Stage 1: the public request shape, before any PDP call.
    fn shape(
        &self,
        site: &Site<'_>,
        attribution: EvaluationAttribution,
    ) -> Result<Shaped, DomainError> {
        let tenant_id = attribution.tenant_id;
        if tenant_id.as_uuid().is_nil() {
            return Err(self.reject_shape(site, "tenant_id", tokens::TENANT_ID_REQUIRED, ""));
        }
        let metric_ref = attribution.metric;
        let metric = MetricCatalog::parse_metric(&metric_ref.metric_type).ok_or_else(|| {
            self.reject_shape(
                site,
                "metric.type",
                tokens::METRIC_INVALID,
                &metric_ref.metric_type,
            )
        })?;
        if metric_ref.id.as_deref().is_some_and(str::is_empty) {
            return Err(self.reject_shape(site, "metric.id", tokens::METRIC_ID_INVALID, ""));
        }
        if metric_ref
            .id
            .as_deref()
            .is_some_and(|id| id.chars().count() > METRIC_ID_MAX_LEN)
        {
            return Err(self.reject_shape(site, "metric.id", tokens::METRIC_ID_TOO_LONG, ""));
        }
        let subjects = self.shape_subjects(site, tenant_id, &attribution.subjects)?;
        Ok(Shaped {
            tenant_id,
            metric,
            metric_id: metric_ref.id,
            subjects,
            metadata: metric_ref.metadata,
        })
    }

    /// The subject set: at least one named subject, no two of one type, a
    /// tenant-type subject only for the request's tenant. The tenant subject
    /// comes first, whether named or not.
    // @cpt-begin:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-tenant
    fn shape_subjects(
        &self,
        site: &Site<'_>,
        tenant_id: TenantId,
        claims: &[SubjectClaim],
    ) -> Result<Vec<SubjectRef>, DomainError> {
        if claims.is_empty() {
            return Err(self.reject_shape(site, "subjects", tokens::SUBJECTS_REQUIRED, ""));
        }
        if claims.len() > SUBJECTS_MAX {
            return Err(self.reject_shape(site, "subjects", tokens::SUBJECTS_TOO_MANY, ""));
        }
        let mut subjects = vec![SubjectRef::tenant(tenant_id)];
        let mut named = Vec::with_capacity(claims.len());
        for claim in claims {
            if claim.id.trim().is_empty() {
                return Err(self.reject_shape(
                    site,
                    "subjects",
                    tokens::SUBJECT_ID_REQUIRED,
                    &claim.subject_type,
                ));
            }
            if claim.id.chars().count() > SUBJECT_ID_MAX_LEN {
                return Err(self.reject_shape(
                    site,
                    "subjects",
                    tokens::SUBJECT_ID_TOO_LONG,
                    &claim.subject_type,
                ));
            }
            let subject_type =
                MetricCatalog::parse_subject_type(&claim.subject_type).ok_or_else(|| {
                    self.reject_shape(
                        site,
                        "subjects",
                        tokens::SUBJECT_TYPE_INVALID,
                        &claim.subject_type,
                    )
                })?;
            if named.contains(&subject_type) {
                return Err(self.reject_shape(
                    site,
                    "subjects",
                    tokens::SUBJECT_TYPE_DUPLICATE,
                    &claim.subject_type,
                ));
            }
            named.push(subject_type.clone());
            let subject = SubjectRef {
                subject_type,
                subject_id: claim.id.clone(),
            };
            if subject.is_tenant() {
                if Uuid::parse_str(&claim.id).ok() != Some(tenant_id.as_uuid()) {
                    return Err(self.reject_shape(
                        site,
                        "subjects",
                        tokens::TENANT_SUBJECT_MISMATCH,
                        &claim.subject_type,
                    ));
                }
                continue;
            }
            subjects.push(subject);
        }
        subjects[1..].sort_by(|a, b| a.subject_type.as_ref().cmp(b.subject_type.as_ref()));
        Ok(subjects)
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-subject-resolution:p1:inst-res-tenant

    /// A malformed public shape: a denial before the PDP.
    fn reject_shape(
        &self,
        site: &Site<'_>,
        field: &'static str,
        reason: &'static str,
        offending: &str,
    ) -> DomainError {
        self.record_shape(
            site,
            field,
            reason,
            offending,
            ValidationReason::ShapeInvalid,
        )
    }

    /// Record, log and build a malformed-attribution rejection.
    fn record_shape(
        &self,
        site: &Site<'_>,
        field: &'static str,
        reason: &'static str,
        offending: &str,
        validation: ValidationReason,
    ) -> DomainError {
        self.metrics.record_denial(DenialReason::InvalidArgument);
        self.metrics
            .record_contract_validation_failure(ValidationSurface::CallerAttribution, validation);
        tracing::warn!(
            target: LOG_TARGET,
            actor = %site.actor,
            resource = site.resource,
            action = site.action,
            field,
            reason,
            offending,
            "malformed attribution rejected before the PDP"
        );
        DomainError::InvalidArgument { field, reason }
    }
}

impl Shaped {
    /// The rest of the attribution tuple as PDP resource properties. The
    /// tenant is the admission target and is written by `Admission` itself.
    fn pdp_properties(&self) -> Map<String, Value> {
        let mut properties = Map::new();
        properties.insert(
            properties::METRIC.to_owned(),
            Value::String(self.metric.as_str().to_owned()),
        );
        if let Some(id) = &self.metric_id {
            properties.insert(properties::METRIC_ID.to_owned(), Value::String(id.clone()));
        }
        properties.insert(
            properties::SUBJECTS.to_owned(),
            Value::Array(
                self.subjects
                    .iter()
                    .map(|subject| json!({ "type": subject.subject_type.as_ref(), "id": subject.subject_id }))
                    .collect(),
            ),
        );
        properties
    }
}

/// The canonical form of a request's `metadata`: a number with no fractional
/// part is an integer, however it was written. JSON Schema already reads `5.0`
/// as the integer 5; this makes the value itself say so, for the conditions of
/// a Quota, for a Policy, and for the request fingerprint.
pub(crate) fn canonical(mut metadata: Map<String, Value>) -> Map<String, Value> {
    fn settle(value: &mut Value) {
        match value {
            Value::Number(number) if number.is_f64() => {
                if let Some(integer) = number.as_f64().and_then(integral) {
                    *number = integer;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(settle),
            Value::Object(fields) => fields.values_mut().for_each(settle),
            _ => {}
        }
    }
    metadata.values_mut().for_each(settle);
    metadata
}

/// `float` as an integer, when it has no fractional part and fits one.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn integral(float: f64) -> Option<serde_json::Number> {
    if float.fract() != 0.0 {
        return None;
    }
    if float >= i64::MIN as f64 && float < i64::MAX as f64 {
        Some((float as i64).into())
    } else if float >= 0.0 && float < u64::MAX as f64 {
        Some((float as u64).into())
    } else {
        None
    }
}

/// The whole metric object as its metric type's schema validates it.
pub(crate) fn metric_document(
    metric: &MetricId,
    id: Option<&str>,
    metadata: Option<&Map<String, Value>>,
) -> Value {
    let mut document = Map::new();
    document.insert("type".to_owned(), Value::String(metric.as_str().to_owned()));
    if let Some(id) = id {
        document.insert("id".to_owned(), Value::String(id.to_owned()));
    }
    if let Some(metadata) = metadata {
        document.insert("metadata".to_owned(), Value::Object(metadata.clone()));
    }
    Value::Object(document)
}

/// Log context of one ingress call.
struct Site<'c> {
    actor: Actor,
    resource: &'c str,
    action: &'c str,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "attribution_tests.rs"]
mod attribution_tests;

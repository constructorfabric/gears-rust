//! The catalogue's entities and its read API.
//!
//! Every contract is validated by the `gts` payload validator compiled from
//! its resolved type, the one the registry itself validates with. The
//! dialect the schema declares applies, with format assertion on, and
//! `x-gts-type-ref`, `x-gts-instance-ref` and `x-gts-pattern-ref` are compiled
//! as keywords of the schema itself. A GTS reference constraint therefore
//! counts toward a branch's validity like `type` or `format` does: a `oneOf`
//! over two branches that differ only in their entity reference picks exactly
//! one, and a branch that carries no GTS constraint is not "matched" by
//! default. The request path then validates a document with no allocation
//! beyond the envelope it wraps.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use gts::{GtsId, GtsTypeId, PayloadValidationError, PayloadValidator};
use quota_enforcement_sdk::{ContractRef, METRIC_BASE_TYPE, MetricId, SubjectScope};
use serde_json::Value;
use toolkit_macros::domain_model;

/// A well-formed instance id whose declaring type is the metric base. Whether
/// the registry knows it is a separate question the bootstrap answers.
#[must_use]
pub fn parse_metric_under_base(text: &str) -> Option<MetricId> {
    let parsed = GtsId::try_new(text).ok()?;
    if parsed.is_type() || parsed.get_type_id().as_deref() != Some(METRIC_BASE_TYPE) {
        return None;
    }
    MetricId::parse(text).ok()
}

/// A resolved contract schema compiled for validation.
#[domain_model]
pub struct CompiledContract {
    validator: PayloadValidator,
    schema: Arc<Value>,
}

impl CompiledContract {
    /// Wrap the payload validator and the resolved schema it was compiled from.
    #[must_use]
    pub fn new(validator: PayloadValidator, schema: Value) -> Self {
        Self {
            validator,
            schema: Arc::new(schema),
        }
    }

    /// The resolved schema used by this immutable catalogue, without a registry read.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// The contract type.
    #[must_use]
    pub fn type_id(&self) -> &GtsTypeId {
        self.validator.type_id()
    }

    /// Validate the whole `document` (the `{type, metadata}` envelope, never
    /// the inner object alone) against the schema and its entity references
    /// in one pass. The diagnostics are for the log;
    /// callers surface a closed reason token.
    ///
    /// # Errors
    ///
    /// Returns the JSON Schema violations found or, only when none is, the
    /// GTS reference violations. A rejection too costly to explain is
    /// reported as one unexplained reason.
    pub fn validate(&self, document: &Value) -> Result<(), Vec<String>> {
        self.validator
            .validate(document)
            .map_err(|error| match error {
                PayloadValidationError::Constraints(violations) => violations,
                PayloadValidationError::References(violations) => violations
                    .into_iter()
                    .map(|v| format!("{}: {}", v.field_path, v.reason))
                    .collect(),
                PayloadValidationError::Unexplained(reason) => vec![reason],
            })
    }
}

impl fmt::Debug for CompiledContract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledContract")
            .field("type_id", self.type_id())
            .finish_non_exhaustive()
    }
}

/// A configured concrete subject projection: the type half of a Quota's
/// subject identity.
#[domain_model]
#[derive(Debug)]
pub struct SubjectProjectionContract {
    /// The projection type, derived from `gts.cf.core.qe.subj.v1~`.
    pub type_id: GtsTypeId,
    /// The scope its `scope` trait declares.
    pub scope: SubjectScope,
    /// The metrics its `admitted_metrics` trait declares.
    pub admitted_metrics: HashSet<MetricId>,
}

/// A configured concrete resource projection.
#[domain_model]
#[derive(Debug)]
pub struct ResourceProjectionContract {
    /// The projection type, derived from `gts.cf.core.qe.res.v1~`.
    pub type_id: GtsTypeId,
    /// Validates the complete `{type, id?, metadata}` document.
    pub contract: CompiledContract,
}

/// The constraint contract attached to a metric request contract.
#[domain_model]
#[derive(Debug)]
pub struct ConstraintContract {
    /// The contract at the version its id declares.
    pub reference: ContractRef,
    /// Validates the `{type, metadata}` envelope of a Quota's metadata.
    pub contract: CompiledContract,
}

/// The one request contract of an admitted metric.
#[domain_model]
#[derive(Debug)]
pub struct MetricRequestContract {
    /// The contract type, derived from `gts.cf.core.qe.request.v1~`.
    pub type_id: GtsTypeId,
    /// The metric its `metric` trait names.
    pub metric: MetricId,
    /// The constraint contract its `constraint_contract` trait attaches.
    pub constraint: ConstraintContract,
    /// Validates the `{type, metadata}` envelope of operation metadata.
    pub contract: CompiledContract,
}

/// Everything the catalogue knows about one admitted metric.
#[derive(Debug)]
struct MetricEntry {
    request: MetricRequestContract,
    /// The reverse index: which configured projection admits the metric at
    /// each scope. Unique per pair by construction.
    by_scope: HashMap<SubjectScope, GtsTypeId>,
}

/// Why a `(metric, kind)` pair has no projection.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogMiss {
    /// No configured projection admits the metric at any scope.
    MetricNotAdmitted,
    /// The kind is a scope no configured projection declares.
    KindUnknown,
    /// The kind is a known scope, but no projection admits the metric there.
    KindNotAdmitted,
}

/// The immutable process-local catalogue. Built once at bootstrap, read on
/// every subject-based request without a registry call.
///
/// `cpt-cf-quota-enforcement-dod-projection-catalog` stays open until the
/// active-Quota compatibility check runs against the database.
#[domain_model]
#[derive(Debug)]
pub struct ProjectionContractCatalog {
    subjects: HashMap<GtsTypeId, SubjectProjectionContract>,
    resources: HashMap<GtsTypeId, ResourceProjectionContract>,
    metrics: HashMap<MetricId, MetricEntry>,
    scopes: HashSet<SubjectScope>,
}

impl ProjectionContractCatalog {
    /// A catalogue with no projections. Every mapping misses.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            subjects: HashMap::new(),
            resources: HashMap::new(),
            metrics: HashMap::new(),
            scopes: HashSet::new(),
        }
    }

    /// Assemble the catalogue from validated parts. The builder has already
    /// proven `(metric, scope)` uniqueness and that every admitted metric has
    /// exactly one request contract.
    pub(super) fn assemble(
        subjects: Vec<SubjectProjectionContract>,
        resources: Vec<ResourceProjectionContract>,
        requests: Vec<MetricRequestContract>,
    ) -> Self {
        let mut metrics: HashMap<MetricId, MetricEntry> = requests
            .into_iter()
            .map(|request| {
                (
                    request.metric.clone(),
                    MetricEntry {
                        request,
                        by_scope: HashMap::new(),
                    },
                )
            })
            .collect();
        let mut scopes = HashSet::new();
        for projection in &subjects {
            scopes.insert(projection.scope.clone());
            for metric in &projection.admitted_metrics {
                if let Some(entry) = metrics.get_mut(metric) {
                    entry
                        .by_scope
                        .insert(projection.scope.clone(), projection.type_id.clone());
                }
            }
        }
        Self {
            subjects: subjects
                .into_iter()
                .map(|p| (p.type_id.clone(), p))
                .collect(),
            resources: resources
                .into_iter()
                .map(|r| (r.type_id.clone(), r))
                .collect(),
            metrics,
            scopes,
        }
    }

    /// The projection that admits `metric` at `scope`.
    ///
    /// # Errors
    ///
    /// Returns which part of the pair the catalogue does not know.
    pub fn map_subject(
        &self,
        metric: &MetricId,
        scope: &SubjectScope,
    ) -> Result<&GtsTypeId, CatalogMiss> {
        let entry = self
            .metrics
            .get(metric)
            .ok_or(CatalogMiss::MetricNotAdmitted)?;
        entry
            .by_scope
            .get(scope)
            .ok_or(if self.scopes.contains(scope) {
                CatalogMiss::KindNotAdmitted
            } else {
                CatalogMiss::KindUnknown
            })
    }

    /// The request contract of an admitted metric.
    #[must_use]
    pub fn request_contract(&self, metric: &MetricId) -> Option<&MetricRequestContract> {
        self.metrics.get(metric).map(|e| &e.request)
    }

    /// A configured resource projection.
    #[must_use]
    pub fn resource_projection(&self, type_id: &GtsTypeId) -> Option<&ResourceProjectionContract> {
        self.resources.get(type_id)
    }

    /// A configured subject projection.
    #[must_use]
    pub fn subject_projection(&self, type_id: &GtsTypeId) -> Option<&SubjectProjectionContract> {
        self.subjects.get(type_id)
    }

    /// True when `projection` is configured and admits `metric`.
    #[must_use]
    pub fn admits(&self, projection: &GtsTypeId, metric: &MetricId) -> bool {
        self.subjects
            .get(projection)
            .is_some_and(|p| p.admitted_metrics.contains(metric))
    }

    /// True when some configured projection declares `scope`.
    #[must_use]
    pub fn knows_scope(&self, scope: &SubjectScope) -> bool {
        self.scopes.contains(scope)
    }

    /// The configured subject projections.
    pub fn subject_projections(&self) -> impl Iterator<Item = &SubjectProjectionContract> {
        self.subjects.values()
    }

    /// The configured resource projections.
    pub fn resource_projections(&self) -> impl Iterator<Item = &ResourceProjectionContract> {
        self.resources.values()
    }

    /// The admitted metrics.
    pub fn admitted_metrics(&self) -> impl Iterator<Item = &MetricId> {
        self.metrics.keys()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "model_tests.rs"]
mod model_tests;

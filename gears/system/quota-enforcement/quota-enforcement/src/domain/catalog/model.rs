//! A catalogue entry and the checks that build it from a registered metric
//! type (`cpt-cf-quota-enforcement-algo-catalog-bootstrap`). An entry is
//! built from the registry content alone, so every replica that reads the
//! same content builds the same entry.

use std::fmt;
use std::sync::Arc;

use gts::{GtsId, GtsTypeId, PayloadValidationError, PayloadValidator};
use quota_enforcement_sdk::{MetricId, MetricKind};
use quota_resolver_sdk::{
    QUOTA_METRIC_TYPE_ID, QUOTA_SUBJECT_TYPE_ID, QUOTA_TENANT_SUBJECT_TYPE_ID,
};
use serde::Deserialize;
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::ports::metrics::{MetricLabel, ValidationReason};
use crate::domain::ports::type_registry::RegisteredType;

/// The `enforcement` trait of a metric type.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricMode {
    /// Operations on the metric are checked against quotas.
    QuotaGated,
    /// The metric is not checked against quotas; debit and lease acquisition
    /// are refused.
    Direct,
}

/// The traits of the metric base, as the registry merges them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetricTraits {
    kind: MetricKind,
    #[serde(default)]
    admitted_subjects: Vec<String>,
    #[serde(default = "quota_gated")]
    enforcement: MetricMode,
}

const fn quota_gated() -> MetricMode {
    MetricMode::QuotaGated
}

/// A metric type that passed every check: what requests and writes use.
#[domain_model]
pub struct ServedMetric {
    id: MetricId,
    kind: MetricKind,
    mode: MetricMode,
    admitted_subjects: Vec<GtsTypeId>,
    schema: Arc<Value>,
    validator: PayloadValidator,
    label: MetricLabel,
}

impl ServedMetric {
    /// The metric type.
    #[must_use]
    pub fn id(&self) -> &MetricId {
        &self.id
    }

    /// The `kind` trait.
    #[must_use]
    pub fn kind(&self) -> MetricKind {
        self.kind
    }

    /// The `enforcement` trait.
    #[must_use]
    pub fn mode(&self) -> MetricMode {
        self.mode
    }

    /// The resolved schema of the metric type.
    #[must_use]
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// The `metric` label of the instruments.
    #[must_use]
    pub fn label(&self) -> &MetricLabel {
        &self.label
    }

    /// Whether the metric's quotas may count `subject_type`: the tenant type
    /// always, any other type when it is an admitted type or derives from one.
    #[must_use]
    pub fn admits(&self, subject_type: &GtsTypeId) -> bool {
        subject_type.as_ref() == QUOTA_TENANT_SUBJECT_TYPE_ID
            || self
                .admitted_subjects
                .iter()
                .any(|admitted| subject_type.as_ref().starts_with(admitted.as_ref()))
    }

    /// Validate the whole metric object against the resolved schema.
    ///
    /// # Errors
    ///
    /// The violations, for the log.
    pub fn validate(&self, document: &Value) -> Result<(), Vec<String>> {
        validate(&self.validator, document)
    }
}

impl fmt::Debug for ServedMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServedMetric")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("mode", &self.mode)
            .field("admitted_subjects", &self.admitted_subjects)
            .finish_non_exhaustive()
    }
}

/// One metric type the catalogue has read.
#[domain_model]
#[derive(Debug, Clone)]
pub enum CatalogEntry {
    /// Requests and writes for the metric run.
    Served(Arc<ServedMetric>),
    /// The type is not a valid metric type; requests and writes that check
    /// the metric type fail with `FailedPrecondition`.
    Unavailable {
        /// The failed check.
        reason: ValidationReason,
    },
}

/// Build the entry of `registered`.
// @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-concrete
#[must_use]
pub fn build_entry(registered: RegisteredType) -> CatalogEntry {
    let unavailable = |reason| CatalogEntry::Unavailable { reason };
    if registered.is_abstract {
        return unavailable(ValidationReason::Abstract);
    }
    if !derives_from(&registered.id, QUOTA_METRIC_TYPE_ID) {
        return unavailable(ValidationReason::NotDerived);
    }
    if own_major(&registered.id).is_none_or(|major| major == 0) {
        return unavailable(ValidationReason::MajorVersionZero);
    }
    let Ok(traits) = MetricTraits::deserialize(&registered.effective_traits) else {
        return unavailable(ValidationReason::TraitInvalid);
    };
    let mut admitted_subjects = Vec::with_capacity(traits.admitted_subjects.len());
    for raw in &traits.admitted_subjects {
        match GtsTypeId::try_new(raw) {
            Ok(id) if id.as_ref().starts_with(QUOTA_SUBJECT_TYPE_ID) => admitted_subjects.push(id),
            _ => return unavailable(ValidationReason::TraitInvalid),
        }
    }
    let Ok(validator) = registered.payload else {
        return unavailable(ValidationReason::SchemaInvalid);
    };
    // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-index
    let id = MetricId::new(registered.id);
    let label = MetricLabel::admitted(&id);
    CatalogEntry::Served(Arc::new(ServedMetric {
        id,
        kind: traits.kind,
        mode: traits.enforcement,
        admitted_subjects,
        schema: Arc::new(registered.schema),
        validator,
        label,
    }))
    // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-index
}
// @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-concrete

/// Whether `id` is a type derived from `base` (never `base` itself). GTS ids
/// are chained, so the prefix is the ancestry.
#[must_use]
pub fn derives_from(id: &GtsTypeId, base: &str) -> bool {
    id.as_ref().len() > base.len() && id.as_ref().starts_with(base)
}

/// The major version of the type's own segment.
fn own_major(id: &GtsTypeId) -> Option<u32> {
    GtsId::try_new(id.as_ref())
        .ok()?
        .segments()
        .last()?
        .ver_major_opt()
}

/// Validate `document` with `validator`, flattening the diagnostics.
///
/// # Errors
///
/// The JSON Schema violations or, when there are none, the GTS reference
/// violations.
pub fn validate(validator: &PayloadValidator, document: &Value) -> Result<(), Vec<String>> {
    validator.validate(document).map_err(|error| match error {
        PayloadValidationError::Constraints(violations) => violations,
        PayloadValidationError::References(violations) => violations
            .into_iter()
            .map(|v| format!("{}: {}", v.field_path, v.reason))
            .collect(),
        PayloadValidationError::Unexplained(reason) => vec![reason],
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "model_tests.rs"]
mod model_tests;

//! The expected children of a fact version (DESIGN §3.1 "Expected children of a fact"; feature 06
//! §3.3; T-D-75, T-D-87): what roll-up waits for, recorded as `expected_manifest_digest` on the
//! fact head once the fact is bound, and never derived from the counters that happen to exist.
//!
//! - `period_line` (recurring): exactly one child over the billing period.
//! - `one_time`: none while R-19 is open (T-D-18).
//! - usage, `subscription_line` scope: one child per window of `window_geometry(rating_window,
//!   served extent)` that holds a chargeable or unresolved second. A usage fact is one item, and
//!   its bindings must agree on the window, the scope, the meter and the dimension value (one cell
//!   per item and line at launch, R-16); otherwise its child over the billing period fails
//!   `usage_policy_mismatch`. A `resource`-scoped policy is LAUNCH-GATED: its child fails
//!   `resource_scope_not_supported`.
//! - a `zeroed` version: none.

use bss_rating_core::engine::current::geometry::{FactExtent, window_geometry};
use bss_rating_core::engine::current::input::{
    AggregationKey, AggregationScope, ChildKind, CoreUsagePolicy, PolicyScope,
};
use bss_rating_core::{Interval, RatingWindow};
use serde_json::json;
use time::OffsetDateTime;
use toolkit_macros::domain_model;

use crate::domain::child::{ChildFailure, ChildStatus, NewChild};
use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::fact::key::{ChildKey, window_child_key};
use crate::domain::fact::model::{AcceptedTerm, FactKind, FactVersion, Obligation, Timing};

/// What a usage fact is rated under: the one policy, meter and cell its bindings agree on.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsagePlan {
    /// The binding's usage rating policy.
    pub policy: CoreUsagePolicy,
    /// The derived meter, `products.derived/<code>@<n>`.
    pub meter: String,
    /// The meter reference's version text.
    pub meter_version: String,
    /// The binding's dimension key.
    pub dimension_key: Option<String>,
    /// The dimension value pinned at sale.
    pub dimension_value: Option<String>,
    /// The binding of record of the first chargeable slice.
    pub binding_digest: Sha256Hex,
}

/// How a usage fact's bindings shape its children.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageShape {
    /// No chargeable slice names a term: one child over the billing period waits for a version
    /// that does.
    NoTerms,
    /// The bindings disagree, or one is not a usage binding.
    Mismatch,
    /// One plan, `resource`-scoped: LAUNCH-GATED.
    ResourceScope(UsagePlan),
    /// One plan, `subscription_line`-scoped.
    Plan(UsagePlan),
}

/// The plan of a usage fact's bindings.
#[must_use]
pub fn usage_shape(fact: &FactVersion) -> UsageShape {
    let terms = fact.chargeable_terms();
    let Some(first) = terms.first() else {
        return UsageShape::NoTerms;
    };
    let Some(plan) = plan_of(first) else {
        return UsageShape::Mismatch;
    };
    let agrees = terms.iter().all(|term| {
        plan_of(term).is_some_and(|other| {
            other.policy.rating_window == plan.policy.rating_window
                && other.policy.aggregation_scope == plan.policy.aggregation_scope
                && other.meter == plan.meter
                && other.dimension_key == plan.dimension_key
                && other.dimension_value == plan.dimension_value
        })
    });
    if !agrees {
        return UsageShape::Mismatch;
    }
    match plan.policy.aggregation_scope {
        PolicyScope::SubscriptionLine => UsageShape::Plan(plan),
        PolicyScope::Resource => UsageShape::ResourceScope(plan),
    }
}

fn plan_of(term: &AcceptedTerm) -> Option<UsagePlan> {
    let binding = &term.binding;
    let policy = binding.usage_policy.clone()?;
    let meter = binding.meter.as_ref()?;
    Some(UsagePlan {
        policy,
        meter: meter.usage_type_id.clone(),
        meter_version: meter.version.clone(),
        dimension_key: binding.dimension_key.clone(),
        dimension_value: binding.dimension_value.clone(),
        binding_digest: term.binding_digest.clone(),
    })
}

/// One expected child.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedChild {
    /// Its identity.
    pub key: ChildKey,
    /// What kind of child.
    pub kind: ChildKind,
    /// Its window.
    pub window: RatingWindow,
    /// Its aggregation key.
    pub aggregation_key: AggregationKey,
    /// The dimension value pinned at sale.
    pub dimension_value: Option<String>,
    /// Whether fact intake creates it; the scheduler creates the others when they are due.
    pub at_intake: bool,
    /// `pending`, or `failed` for a child intake fails closed.
    pub status: ChildStatus,
    /// Why it failed.
    pub failure: Option<ChildFailure>,
    /// Its first time wake-up.
    pub next_check_at: Option<OffsetDateTime>,
}

impl ExpectedChild {
    /// The row fact intake inserts for this child.
    #[must_use]
    pub fn new_child(&self, fact: &FactVersion, shard: i16) -> NewChild {
        NewChild {
            child_id: self.key.child_id,
            tenant_id: fact.fact.tenant_id,
            fact_id: fact.fact.fact_id,
            shard,
            kind: self.kind,
            subscription_id: fact.subscription_id,
            item_id: fact.item_id,
            dimension_value: self.dimension_value.clone(),
            aggregation_key: self.aggregation_key.clone(),
            agg_key_digest: self.key.agg_key_digest.clone(),
            window: self.window,
            status: self.status,
            failure: self.failure,
            next_check_at: self.next_check_at,
        }
    }
}

/// The expected set of a fact version and, for a scheduled usage fact, its plan.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Expected {
    /// Every expected child, in window order.
    pub children: Vec<ExpectedChild>,
    /// The plan the usage schedule is created from; `None` when nothing is scheduled.
    pub schedule: Option<UsagePlan>,
}

impl Expected {
    /// `expected_manifest_digest`: SHA-256 over the canonical JSON of the sorted child ids.
    #[must_use]
    pub fn manifest_digest(&self) -> Sha256Hex {
        let mut ids: Vec<String> = self
            .children
            .iter()
            .map(|child| child.key.child_id.to_string())
            .collect();
        ids.sort_unstable();
        Sha256Hex::of_json(&json!({ "manifest": 1, "children": ids }))
    }
}

/// The expected children of `fact` as of `now` (the wake-up of an advance `period_line`).
///
/// # Errors
///
/// [`DomainError::Internal`] when the served extent does not fit the period, which the profile
/// checks rule out.
pub fn expected_children(fact: &FactVersion, now: OffsetDateTime) -> Result<Expected, DomainError> {
    let empty = Expected {
        children: Vec::new(),
        schedule: None,
    };
    if fact.obligation == Obligation::Zeroed {
        return Ok(empty);
    }
    match fact.kind {
        // @cpt-begin:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-single
        // @cpt-begin:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-one-time
        FactKind::OneTime => Ok(empty),
        // @cpt-end:cpt-cf-bss-rating-flow-schedule-accept-fact:p1:inst-af-one-time
        FactKind::Recurring => {
            let next_check_at = match fact.timing {
                Timing::Advance => now,
                Timing::Arrears => fact.served.end,
            };
            let child = single(
                fact,
                None,
                None,
                ChildStatus::Pending,
                None,
                Some(next_check_at),
            )?;
            Ok(Expected {
                children: vec![child],
                schedule: None,
            })
        }
        // @cpt-end:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-single
        FactKind::Usage => usage_children(fact),
    }
}

fn usage_children(fact: &FactVersion) -> Result<Expected, DomainError> {
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-usage
    let plan = match usage_shape(fact) {
        // A served extent that is only nonchargeable expects no usage child; one that holds
        // unresolved time but no chargeable term expects one child over the billing period, which
        // waits for a version that names the terms.
        UsageShape::NoTerms if !fact.slices.iter().any(|slice| slice.coverage.is_rated()) => {
            return Ok(Expected {
                children: Vec::new(),
                schedule: None,
            });
        }
        UsageShape::NoTerms => {
            let child = single(fact, None, None, ChildStatus::Pending, None, None)?;
            return Ok(Expected {
                children: vec![child],
                schedule: None,
            });
        }
        // @cpt-begin:cpt-cf-bss-rating-state-schedule-child-creation:p1:inst-cc-gated
        UsageShape::Mismatch => {
            let failure = Some(ChildFailure::UsagePolicyMismatch);
            let child = single(fact, None, None, ChildStatus::Failed, failure, None)?;
            return Ok(Expected {
                children: vec![child],
                schedule: None,
            });
        }
        UsageShape::ResourceScope(plan) => {
            let failure = Some(ChildFailure::ResourceScopeNotSupported);
            let child = single(
                fact,
                Some(&plan),
                plan.dimension_value.clone(),
                ChildStatus::Failed,
                failure,
                None,
            )?;
            return Ok(Expected {
                children: vec![child],
                schedule: None,
            });
        }
        // @cpt-end:cpt-cf-bss-rating-state-schedule-child-creation:p1:inst-cc-gated
        UsageShape::Plan(plan) => plan,
    };
    let extent = FactExtent {
        served: fact.served,
        period: fact.period,
    };
    // @cpt-begin:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-event
    // `window_geometry` knows only `BillingCycle` and `CalendarHour`: no per-event child exists.
    let windows = window_geometry(&plan.policy, extent).map_err(|e| {
        DomainError::Internal(format!("window geometry of a checked fact: {e}"), None)
    })?;
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-event
    let key = aggregation_key(fact, Some(&plan));
    let mut children = Vec::with_capacity(windows.len());
    for window in windows.into_iter().filter(|window| is_rated(fact, window)) {
        children.push(ExpectedChild {
            key: window_child_key(fact.fact.fact_id, window.window_start, &key)?,
            kind: ChildKind::UsageWindow,
            window,
            aggregation_key: key.clone(),
            dimension_value: plan.dimension_value.clone(),
            at_intake: false,
            status: ChildStatus::Pending,
            failure: None,
            next_check_at: None,
        });
    }
    // @cpt-end:cpt-cf-bss-rating-algo-schedule-expected-children:p1:inst-ec-usage
    Ok(Expected {
        children,
        schedule: Some(plan),
    })
}

/// Whether a usage window holds a chargeable or unresolved second of the fact.
fn is_rated(fact: &FactVersion, window: &RatingWindow) -> bool {
    fact.slices.iter().any(|slice| {
        slice.coverage.is_rated()
            && slice.span.start < window.served_to
            && window.served_from < slice.span.end
    })
}

/// The one child of a fact over its billing period, created at intake.
fn single(
    fact: &FactVersion,
    plan: Option<&UsagePlan>,
    dimension_value: Option<String>,
    status: ChildStatus,
    failure: Option<ChildFailure>,
    next_check_at: Option<OffsetDateTime>,
) -> Result<ExpectedChild, DomainError> {
    let kind = match fact.kind {
        FactKind::Recurring => ChildKind::PeriodLine,
        FactKind::Usage => ChildKind::UsageWindow,
        FactKind::OneTime => ChildKind::OneTime,
    };
    let key = aggregation_key(fact, plan);
    Ok(ExpectedChild {
        key: window_child_key(fact.fact.fact_id, fact.period.start, &key)?,
        kind,
        window: billing_window(fact.period, fact.served),
        aggregation_key: key,
        dimension_value,
        at_intake: true,
        status,
        failure,
        next_check_at,
    })
}

const fn billing_window(period: Interval, served: Interval) -> RatingWindow {
    RatingWindow {
        window_start: period.start,
        window_end: period.end,
        served_from: served.start,
        served_to: served.end,
    }
}

fn aggregation_key(fact: &FactVersion, plan: Option<&UsagePlan>) -> AggregationKey {
    AggregationKey {
        tenant_axes: fact.axes,
        subscription_id: fact.subscription_id,
        sub_line_key: fact.sub_line_key.clone(),
        item_id: fact.item_id,
        meter: plan.map(|plan| plan.meter.clone()),
        dimension_key: plan.and_then(|plan| plan.dimension_key.clone()),
        scope: AggregationScope::SubscriptionLine,
    }
}

#[cfg(test)]
#[path = "expected_tests.rs"]
mod tests;

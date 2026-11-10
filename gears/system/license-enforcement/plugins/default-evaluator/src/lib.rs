//! Default grant evaluator of the license-enforcement gear (DESIGN §3.1,
//! Default evaluator).
//!
//! A binding matches the request subject, on its type or one of its base types,
//! or the tenant of the request on the tenant Subject type. Conditions and recipient rules are
//! not supported: `validate` rejects any non-empty rules.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::sync::Arc;

use gts::{GtsSchema, GtsTypeId};
use license_enforcement_sdk::models::{Conditions, Grant, GrantState};
use license_enforcement_sdk::{
    BindingFilter, Evaluation, EvaluationError, EvaluatorDenial, GrantEvaluatorV1,
};
use license_resolver_sdk::{LicenseCheckRequest, LicenseTenantSubjectV1};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_canonical_errors::FieldViolation;

license_enforcement_sdk::register_evaluator!(|| Arc::new(DefaultEvaluator));

/// `field_violations[].reason` of non-empty conditions or recipient rules.
const RULES_INVALID: &str = license_enforcement_sdk::error::reason::RULES_INVALID;

/// The default evaluator.
pub struct DefaultEvaluator;

fn tenant_subject_type() -> GtsTypeId {
    GtsTypeId::new(<LicenseTenantSubjectV1 as GtsSchema>::TYPE_ID)
}

/// `type_id` and its base types, most derived first, without the first chain
/// segment, the Subject base type: every Subject type derives from it.
fn self_and_bases(type_id: &GtsTypeId) -> impl Iterator<Item = GtsTypeId> + '_ {
    let id = type_id.as_ref();
    id.match_indices('~')
        .map(|(i, _)| i + 1)
        .skip(1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|end| GtsTypeId::new(&id[..end]))
}

fn is_empty(rules: &Conditions) -> bool {
    match &rules.0 {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn state_denial(state: GrantState) -> Option<EvaluatorDenial> {
    match state {
        GrantState::Active => None,
        GrantState::AwaitingAcceptance => Some(EvaluatorDenial::AwaitingAcceptance),
        GrantState::Suspended => Some(EvaluatorDenial::Suspended),
        // `GrantState` is non-exhaustive: a state this crate does not know denies.
        _ => Some(EvaluatorDenial::Terminated),
    }
}

impl GrantEvaluatorV1 for DefaultEvaluator {
    fn named_types(&self) -> Vec<GtsTypeId> {
        vec![tenant_subject_type()]
    }

    fn candidate_filter(&self, request: &LicenseCheckRequest) -> Vec<BindingFilter> {
        let mut filters: Vec<BindingFilter> = match &request.subject.id {
            Some(id) => self_and_bases(&request.subject.gts_type)
                .map(|t| BindingFilter::exact(t, id.clone()))
                .collect(),
            None => Vec::new(),
        };
        let tenant =
            BindingFilter::new(tenant_subject_type(), request.context.tenant_id.to_string());
        // The tenant filter covers an exact one on the same type and id.
        filters
            .retain(|f| f.type_pattern != tenant.type_pattern || f.binding_id != tenant.binding_id);
        filters.push(tenant);
        filters
    }

    fn evaluate(
        &self,
        _request: &LicenseCheckRequest,
        candidate: &Grant,
        root: &Grant,
        instant: OffsetDateTime,
    ) -> Evaluation {
        if let Some(reason) = state_denial(root.state).or_else(|| state_denial(candidate.state)) {
            return Evaluation::Deny(reason);
        }
        let window = candidate.validity.window();
        let in_window = if instant < window.not_before {
            return Evaluation::Deny(EvaluatorDenial::NotYetValid);
        } else if instant < window.expires_at {
            Evaluation::Pass
        } else {
            match window.grace_until {
                Some(grace_until) if instant < grace_until => Evaluation::PassInGrace,
                Some(_) => return Evaluation::Deny(EvaluatorDenial::GraceExpired),
                None => return Evaluation::Deny(EvaluatorDenial::Expired),
            }
        };
        // Write paths reject conditions; a stored one denies instead of being ignored.
        if candidate.conditions.as_ref().is_some_and(|c| !is_empty(c)) {
            return Evaluation::Deny(EvaluatorDenial::ConditionUnmet);
        }
        in_window
    }

    /// The caller sets `field` of each violation to the path of the rules.
    fn validate(&self, rules: &Conditions, _schema: &Value) -> Vec<FieldViolation> {
        if is_empty(rules) {
            Vec::new()
        } else {
            vec![FieldViolation::new(
                "",
                "the default evaluator does not support conditions or recipient rules",
                RULES_INVALID,
            )]
        }
    }

    fn matches(&self, rules: &Conditions, _object: &Value) -> Result<bool, EvaluationError> {
        if is_empty(rules) {
            Ok(true)
        } else {
            Err(EvaluationError::Malformed(
                "the default evaluator does not support rules".to_owned(),
            ))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "lib_tests.rs"]
mod lib_tests;

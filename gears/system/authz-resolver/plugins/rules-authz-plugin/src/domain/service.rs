//! Service implementation for the rules `AuthZ` resolver plugin.
//!
//! Decision procedure (fail closed):
//! 0. An unconditional grant allows with no constraints only for its exact subject (ID and
//!    home tenant), resource type and action, and only when the PEP declares the resource
//!    property-less and does not require constraints. Otherwise it never applies.
//! 1. Candidate rules match the authenticated subject, exact resource type and exact action.
//! 2. A delegated rule needs an accepted proof reference in its configured property; a missing
//!    or unaccepted reference refuses only that rule, never another complete rule.
//! 3. A payer-use rule refuses a supplied proposed payer outside its set.
//! 4. Each path survives only if every predicate whose property the request supplies holds for
//!    the supplied value. Surviving paths (plus the payer-use predicate) become OR constraints.
//! 5. No surviving path means deny. Proof-caused denials carry a stable deny code.
//!
//! Paths of different rules are never merged: an incomplete path cannot borrow a predicate from
//! another rule.

use std::collections::HashMap;

use authz_resolver_sdk::{
    Constraint, DenyReason, EqPredicate, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, InPredicate, Predicate,
};
use toolkit_macros::domain_model;
use toolkit_security::pep_properties;
use uuid::Uuid;

use crate::config::{PathConfig, RuleConfig, RulesAuthZPluginConfig, UnconditionalGrantConfig};

/// Deny code: a delegated path required a proof reference that was not presented. **Interim**
/// code pending platform agreement on delegation-proof deny reasons.
pub const DENY_DELEGATION_PROOF_REQUIRED: &str = "delegation_proof_required";
/// Deny code: a presented proof reference is not accepted. **Interim** (see above).
pub const DENY_DELEGATION_PROOF_INVALID: &str = "delegation_proof_invalid";
/// Deny code when no configured rule grants the request.
pub const DENY_NO_MATCHING_RULE: &str = "no_matching_rule";

/// Rules `AuthZ` resolver service.
#[domain_model]
pub struct Service {
    policy_revision: String,
    rules: Vec<RuleConfig>,
    unconditional_grants: Vec<UnconditionalGrantConfig>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProofFailure {
    Required,
    Invalid,
}

impl Service {
    /// Build from a validated configuration.
    ///
    /// # Errors
    /// Returns the configuration validation error.
    pub fn from_config(config: &RulesAuthZPluginConfig) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self {
            policy_revision: config.policy_revision.clone(),
            rules: config.rules.clone(),
            unconditional_grants: config.unconditional_grants.clone(),
        })
    }

    /// Revision of the loaded policy.
    #[must_use]
    pub fn policy_revision(&self) -> &str {
        &self.policy_revision
    }

    /// Evaluate an authorization request.
    #[must_use]
    pub fn evaluate(&self, request: &EvaluationRequest) -> EvaluationResponse {
        if self
            .unconditional_grants
            .iter()
            .any(|grant| grant_applies(grant, request))
        {
            return EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            };
        }
        let supplied = supplied_properties(request);
        let mut constraints = Vec::new();
        let mut proof_failure: Option<ProofFailure> = None;
        for rule in self.rules.iter().filter(|rule| applies(rule, request)) {
            if let Some(failure) = delegation_failure(rule, request) {
                proof_failure = proof_failure.max(Some(failure));
                continue;
            }
            if let Some(payer) = &rule.payer_use
                && supplied
                    .get(payer.property.as_str())
                    .is_some_and(|value| !value.is_some_and(|v| payer.values.contains(&v)))
            {
                continue;
            }
            for path in &rule.paths {
                if path_holds(path, &supplied) {
                    constraints.push(constraint(path, rule));
                }
            }
        }
        if constraints.is_empty() {
            let error_code = match proof_failure {
                Some(ProofFailure::Required) => DENY_DELEGATION_PROOF_REQUIRED,
                Some(ProofFailure::Invalid) => DENY_DELEGATION_PROOF_INVALID,
                None => DENY_NO_MATCHING_RULE,
            };
            return EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: Some(DenyReason {
                        error_code: error_code.to_owned(),
                        details: None,
                    }),
                },
            };
        }
        EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                deny_reason: None,
            },
        }
    }
}

fn subject_tenant(request: &EvaluationRequest) -> Option<Uuid> {
    request
        .subject
        .properties
        .get("tenant_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// The exact triple, on a property-less check that needs no row-level constraints.
fn grant_applies(grant: &UnconditionalGrantConfig, request: &EvaluationRequest) -> bool {
    let context = &request.context;
    context.supported_properties.is_empty()
        && !context.require_constraints
        && grant.subject.id == request.subject.id
        && subject_tenant(request) == Some(grant.subject.tenant_id)
        && grant.resource_type == request.resource.resource_type
        && grant.action == request.action.name
}

fn applies(rule: &RuleConfig, request: &EvaluationRequest) -> bool {
    let subject = &request.subject;
    let tenant = subject_tenant(request);
    rule.resource_type == request.resource.resource_type
        && rule.actions.contains(&request.action.name)
        && rule.subject.id.is_none_or(|id| id == subject.id)
        && rule.subject.tenant_id.is_none_or(|t| Some(t) == tenant)
        && rule
            .subject
            .subject_type
            .as_ref()
            .is_none_or(|t| subject.subject_type.as_ref() == Some(t))
}

fn delegation_failure(rule: &RuleConfig, request: &EvaluationRequest) -> Option<ProofFailure> {
    let delegation = rule.delegation.as_ref()?;
    match request
        .resource
        .properties
        .get(&delegation.proof_property)
        .and_then(serde_json::Value::as_str)
    {
        None => Some(ProofFailure::Required),
        Some(proof) if delegation.accepted.iter().any(|a| a == proof) => None,
        Some(_) => Some(ProofFailure::Invalid),
    }
}

/// Supplied UUID-valued properties; a present non-UUID value is `None` and satisfies nothing.
fn supplied_properties(request: &EvaluationRequest) -> HashMap<&str, Option<Uuid>> {
    let mut supplied: HashMap<&str, Option<Uuid>> = request
        .resource
        .properties
        .iter()
        .map(|(key, value)| {
            (
                key.as_str(),
                value.as_str().and_then(|s| Uuid::parse_str(s).ok()),
            )
        })
        .collect();
    if let Some(id) = request.resource.id {
        supplied.insert(pep_properties::RESOURCE_ID, Some(id));
    }
    supplied
}

fn path_holds(path: &PathConfig, supplied: &HashMap<&str, Option<Uuid>>) -> bool {
    path.predicates.iter().all(|predicate| {
        supplied
            .get(predicate.property.as_str())
            .is_none_or(|value| value.is_some_and(|v| predicate.values.contains(&v)))
    })
}

fn predicate(property: &str, values: &[Uuid]) -> Predicate {
    match values {
        [single] => Predicate::Eq(EqPredicate::new(property, *single)),
        _ => Predicate::In(InPredicate::new(property, values.iter().copied())),
    }
}

fn constraint(path: &PathConfig, rule: &RuleConfig) -> Constraint {
    let mut predicates: Vec<Predicate> = path
        .predicates
        .iter()
        .map(|p| predicate(&p.property, &p.values))
        .collect();
    if let Some(payer) = &rule.payer_use {
        predicates.push(predicate(&payer.property, &payer.values));
    }
    Constraint { predicates }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

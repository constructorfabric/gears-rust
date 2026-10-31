//! The compiled policy and its evaluation.
//!
//! [`Policy::compile`] turns a validated [`OrdersTestAuthZPluginConfig`] into matchers and
//! pre-built constraints once, at gear init; [`Policy::evaluate`] is then a pure function of
//! the request. It reads only the subject id and `tenant_id` property, the resource type and
//! properties, the action name and `require_constraints` — never token scopes, the tenant
//! context or capabilities — and emits only `eq`/`in` predicates.
//!
//! When the enforcement point does not require constraints it does not enforce them either
//! (Event Broker's tenant check discards the scope), so a rule is then decided locally against
//! the request's resource properties.

use authz_resolver_sdk::{
    Constraint, DenyReason, EqPredicate, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, InPredicate, Predicate,
};
use serde_json::Value;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::config::{
    ConfigError, Grant, OrdersTestAuthZPluginConfig, PredicateSpec, Rule, SubjectMatch,
    UUID_PROPERTIES,
};

/// The wildcard token: a trailing resource-type suffix, or the sole action.
const WILDCARD: &str = "*";

/// The subject property a declared subject tenant is compared with.
const SUBJECT_TENANT_PROPERTY: &str = "tenant_id";

/// Deny reason: a delegation rule matched but the request carries no proof.
pub const DELEGATION_PROOF_REQUIRED: &str = "delegation_proof_required";

/// Deny reason: a delegation rule matched but the request's proof is not accepted.
pub const DELEGATION_PROOF_INVALID: &str = "delegation_proof_invalid";

/// A compiled, immutable policy.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Policy {
    rules: Vec<CompiledRule>,
    grants: Vec<CompiledGrant>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    subject: SubjectMatcher,
    resource: Matcher,
    actions: Actions,
    paths: Vec<Constraint>,
    /// The same paths as `paths`, as local checks against the request's resource properties.
    checks: Vec<Vec<PropertyCheck>>,
    delegation: Option<CompiledDelegation>,
}

/// One predicate as a local check: the resource property must be present and hold one of the
/// accepted values (`eq` accepts exactly one).
#[derive(Debug, Clone)]
struct PropertyCheck {
    property: String,
    accepted: Accepted,
}

/// Accepted values of a [`PropertyCheck`]: parsed UUIDs for a [`UUID_PROPERTIES`] property,
/// strings otherwise.
#[derive(Debug, Clone)]
enum Accepted {
    Uuids(Vec<Uuid>),
    Strings(Vec<String>),
}

/// A rule's delegation requirement: the resource property carrying the proof, and the proofs
/// accepted.
#[derive(Debug, Clone)]
struct CompiledDelegation {
    proof_property: String,
    accepted: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompiledGrant {
    subject: SubjectMatcher,
    resource: Matcher,
    action: Actions,
}

#[derive(Debug, Clone)]
struct SubjectMatcher {
    id: Uuid,
    tenant: Option<Uuid>,
}

#[derive(Debug, Clone)]
enum Matcher {
    Exact(String),
    Prefix(String),
}

#[derive(Debug, Clone)]
enum Actions {
    Any,
    Named(Vec<String>),
}

/// Why a delegation rule did not match, in increasing precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DelegationFailure {
    Invalid,
    Required,
}

impl Policy {
    /// Validate `config` and compile it.
    ///
    /// # Errors
    ///
    /// The first [`ConfigError`] that [`OrdersTestAuthZPluginConfig::validate`] reports.
    pub fn compile(config: &OrdersTestAuthZPluginConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            rules: config.rules.iter().map(CompiledRule::from).collect(),
            grants: config
                .unconditional_grants
                .iter()
                .map(CompiledGrant::from)
                .collect(),
        })
    }

    /// Number of compiled rules.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Number of compiled unconditional grants.
    #[must_use]
    pub fn grant_count(&self) -> usize {
        self.grants.len()
    }

    /// Answer `request`.
    ///
    /// Matching rules permit under the union of their paths, in declaration order; when the PEP
    /// does not require constraints a rule matches only if one of its paths is satisfied by the
    /// request's resource properties (it is decided here, not by the PEP). Else a matching grant
    /// permits with no constraints when the PEP does not require constraints; else a delegation
    /// rule that matched (a rule decided locally and not satisfied never matches, so records no
    /// reason) without an accepted proof denies with [`DELEGATION_PROOF_REQUIRED`]
    /// (preferred) or [`DELEGATION_PROOF_INVALID`]; else the request is denied with no reason.
    /// A policy outcome is never an error.
    #[must_use]
    pub fn evaluate(&self, request: &EvaluationRequest) -> EvaluationResponse {
        let mut constraints = Vec::new();
        let mut matched = false;
        let mut failure: Option<DelegationFailure> = None;

        let decide_locally = !request.context.require_constraints;

        for rule in self.rules.iter().filter(|r| r.applies_to(request)) {
            // A rule the request does not satisfy is skipped before its delegation check, so
            // it neither matches nor records a delegation reason.
            if decide_locally && !rule.satisfied_by(request) {
                continue;
            }
            match rule.delegation_check(request) {
                Ok(()) => {
                    matched = true;
                    constraints.extend(rule.paths.iter().cloned());
                }
                Err(reason) => failure = failure.max(Some(reason)),
            }
        }

        if matched {
            return permit(constraints);
        }

        if decide_locally && self.grants.iter().any(|g| g.applies_to(request)) {
            return permit(Vec::new());
        }

        let deny_reason = failure.map(|f| DenyReason {
            error_code: match f {
                DelegationFailure::Required => DELEGATION_PROOF_REQUIRED,
                DelegationFailure::Invalid => DELEGATION_PROOF_INVALID,
            }
            .to_owned(),
            details: None,
        });
        EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason,
            },
        }
    }
}

fn permit(constraints: Vec<Constraint>) -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints,
            deny_reason: None,
        },
    }
}

impl CompiledRule {
    fn applies_to(&self, request: &EvaluationRequest) -> bool {
        self.subject.matches(request)
            && self.resource.matches(&request.resource.resource_type)
            && self.actions.matches(&request.action.name)
    }

    fn delegation_check(&self, request: &EvaluationRequest) -> Result<(), DelegationFailure> {
        let Some(delegation) = &self.delegation else {
            return Ok(());
        };
        match request.resource.properties.get(&delegation.proof_property) {
            None => Err(DelegationFailure::Required),
            Some(Value::String(proof)) if delegation.accepted.iter().any(|a| a == proof) => Ok(()),
            Some(_) => Err(DelegationFailure::Invalid),
        }
    }

    /// At least one path has every predicate satisfied by the request's resource properties.
    fn satisfied_by(&self, request: &EvaluationRequest) -> bool {
        self.checks
            .iter()
            .any(|path| path.iter().all(|check| check.satisfied_by(request)))
    }
}

impl PropertyCheck {
    /// The property is present as a string holding an accepted value; for a UUID property the
    /// string must parse as a UUID equal to an accepted one (compared parsed, so case and
    /// hyphenation do not matter).
    fn satisfied_by(&self, request: &EvaluationRequest) -> bool {
        let Some(actual) = request
            .resource
            .properties
            .get(&self.property)
            .and_then(Value::as_str)
        else {
            return false;
        };
        match &self.accepted {
            Accepted::Uuids(accepted) => {
                Uuid::parse_str(actual).is_ok_and(|actual| accepted.contains(&actual))
            }
            Accepted::Strings(accepted) => accepted.iter().any(|a| a == actual),
        }
    }
}

impl From<&PredicateSpec> for PropertyCheck {
    /// A validated config holds parsable UUIDs for a UUID property.
    fn from(spec: &PredicateSpec) -> Self {
        let values: Vec<&str> = match &spec.value {
            Some(value) => vec![value.as_str()],
            None => spec.values.iter().map(String::as_str).collect(),
        };
        let accepted = if UUID_PROPERTIES.contains(&spec.property.as_str()) {
            Accepted::Uuids(
                values
                    .iter()
                    .filter_map(|v| Uuid::parse_str(v).ok())
                    .collect(),
            )
        } else {
            Accepted::Strings(values.iter().map(|v| (*v).to_owned()).collect())
        };
        Self {
            property: spec.property.clone(),
            accepted,
        }
    }
}

impl CompiledGrant {
    fn applies_to(&self, request: &EvaluationRequest) -> bool {
        self.subject.matches(request)
            && self.resource.matches(&request.resource.resource_type)
            && self.action.matches(&request.action.name)
    }
}

impl SubjectMatcher {
    fn matches(&self, request: &EvaluationRequest) -> bool {
        if request.subject.id != self.id {
            return false;
        }
        let Some(tenant) = self.tenant else {
            return true;
        };
        request
            .subject
            .properties
            .get(SUBJECT_TENANT_PROPERTY)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            == Some(tenant)
    }
}

impl Matcher {
    fn new(resource_type: &str) -> Self {
        match resource_type.strip_suffix(WILDCARD) {
            Some(prefix) => Self::Prefix(prefix.to_owned()),
            None => Self::Exact(resource_type.to_owned()),
        }
    }

    fn matches(&self, resource_type: &str) -> bool {
        match self {
            Self::Exact(exact) => resource_type == exact,
            Self::Prefix(prefix) => resource_type.starts_with(prefix.as_str()),
        }
    }
}

impl Actions {
    fn new(actions: &[String]) -> Self {
        if matches!(actions, [a] if a == WILDCARD) {
            Self::Any
        } else {
            Self::Named(actions.to_vec())
        }
    }

    fn matches(&self, action: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Named(names) => names.iter().any(|n| n == action),
        }
    }
}

impl From<&SubjectMatch> for SubjectMatcher {
    fn from(subject: &SubjectMatch) -> Self {
        Self {
            id: subject.id,
            tenant: subject.tenant_id,
        }
    }
}

impl From<&Rule> for CompiledRule {
    fn from(rule: &Rule) -> Self {
        Self {
            subject: SubjectMatcher::from(&rule.subject),
            resource: Matcher::new(&rule.resource_type),
            actions: Actions::new(&rule.actions),
            paths: rule
                .paths
                .iter()
                .map(|path| Constraint {
                    predicates: path.predicates.iter().map(compile_predicate).collect(),
                })
                .collect(),
            checks: rule
                .paths
                .iter()
                .map(|path| path.predicates.iter().map(PropertyCheck::from).collect())
                .collect(),
            delegation: rule.delegation.as_ref().map(|d| CompiledDelegation {
                proof_property: d.proof_property.clone(),
                accepted: d.accepted.clone(),
            }),
        }
    }
}

impl From<&Grant> for CompiledGrant {
    fn from(grant: &Grant) -> Self {
        Self {
            subject: SubjectMatcher::from(&grant.subject),
            resource: Matcher::new(&grant.resource_type),
            action: Actions::new(std::slice::from_ref(&grant.action)),
        }
    }
}

/// `value` → `eq`, `values` → `in`; values travel as JSON strings (a validated config has
/// exactly one of the two forms).
fn compile_predicate(spec: &PredicateSpec) -> Predicate {
    match &spec.value {
        Some(value) => Predicate::Eq(EqPredicate::new(spec.property.as_str(), value.as_str())),
        None => Predicate::In(InPredicate::new(
            spec.property.as_str(),
            spec.values.iter().map(String::as_str),
        )),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "policy_tests.rs"]
mod policy_tests;

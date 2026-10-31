//! Policy configuration for the e2e policy `AuthZ` resolver plugin.
//!
//! The gear block (`bss-orders-test-authz-plugin`) declares the policy as data: `rules` that answer
//! with constraints and `unconditional_grants` that answer without. [`OrdersTestAuthZPluginConfig::validate`]
//! refuses a policy the evaluator could not answer unambiguously, so a bad suite config fails
//! at gear init rather than as a surprising decision.

use std::collections::HashSet;

use serde::Deserialize;
use uuid::Uuid;

/// The wildcard token: a trailing resource-type suffix, or the sole action.
const WILDCARD: &str = "*";

/// Predicate properties whose values must be UUIDs; the PEP compilers bind them to UUID
/// columns. Values of any other property are free text.
pub const UUID_PROPERTIES: [&str; 6] = [
    "owner_tenant_id",
    "id",
    "resource_tenant_id",
    "seller_tenant_id",
    "payer_tenant_id",
    "subject_tenant_id",
];

/// Plugin configuration: registration settings plus the declared policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdersTestAuthZPluginConfig {
    /// Vendor name for GTS instance registration.
    #[serde(default = "default_vendor")]
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    #[serde(default = "default_priority")]
    pub priority: i16,

    /// Free-text revision of the declared policy, logged at init.
    #[serde(default)]
    pub policy_revision: String,

    /// Rules that answer a matching request with constraints.
    #[serde(default)]
    pub rules: Vec<Rule>,

    /// Grants that answer a matching request with no constraints.
    #[serde(default)]
    pub unconditional_grants: Vec<Grant>,
}

impl Default for OrdersTestAuthZPluginConfig {
    fn default() -> Self {
        Self {
            vendor: default_vendor(),
            priority: default_priority(),
            policy_revision: String::new(),
            rules: Vec::new(),
            unconditional_grants: Vec::new(),
        }
    }
}

fn default_vendor() -> String {
    "constructorfabric".to_owned()
}

const fn default_priority() -> i16 {
    10
}

/// A policy rule: a matching request is permitted under the union of `paths`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Rule identifier, unique across rules and grants.
    pub id: String,

    /// The subject the rule applies to.
    pub subject: SubjectMatch,

    /// Resource type: non-empty, exact or a prefix when it ends with `*` (the head before the
    /// `*` must be non-empty, so a bare `*` is refused).
    pub resource_type: String,

    /// Action names, or exactly `["*"]` for any action.
    pub actions: Vec<String>,

    /// Constraint paths, OR-ed; each path's predicates are AND-ed.
    pub paths: Vec<Path>,

    /// When present, the rule matches only with an accepted delegation proof. It must name a
    /// non-blank proof property and at least one accepted value.
    #[serde(default)]
    pub delegation: Option<Delegation>,
}

/// An unconditional grant: a matching request is permitted with no constraints.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// Grant identifier, unique across rules and grants.
    pub id: String,

    /// The subject the grant applies to.
    pub subject: SubjectMatch,

    /// Resource type: non-empty, exact or a prefix when it ends with `*` (the head before the
    /// `*` must be non-empty, so a bare `*` is refused).
    pub resource_type: String,

    /// Action name, or `*` for any action.
    pub action: String,
}

/// Subject selector of a rule or grant.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectMatch {
    /// The subject id that must match exactly.
    pub id: Uuid,

    /// When present, the subject's `tenant_id` property must equal it.
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
}

/// One constraint path: its predicates are AND-ed.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Path {
    /// The path's predicates; at least one.
    pub predicates: Vec<PredicateSpec>,
}

/// One predicate: `values` becomes an `in` predicate, `value` an `eq` predicate.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredicateSpec {
    /// The PEP property the predicate constrains.
    pub property: String,

    /// Accepted values (`in`); exclusive with `value`.
    #[serde(default)]
    pub values: Vec<String>,

    /// The single accepted value (`eq`); exclusive with `values`.
    #[serde(default)]
    pub value: Option<String>,
}

/// Delegation requirement of a rule.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delegation {
    /// The resource property carrying the delegation proof reference.
    pub proof_property: String,

    /// Proof references the rule accepts.
    pub accepted: Vec<String>,
}

/// A policy the evaluator refuses to load.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Two rules or grants share an id.
    #[error("policy id `{0}` is declared more than once across rules and grants")]
    DuplicateId(String),

    /// A rule declares no action, or a grant an empty one.
    #[error("`{0}` declares no action")]
    EmptyActions(String),

    /// A rule declares no path (a constraint-less permit is a grant).
    #[error("rule `{0}` declares no path; use an unconditional grant for a constraint-less permit")]
    EmptyPaths(String),

    /// A rule's path declares no predicate.
    #[error("rule `{0}` has a path with no predicate")]
    EmptyPredicates(String),

    /// A predicate does not declare exactly one of a non-empty `values` or `value`.
    #[error(
        "rule `{0}`, property `{1}`: declare exactly one of a non-empty `values` or `value`, with no blank entry"
    )]
    PredicateValues(String, String),

    /// A UUID-typed property carries a value that is not a UUID.
    #[error("rule `{0}`, property `{1}`: every value must be a UUID")]
    InvalidUuid(String, String),

    /// A `*` outside a trailing resource-type suffix or a sole action, or a resource type that
    /// is only `*` (a prefix needs a non-empty head).
    #[error("`{0}`: `*` is accepted only as a trailing resource-type suffix or as the sole action")]
    Wildcard(String),

    /// A rule's or grant's resource type is empty.
    #[error("`{0}`: resource type must not be empty")]
    EmptyResourceType(String),

    /// A rule's delegation has a blank proof property, no accepted value, or a blank accepted
    /// value.
    #[error(
        "rule `{0}`: delegation needs a proof property and at least one non-blank accepted value"
    )]
    EmptyDelegation(String),

    /// A rule's predicate names a blank property.
    #[error("rule `{0}`: a predicate names a blank property")]
    EmptyProperty(String),
}

impl OrdersTestAuthZPluginConfig {
    /// Validate the declared policy.
    ///
    /// # Errors
    ///
    /// The first [`ConfigError`] found, in declaration order (rules, then grants).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut ids = HashSet::new();
        for id in self
            .rules
            .iter()
            .map(|r| &r.id)
            .chain(self.unconditional_grants.iter().map(|g| &g.id))
        {
            if !ids.insert(id.as_str()) {
                return Err(ConfigError::DuplicateId(id.clone()));
            }
        }
        self.rules.iter().try_for_each(Rule::validate)?;
        self.unconditional_grants
            .iter()
            .try_for_each(Grant::validate)
    }
}

impl Rule {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_resource_type(&self.id, &self.resource_type)?;
        if self.actions.is_empty() {
            return Err(ConfigError::EmptyActions(self.id.clone()));
        }
        let sole_wildcard = matches!(self.actions.as_slice(), [a] if a == WILDCARD);
        if !sole_wildcard && self.actions.iter().any(|a| a.contains(WILDCARD)) {
            return Err(ConfigError::Wildcard(self.id.clone()));
        }
        if self.actions.iter().any(String::is_empty) {
            return Err(ConfigError::EmptyActions(self.id.clone()));
        }
        if let Some(delegation) = &self.delegation
            && (delegation.proof_property.trim().is_empty()
                || delegation.accepted.is_empty()
                || delegation.accepted.iter().any(|a| a.trim().is_empty()))
        {
            return Err(ConfigError::EmptyDelegation(self.id.clone()));
        }
        if self.paths.is_empty() {
            return Err(ConfigError::EmptyPaths(self.id.clone()));
        }
        for path in &self.paths {
            if path.predicates.is_empty() {
                return Err(ConfigError::EmptyPredicates(self.id.clone()));
            }
            for predicate in &path.predicates {
                predicate.validate(&self.id)?;
            }
        }
        Ok(())
    }
}

impl Grant {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_resource_type(&self.id, &self.resource_type)?;
        if self.action.is_empty() {
            return Err(ConfigError::EmptyActions(self.id.clone()));
        }
        if self.action != WILDCARD && self.action.contains(WILDCARD) {
            return Err(ConfigError::Wildcard(self.id.clone()));
        }
        Ok(())
    }
}

impl PredicateSpec {
    fn validate(&self, rule_id: &str) -> Result<(), ConfigError> {
        if self.property.trim().is_empty() {
            return Err(ConfigError::EmptyProperty(rule_id.to_owned()));
        }
        let values: Vec<&str> = match (self.values.is_empty(), &self.value) {
            (false, None) => self.values.iter().map(String::as_str).collect(),
            (true, Some(value)) => vec![value.as_str()],
            _ => {
                return Err(ConfigError::PredicateValues(
                    rule_id.to_owned(),
                    self.property.clone(),
                ));
            }
        };
        if values.iter().any(|v| v.trim().is_empty()) {
            return Err(ConfigError::PredicateValues(
                rule_id.to_owned(),
                self.property.clone(),
            ));
        }
        if UUID_PROPERTIES.contains(&self.property.as_str())
            && values.iter().any(|v| Uuid::parse_str(v).is_err())
        {
            return Err(ConfigError::InvalidUuid(
                rule_id.to_owned(),
                self.property.clone(),
            ));
        }
        Ok(())
    }
}

/// A resource type is non-empty; `*` is accepted only as its last character (a prefix match)
/// and then only after a non-empty head.
fn validate_resource_type(id: &str, resource_type: &str) -> Result<(), ConfigError> {
    if resource_type.is_empty() {
        return Err(ConfigError::EmptyResourceType(id.to_owned()));
    }
    let head = resource_type
        .strip_suffix(WILDCARD)
        .unwrap_or(resource_type);
    if head.contains(WILDCARD) || head.is_empty() {
        return Err(ConfigError::Wildcard(id.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;

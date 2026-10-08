//! Configuration for the rules `AuthZ` resolver plugin.
//!
//! Every grant is explicit: a rule names its subject, resource type, actions and one or more
//! permission paths. An unconditional grant names one exact subject, resource type and action
//! and applies only to a property-less resource check. There is no default rule, wildcard,
//! allow-all or tenant-membership shortcut. A malformed configuration fails gear initialization.

use std::collections::BTreeSet;

use serde::Deserialize;
use uuid::Uuid;

/// Plugin configuration. Required; there is no default policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesAuthZPluginConfig {
    /// Vendor name for GTS instance registration and resolver selection.
    pub vendor: String,
    /// Plugin priority (lower = higher priority).
    pub priority: i16,
    /// Operator-visible revision of the loaded policy, logged at startup for evidence.
    pub policy_revision: String,
    /// Explicit grants. An empty list denies everything.
    pub rules: Vec<RuleConfig>,
    /// Explicit unconditional grants for property-less resources (for example Event Broker's
    /// `event_type` `produce` check). Absent means none.
    #[serde(default)]
    pub unconditional_grants: Vec<UnconditionalGrantConfig>,
}

/// One unconditional grant: exactly one subject may perform exactly one action on exactly one
/// resource type, with no constraints.
///
/// It applies only when the PEP declares the resource property-less (no supported constraint
/// properties) and does not require constraints, so it can never stand in for row-level scope
/// on a resource that has properties. There is no list, wildcard or partial subject form.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnconditionalGrantConfig {
    /// Unique grant identifier, shared namespace with rule ids (evidence and diagnostics only).
    pub id: String,
    /// The exact authenticated subject.
    pub subject: ExactSubject,
    /// Exact resource type name.
    pub resource_type: String,
    /// Exact action name.
    pub action: String,
}

/// An exact subject: both its ID and its home tenant must match.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactSubject {
    pub id: Uuid,
    pub tenant_id: Uuid,
}

/// One explicit grant: who may perform which actions on one resource type, along which paths.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConfig {
    /// Unique rule identifier (evidence and diagnostics only).
    pub id: String,
    /// Authenticated subject this rule applies to; every present field must match.
    pub subject: SubjectMatch,
    /// Exact resource type name.
    pub resource_type: String,
    /// Exact action names.
    pub actions: Vec<String>,
    /// OR alternatives; every predicate within one path is an AND requirement.
    pub paths: Vec<PathConfig>,
    /// Separate payer-use authority: the named property must be one of these values. Added to
    /// every path of this rule and checked against a supplied proposed value.
    #[serde(default)]
    pub payer_use: Option<PayerUseConfig>,
    /// Delegated path: the request must carry an accepted proof reference in this property.
    #[serde(default)]
    pub delegation: Option<DelegationConfig>,
}

/// Subject selector. At least one field is required; there is no "any subject" form.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectMatch {
    #[serde(default)]
    pub id: Option<Uuid>,
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
    #[serde(default)]
    pub subject_type: Option<String>,
}

/// One AND path of predicates.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathConfig {
    pub predicates: Vec<PredicateConfig>,
}

/// `property IN values` (a single value compiles to equality).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredicateConfig {
    pub property: String,
    pub values: Vec<Uuid>,
}

/// Separate payer-use authority condition.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayerUseConfig {
    pub property: String,
    pub values: Vec<Uuid>,
}

/// Delegated-path condition.
///
/// **Interim** proof check: presence and membership of an opaque reference in `accepted`. It
/// does not verify issuer keys, expiry or revocation; production delegation-proof verification
/// remains an open platform requirement.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationConfig {
    /// Request resource property carrying the presented proof reference.
    pub proof_property: String,
    /// Currently accepted references (revoke by removing).
    pub accepted: Vec<String>,
}

fn non_blank(value: &str, what: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty(),
        "rules-authz: {what} must not be empty"
    );
    anyhow::ensure!(
        !value.contains('*'),
        "rules-authz: {what} must not contain a wildcard"
    );
    Ok(())
}

impl RulesAuthZPluginConfig {
    /// Validate the complete policy before it is served.
    ///
    /// # Errors
    /// Returns an error for any empty, wildcard, nil or duplicate declaration.
    pub fn validate(&self) -> anyhow::Result<()> {
        non_blank(&self.vendor, "vendor")?;
        non_blank(&self.policy_revision, "policy_revision")?;
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            anyhow::ensure!(
                ids.insert(rule.id.as_str()),
                "rules-authz: duplicate rule id"
            );
            rule.validate()?;
        }
        let mut triples = BTreeSet::new();
        for grant in &self.unconditional_grants {
            anyhow::ensure!(
                ids.insert(grant.id.as_str()),
                "rules-authz: duplicate rule id"
            );
            grant.validate()?;
            anyhow::ensure!(
                triples.insert((
                    grant.subject.id,
                    grant.subject.tenant_id,
                    grant.resource_type.as_str(),
                    grant.action.as_str(),
                )),
                "rules-authz: unconditional grant {} duplicates another grant",
                grant.id
            );
        }
        Ok(())
    }
}

impl UnconditionalGrantConfig {
    fn validate(&self) -> anyhow::Result<()> {
        non_blank(&self.id, "unconditional grant id")?;
        non_blank(&self.resource_type, "resource_type")?;
        non_blank(&self.action, "action")?;
        anyhow::ensure!(
            !self.subject.id.is_nil() && !self.subject.tenant_id.is_nil(),
            "rules-authz: unconditional grant {} subject must not be nil",
            self.id
        );
        Ok(())
    }
}

impl RuleConfig {
    fn validate(&self) -> anyhow::Result<()> {
        non_blank(&self.id, "rule id")?;
        non_blank(&self.resource_type, "resource_type")?;
        let subject = &self.subject;
        anyhow::ensure!(
            subject.id.is_some() || subject.tenant_id.is_some() || subject.subject_type.is_some(),
            "rules-authz: rule {} must select a subject",
            self.id
        );
        anyhow::ensure!(
            subject.id.is_none_or(|id| !id.is_nil())
                && subject.tenant_id.is_none_or(|id| !id.is_nil()),
            "rules-authz: rule {} subject must not be nil",
            self.id
        );
        if let Some(kind) = &subject.subject_type {
            non_blank(kind, "subject_type")?;
        }
        anyhow::ensure!(
            !self.actions.is_empty(),
            "rules-authz: rule {} needs actions",
            self.id
        );
        for action in &self.actions {
            non_blank(action, "action")?;
        }
        anyhow::ensure!(
            !self.paths.is_empty(),
            "rules-authz: rule {} needs at least one path",
            self.id
        );
        for path in &self.paths {
            anyhow::ensure!(
                !path.predicates.is_empty(),
                "rules-authz: rule {} has an unconstrained path",
                self.id
            );
            for predicate in &path.predicates {
                values(&predicate.property, &predicate.values, &self.id)?;
            }
        }
        if let Some(payer) = &self.payer_use {
            values(&payer.property, &payer.values, &self.id)?;
        }
        if let Some(delegation) = &self.delegation {
            non_blank(&delegation.proof_property, "proof_property")?;
            anyhow::ensure!(
                !delegation.accepted.is_empty(),
                "rules-authz: rule {} delegation accepts nothing",
                self.id
            );
            for proof in &delegation.accepted {
                non_blank(proof, "accepted proof")?;
            }
        }
        Ok(())
    }
}

fn values(property: &str, values: &[Uuid], rule: &str) -> anyhow::Result<()> {
    non_blank(property, "predicate property")?;
    anyhow::ensure!(
        !values.is_empty() && values.iter().all(|v| !v.is_nil()),
        "rules-authz: rule {rule} predicate {property} needs non-nil values"
    );
    Ok(())
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;

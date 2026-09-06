// Created: 2026-09-06 by Constructor Tech
//! The Type Validator port and its vocabulary.
//!
//! The service consumes GTS types and never authors them. What it owns is the
//! decision to treat a type's rules as **hard** checks: a value that fails at
//! consumption time fails inside whichever gear read it, far from the
//! administrator who set it, so every rule the type declares rejects the value
//! here instead. The port is generic over any GTS type id; for a setting the id
//! passed is the declaration's `value_type_id`, never the setting key.
//!
//! The port lives in the domain, not in the SDK, because it is a dependency of
//! this gear's own services rather than a contract a consumer calls; the
//! binding over the types registry is in `infra`.

pub mod guards;

use async_trait::async_trait;
use serde_json::Value;

use crate::domain::error::DomainError;

/// One field-level fault a value was rejected for.
///
/// Tooling matches on `code`, never on `message`; `field` is a JSON pointer
/// below `value`, so a fault in a nested position names the position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldViolation {
    /// Where in the value the fault lies: `value` for the whole, `value/a/0`
    /// for a nested position.
    pub field: String,
    /// A stable code from [`crate::field`].
    pub code: &'static str,
    /// Human-readable detail.
    pub message: String,
}

/// The outcome of validating a value against a type: accepted, or every fault
/// that was found rather than only the first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationResult {
    /// Empty when the value was accepted.
    pub violations: Vec<FieldViolation>,
}

impl ValidationResult {
    /// A value that satisfied every rule.
    #[must_use]
    pub fn accepted() -> Self {
        Self::default()
    }

    /// Whether the value was accepted.
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        self.violations.is_empty()
    }

    /// The result as an error for callers that refuse on any fault.
    ///
    /// # Errors
    /// [`DomainError::Validation`] carrying the first fault when any exists;
    /// the full list stays on `self` for callers that report field-by-field.
    pub fn into_result(self) -> Result<(), DomainError> {
        match self.violations.into_iter().next() {
            None => Ok(()),
            Some(first) => Err(DomainError::Validation {
                field: first.field,
                code: first.code,
                message: first.message,
            }),
        }
    }
}

/// The resolved trait set of a value type, merged across its inheritance chain.
///
/// The keys are read from the type's `x-gts-traits`. The curated value-type
/// catalogue this vocabulary describes does not exist in the workspace yet;
/// these are the names this gear reads, recorded in DECOMPOSITION §1 as the
/// contract the catalogue must meet or this reader must follow.
// Three flags mirror three boolean traits of the vocabulary; a state enum would
// invent a relationship between `secret`, `multiline` and `regex` that the
// traits do not have.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraitSet {
    /// The value is a credential: stored by reference, masked on every
    /// administrative path, plaintext machine-only.
    pub secret: bool,
    /// Rendered as multi-line text.
    pub multiline: bool,
    /// The value is a cron expression in this dialect.
    pub cron_dialect: Option<String>,
    /// The value is a member of the enumeration this source supplies.
    pub dynamic_enum_source: Option<String>,
    /// The value is a GTS instance id of this type.
    pub entity_reference: Option<String>,
    /// The value is a regular expression that must compile.
    pub regex: bool,
    /// The merged `x-gts-traits` object as the registry returned it, for
    /// rendering metadata that names traits this gear does not interpret.
    pub raw: Value,
}

impl TraitSet {
    /// Read the interpreted traits off a merged `x-gts-traits` object.
    #[must_use]
    pub fn from_traits(raw: Value) -> Self {
        let flag = |name: &str| raw.get(name).and_then(Value::as_bool).unwrap_or(false);
        let text = |name: &str| raw.get(name).and_then(Value::as_str).map(ToOwned::to_owned);
        Self {
            secret: flag("secret"),
            multiline: flag("multiline"),
            cron_dialect: text("cron_dialect"),
            dynamic_enum_source: text("dynamic_enum_source"),
            entity_reference: text("entity_reference"),
            regex: flag("regex"),
            raw,
        }
    }
}

/// Validation of values against GTS types, and resolution of a type's traits.
#[async_trait]
pub trait TypeValidator: Send + Sync {
    /// Validate `value` against the type `value_type_id` names.
    ///
    /// Every rule is a hard check and every fault is collected. A type that
    /// cannot be resolved is a rejection, never an acceptance.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the registry cannot be reached;
    /// [`DomainError::Internal`] when the registry returns a schema that is
    /// not a valid JSON Schema.
    async fn validate_value(
        &self,
        value_type_id: &str,
        value: &Value,
    ) -> Result<ValidationResult, DomainError>;

    /// The trait set of the type `value_type_id` names.
    ///
    /// # Errors
    /// [`DomainError::Validation`] on `value_type_id` when the type is not
    /// registered — callers treat this as fail-closed, never as an empty set;
    /// [`DomainError::Unavailable`] when the registry cannot be reached.
    async fn resolve_traits(&self, value_type_id: &str) -> Result<TraitSet, DomainError>;
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod validation_tests;

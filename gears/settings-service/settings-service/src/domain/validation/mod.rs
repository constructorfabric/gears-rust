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

pub mod cron;
pub mod guards;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// One field-level fault a value was rejected for.
///
/// Tooling matches on `code`, never on `message`; `field` is a JSON pointer
/// below `value`, so a fault in a nested position names the position.
#[domain_model]
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

/// The outcome of validating a value against a type: accepted, or the faults
/// that were found — every fault of the rules that ran, not only the first.
///
/// Two checks end the run early, by design. The guards (the byte cap, the
/// canonical-number rule) refuse alone, before the type is even resolved: a
/// value that could not be cached or audited has no shape worth checking.
/// And a value past the leaf cap is answered with the faults found so far,
/// the leaf-bound trait rules not run.
#[domain_model]
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
/// The keys are read from the type's `x-gts-traits`, whether the type is one of
/// the catalogue or a module's own; these are the names this gear reads,
/// recorded in DECOMPOSITION §1 as the contract every value type's traits meet.
// Three flags mirror three boolean traits of the vocabulary; a state enum would
// invent a relationship between `secret`, `multiline` and `regex` that the
// traits do not have.
#[domain_model]
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

/// A trait that is present on a type but not of the type the vocabulary gives
/// it — `"secret": "true"` where a boolean is due.
///
/// Distinct from an absent trait, which reads as `false` or `None`. A type that
/// spells a trait wrongly must not be read as if it had left it out: `secret`
/// decides whether a value is stored in clear, and a misspelt `true` would
/// store a credential as public data.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedTrait {
    /// The trait's name in `x-gts-traits`, or the block's own name when the
    /// block itself has the wrong shape.
    pub name: &'static str,
    /// What the vocabulary expects there.
    pub expected: &'static str,
    /// The JSON type found instead.
    pub found: &'static str,
}

impl std::fmt::Display for MalformedTrait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "trait `{}` must be {}, found {}",
            self.name, self.expected, self.found
        )
    }
}

impl std::error::Error for MalformedTrait {}

/// The JSON type of a value, as a malformed trait names it.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

impl TraitSet {
    /// Check the trait blocks one level of a type declares, before they are
    /// merged. The registry's merge keeps an `x-gts-traits` object and the
    /// `properties` object of an `x-gts-traits-schema`, and drops anything
    /// else without a sign: `"x-gts-traits": ["secret"]` would reach
    /// [`Self::from_traits`] as no traits at all and classify a secret type as
    /// public. A block that is present is an object, or the type is malformed.
    ///
    /// # Errors
    /// [`MalformedTrait`] naming the block that is not an object.
    pub fn check_declared(
        traits: Option<&Value>,
        traits_schema: Option<&Value>,
    ) -> Result<(), MalformedTrait> {
        let not_an_object = |name: &'static str, value: &Value| MalformedTrait {
            name,
            expected: "an object",
            found: json_kind(value),
        };
        if let Some(traits) = traits
            && !traits.is_object()
        {
            return Err(not_an_object("x-gts-traits", traits));
        }
        if let Some(schema) = traits_schema {
            if !schema.is_object() {
                return Err(not_an_object("x-gts-traits-schema", schema));
            }
            if let Some(properties) = schema.get("properties")
                && !properties.is_object()
            {
                return Err(not_an_object("x-gts-traits-schema.properties", properties));
            }
        }
        Ok(())
    }

    /// Read the interpreted traits off a merged `x-gts-traits` object.
    ///
    /// # Errors
    /// [`MalformedTrait`] for a trait that is present but not of its type. An
    /// absent trait is `false` or `None`, never an error: an empty object is a
    /// real answer.
    pub fn from_traits(raw: Value) -> Result<Self, MalformedTrait> {
        let flag = |name: &'static str| match raw.get(name) {
            None => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(MalformedTrait {
                name,
                expected: "a boolean",
                found: json_kind(other),
            }),
        };
        let text = |name: &'static str| match raw.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(other) => Err(MalformedTrait {
                name,
                expected: "a string",
                found: json_kind(other),
            }),
        };
        Ok(Self {
            secret: flag("secret")?,
            multiline: flag("multiline")?,
            cron_dialect: text("cron_dialect")?,
            dynamic_enum_source: text("dynamic_enum_source")?,
            entity_reference: text("entity_reference")?,
            regex: flag("regex")?,
            raw,
        })
    }
}

/// The namespace this gear registers its own types under: the value-type
/// catalogue, the abstract base every setting key derives from — and so every
/// declared setting — and the gear's resource types.
fn gear_namespace() -> String {
    format!("{}cf.core.settings.", toolkit_gts::GTS_ID_PREFIX)
}

/// Whether `type_id` is one of this gear's own types that is not a value
/// type: under the gear's namespace, and outside the catalogue prefix
/// `type_`. The setting base and every setting key are such ids; a catalogue
/// type, a type derived from one, and any type of another namespace are not.
///
/// Decided on the identifier alone, so the answer needs no registry.
#[must_use]
pub(crate) fn is_gear_type_outside_catalogue(type_id: &str) -> bool {
    type_id.starts_with(&gear_namespace())
        && !type_id.starts_with(&settings_service_sdk::catalogue::value_type_prefix())
}

/// Why a named type cannot be the value type of a declaration.
///
/// A refusal is an answer about the type, never a failure of the lookup: the
/// port returns it beside its error, so a caller tells the three apart by
/// variant. Each carries the detail the caller is shown. On the REST surface
/// a refusal is a validation fault of `value_type_id` (the `From` below); the
/// contribution reconciler refuses the one item that named the type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueTypeRefusal {
    /// A type that describes no value: one of this gear's own types outside
    /// the catalogue (see `is_gear_type_outside_catalogue`), or a type whose
    /// own schema is marked `x-gts-abstract`.
    NotAValueType(String),
    /// No type is registered under the id.
    Unknown(String),
    /// The type is registered, but its schema does not build into a
    /// validator, so no value of it could be checked.
    Unsupported(String),
}

impl From<ValueTypeRefusal> for DomainError {
    fn from(refusal: ValueTypeRefusal) -> Self {
        let (code, message) = match refusal {
            ValueTypeRefusal::NotAValueType(message) => (crate::field::NOT_A_VALUE_TYPE, message),
            ValueTypeRefusal::Unknown(message) => (crate::field::VALUE_TYPE_UNKNOWN, message),
            ValueTypeRefusal::Unsupported(message) => {
                (crate::field::VALUE_TYPE_UNSUPPORTED, message)
            }
        };
        Self::Validation {
            field: "value_type_id".to_owned(),
            code,
            message,
        }
    }
}

/// Validation of values against GTS types, and resolution of a type's traits.
#[async_trait]
pub trait TypeValidator: Send + Sync {
    /// Whether `value_type_id` names a value type — a type a declaration may
    /// take the shape of its values from. Asked wherever a declaration is
    /// accepted, before its traits are resolved or its default is validated.
    ///
    /// A value type is a catalogue type, or one a module registered in its
    /// own namespace. `Ok(Err(_))` says why the named type is not one (see
    /// [`ValueTypeRefusal`]); `Ok(Ok(()))` that it is.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the registry cannot be reached;
    /// [`DomainError::Internal`] when the registry holds the type and cannot
    /// build it — a parent it no longer has, a stored schema it refuses.
    async fn ensure_value_type(
        &self,
        value_type_id: &str,
    ) -> Result<Result<(), ValueTypeRefusal>, DomainError>;

    /// Validate `value` against the type `value_type_id` names.
    ///
    /// Every rule is a hard check, and every fault of the rules that ran is
    /// collected; the guards refuse alone and first, and the leaf cap ends the
    /// rules with what was found (see [`ValidationResult`]). A type that
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

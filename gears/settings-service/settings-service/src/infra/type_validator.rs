// Created: 2026-09-06 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-component:p1
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-traits:p1
//! The Type Validator over the types registry.
//!
//! Generic over any GTS type id. For a setting the id passed in is the
//! declaration's `value_type_id` — the curated catalogue type its values
//! conform to — never the setting key, which is a type of its own that
//! describes nothing about the value's shape.
//!
//! # Two rules the registry does not know
//!
//! Every check here is **hard**. A `format` keyword is an annotation to a
//! JSON Schema validator by default; here it is asserted. A trait rule — a
//! regex that must compile, a reference that must resolve — is not a schema
//! keyword at all; here it rejects the value.
//!
//! # Failing closed
//!
//! Two of the trait rules name something outside the value: the dialect a cron
//! expression is written in, and the source a dynamic enumeration draws its
//! members from. When the gear cannot check what the trait names — a dialect it
//! does not implement, a source this deployment does not know — the value is
//! refused rather than admitted. An uncheckable rule is not an absent one, and
//! accepting silently is how a rule stops being a rule.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{GtsTypeSchema, TypesRegistryClient};

use crate::domain::error::DomainError;
use crate::domain::validation::{
    FieldViolation, MalformedTrait, TraitSet, TypeValidator, ValidationResult, cron, guards,
};
use crate::field;

/// Where the validator fetches types and resolves references.
///
/// A narrower seam than the whole registry client, so the validator's rules can
/// be exercised against a hand-built schema without standing up the registry.
#[async_trait]
pub trait SchemaSource: Send + Sync {
    /// The type schema, or `None` when no such type is registered.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the registry cannot be reached.
    async fn type_schema(&self, type_id: &str) -> Result<Option<GtsTypeSchema>, DomainError>;

    /// Which of `instance_ids` are registered GTS instances, each with the
    /// type id it is registered under, in one lookup.
    ///
    /// What both leaf-bound registry traits ask — an entity reference must
    /// resolve to an instance of its target type, a dynamic-enum member is an
    /// instance of its source — asked once per value over the distinct ids,
    /// never once per leaf and never by listing a source's members. The type
    /// comes back with the instance because the id's spelling is not the
    /// boundary: a type derived from the target spells the target as its
    /// prefix, and so does every instance of it.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the registry cannot be reached.
    async fn resolve_instances(
        &self,
        instance_ids: &[&str],
    ) -> Result<HashMap<String, String>, DomainError>;
}

fn unavailable(what: &str, err: &CanonicalError) -> DomainError {
    DomainError::dependency_unavailable("types registry", what, err)
}

#[async_trait]
impl SchemaSource for Arc<dyn TypesRegistryClient> {
    async fn type_schema(&self, type_id: &str) -> Result<Option<GtsTypeSchema>, DomainError> {
        match self.get_type_schema(type_id).await {
            Ok(schema) => Ok(Some(schema)),
            Err(CanonicalError::NotFound { .. }) => Ok(None),
            Err(err) => Err(unavailable("get_type_schema", &err)),
        }
    }

    async fn resolve_instances(
        &self,
        instance_ids: &[&str],
    ) -> Result<HashMap<String, String>, DomainError> {
        if instance_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<String> = instance_ids.iter().map(|id| (*id).to_owned()).collect();
        let mut registered = HashMap::new();
        for (id, outcome) in self.get_instances(ids).await {
            match outcome {
                Ok(instance) => {
                    registered.insert(id, instance.type_id().to_string());
                }
                // Absent, or not an instance id at all: the answer is "no",
                // not a failure of the lookup.
                Err(CanonicalError::NotFound { .. } | CanonicalError::InvalidArgument { .. }) => {}
                Err(err) => return Err(unavailable("get_instances", &err)),
            }
        }
        Ok(registered)
    }
}

/// [`TypeValidator`] over a [`SchemaSource`] — in production, the registry.
pub struct GtsTypeValidator<S = Arc<dyn TypesRegistryClient>> {
    source: S,
}

/// The most string leaves a trait-checked value may hold. The byte cap bounds
/// a value's size; this bounds the work a leaf-bound trait does on it — a
/// parse or a compile per leaf, and a registry lookup per distinct id — so a
/// value of thousands of two-byte strings is refused, not processed.
pub const MAX_TRAIT_LEAVES: usize = 1_000;

impl<S: SchemaSource> GtsTypeValidator<S> {
    /// Validate against the types this source resolves.
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// The source, for a test that counts what it was asked.
    #[cfg(test)]
    pub const fn source(&self) -> &S {
        &self.source
    }

    /// Resolve a type, failing closed on an unknown id.
    ///
    /// An unknown type is a fault of the *declaration* that named it, reported
    /// on `value_type_id`; an unreachable registry is unavailability. Neither is
    /// ever an acceptance.
    async fn resolve(&self, value_type_id: &str) -> Result<GtsTypeSchema, DomainError> {
        self.source
            .type_schema(value_type_id)
            .await?
            .ok_or_else(|| DomainError::Validation {
                field: "value_type_id".to_owned(),
                code: field::VALUE_TYPE_UNKNOWN,
                message: format!("`{value_type_id}` is not a registered GTS type"),
            })
    }
}

/// The violation a type whose `x-gts-traits` is not well-formed produces.
///
/// Reported on `value_type_id`, as an unknown type is: the fault is the type's,
/// and the declaration is what named it.
fn malformed_trait(value_type_id: &str, malformed: &MalformedTrait) -> FieldViolation {
    FieldViolation {
        field: "value_type_id".to_owned(),
        code: field::VALUE_TYPE_MALFORMED,
        message: format!(
            "`{value_type_id}` carries a malformed trait (catalogue drift): {malformed}"
        ),
    }
}

#[async_trait]
impl<S: SchemaSource> TypeValidator for GtsTypeValidator<S> {
    async fn validate_value(
        &self,
        value_type_id: &str,
        value: &Value,
    ) -> Result<ValidationResult, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-3
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-4
        // The guards first, before anything is resolved: a value over the cap
        // or carrying a non-canonical number is refused whatever shape it has,
        // and asking the registry about it would only cost a round trip.
        if let Err(violation) = guards::check(value) {
            return Ok(ValidationResult {
                violations: vec![violation],
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-4
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-3
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-1
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-2
        // An unresolvable type is a rejection, not a vacuous pass: a value nobody
        // could check is not a value anybody has accepted.
        let schema = match self.resolve(value_type_id).await {
            Ok(schema) => schema,
            Err(DomainError::Validation {
                field,
                code,
                message,
            }) => {
                return Ok(ValidationResult {
                    violations: vec![FieldViolation {
                        field,
                        code,
                        message,
                    }],
                });
            }
            Err(other) => return Err(other),
        };
        // A trait spelled wrongly is the type's fault, reported like an unknown
        // type: a value nobody can classify is not a value anybody has accepted.
        let traits = match TraitSet::from_traits(schema.effective_traits()) {
            Ok(traits) => traits,
            Err(malformed) => {
                return Ok(ValidationResult {
                    violations: vec![malformed_trait(value_type_id, &malformed)],
                });
            }
        };
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-2
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-1

        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-12
        let mut violations = Vec::new();

        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-5
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-6
        // The effective schema has the parent chain inlined. `format` keywords
        // are asserted, not annotated: `uri`, `ipv4` and their kind reject a
        // value that does not match, exactly as `type` or `maximum` would.
        let effective = schema.effective_schema();
        let validator = jsonschema::options()
            .should_validate_formats(true)
            .build(&effective)
            .map_err(|e| DomainError::Internal {
                diagnostic: format!(
                    "GTS type `{value_type_id}` is not a valid JSON Schema (catalogue drift): {e}"
                ),
            })?;
        for error in validator.iter_errors(value) {
            let code = match error.kind() {
                jsonschema::error::ValidationErrorKind::Format { .. } => field::VALUE_FORMAT,
                _ => field::VALUE_SCHEMA,
            };
            violations.push(FieldViolation {
                field: format!("value{}", error.instance_path()),
                code,
                message: error.to_string(),
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-6
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-5

        // @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-rules:p1
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-7
        // Trait rules apply to the string leaves a trait describes, collected
        // once and only when a trait asks for them. Their count is bounded as
        // the bytes are: each leaf costs a parse or a compile, and each distinct
        // id a registry lookup, so a value of thousands of two-byte strings is
        // refused rather than processed.
        let leaf_bound = traits.cron_dialect.is_some()
            || traits.regex
            || traits.dynamic_enum_source.is_some()
            || traits.entity_reference.is_some();
        let leaves = if leaf_bound {
            string_leaves(value, "value")
        } else {
            Vec::new()
        };
        if leaves.len() > MAX_TRAIT_LEAVES {
            violations.push(FieldViolation {
                field: "value".to_owned(),
                code: field::VALUE_TOO_MANY_LEAVES,
                message: format!(
                    "a value of a trait-checked type holds at most {MAX_TRAIT_LEAVES} strings; this \
                     one holds {}",
                    leaves.len()
                ),
            });
            return Ok(ValidationResult { violations });
        }
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-8
        if let Some(dialect) = traits.cron_dialect.as_deref() {
            for (path, text) in &leaves {
                if cron::is_known(dialect) {
                    if let Err(reason) = cron::parse(text) {
                        violations.push(FieldViolation {
                            field: path.clone(),
                            code: field::VALUE_CRON_INVALID,
                            message: format!("not a cron expression: {reason}"),
                        });
                    }
                } else {
                    violations.push(FieldViolation {
                        field: path.clone(),
                        code: field::VALUE_CRON_DIALECT_UNKNOWN,
                        message: format!(
                            "the type declares the cron dialect `{dialect}`, which this service \
                             cannot check; the value is refused rather than admitted unchecked"
                        ),
                    });
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-8
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-9
        if traits.regex {
            for (path, text) in &leaves {
                if let Err(e) = regex::Regex::new(text) {
                    violations.push(FieldViolation {
                        field: path.clone(),
                        code: field::VALUE_REGEX_INVALID,
                        message: format!("regular expression does not compile: {e}"),
                    });
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-9
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-10
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-11
        // Dynamic-enum membership and entity references both ask the registry
        // whether an id is a registered instance: asked once, for every distinct
        // id the value carries, whatever the number of leaves. A member of a
        // dynamic enumeration is a registered instance derived from its source
        // — its own id is looked up, the members are never listed. A source
        // this deployment does not know refuses every leaf: an unknown
        // membership is not an empty rule.
        let enum_source = match traits.dynamic_enum_source.as_deref() {
            Some(source) if self.source.type_schema(source).await?.is_some() => Some(source),
            Some(source) => {
                for (path, _) in &leaves {
                    violations.push(FieldViolation {
                        field: path.clone(),
                        code: field::VALUE_ENUM_SOURCE_UNKNOWN,
                        message: format!(
                            "the type draws its members from `{source}`, which this deployment \
                             does not know; the value is refused rather than admitted unchecked"
                        ),
                    });
                }
                None
            }
            None => None,
        };
        // Each registered id with the type it is registered under: membership
        // and reference are decided on that type, never on the id's spelling.
        let registered = if enum_source.is_some() || traits.entity_reference.is_some() {
            let mut ids: Vec<&str> = leaves.iter().map(|(_, text)| *text).collect();
            ids.sort_unstable();
            ids.dedup();
            self.source.resolve_instances(&ids).await?
        } else {
            HashMap::new()
        };
        if let Some(source) = enum_source {
            for (path, text) in &leaves {
                if registered.get(*text).is_none_or(|of| of != source) {
                    violations.push(FieldViolation {
                        field: path.clone(),
                        code: field::VALUE_NOT_IN_ENUM,
                        message: format!("`{text}` is not a registered member of `{source}`"),
                    });
                }
            }
        }
        if let Some(target_type) = traits.entity_reference.as_deref() {
            for (path, id) in &leaves {
                if registered.get(*id).is_none_or(|of| of != target_type) {
                    violations.push(FieldViolation {
                        field: path.clone(),
                        code: field::VALUE_REFERENCE_UNRESOLVED,
                        message: format!(
                            "`{id}` does not resolve to a registered instance of `{target_type}`"
                        ),
                    });
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-11
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-10
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-7
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-12

        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-13
        Ok(ValidationResult { violations })
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-13
    }

    async fn resolve_traits(&self, value_type_id: &str) -> Result<TraitSet, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-1
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-2
        // A type that cannot be resolved is an error, never an empty set: an
        // empty set would classify a secret-trait type as public.
        let schema = self.resolve(value_type_id).await?;
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-2
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-1
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-3
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-4
        // Merged across the inheritance chain, so a trait declared on a base
        // reaches every derived type; the raw object travels with the
        // interpreted flags for rendering.
        // A misspelt trait fails here, never defaults: the caller of this port
        // classifies a declaration on `secret`, and an empty or guessed answer
        // would store a credential as public data.
        TraitSet::from_traits(schema.effective_traits()).map_err(|malformed| {
            let violation = malformed_trait(value_type_id, &malformed);
            DomainError::Validation {
                field: violation.field,
                code: violation.code,
                message: violation.message,
            }
        })
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-4
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-resolve-traits:p1:inst-tvv-traits-3
    }
}

/// Every string in a value with its JSON-pointer position below `root`.
fn string_leaves<'v>(value: &'v Value, root: &str) -> Vec<(String, &'v str)> {
    let mut out = Vec::new();
    collect_strings(value, root, &mut out);
    out
}

fn collect_strings<'v>(value: &'v Value, path: &str, out: &mut Vec<(String, &'v str)>) {
    match value {
        Value::String(s) => out.push((path.to_owned(), s.as_str())),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                collect_strings(item, &format!("{path}/{i}"), out);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                collect_strings(v, &format!("{path}/{k}"), out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
#[path = "type_validator_tests.rs"]
mod type_validator_tests;

// Created: 2026-09-06 by Constructor Tech
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

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{GtsTypeSchema, TypesRegistryClient};

use crate::domain::error::DomainError;
use crate::domain::validation::{
    FieldViolation, TraitSet, TypeValidator, ValidationResult, cron, guards,
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

    /// Whether a GTS instance with this id is registered.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the registry cannot be reached.
    async fn instance_exists(&self, instance_id: &str) -> Result<bool, DomainError>;

    /// The members of a dynamic enumeration, or `None` when the source is not
    /// one this deployment knows.
    ///
    /// A source that does not resolve fails the value closed rather than
    /// admitting it: an unknown membership is not an empty rule.
    ///
    /// # Errors
    /// [`DomainError`] when the source cannot be consulted at all.
    async fn enum_members(&self, source: &str) -> Result<Option<Vec<String>>, DomainError>;
}

/// A few members, for a message a reader can act on without printing a
/// thousand of them.
fn summarize(members: &[String]) -> String {
    const SHOWN: usize = 8;
    if members.is_empty() {
        return "nothing".to_owned();
    }
    let head = members
        .iter()
        .take(SHOWN)
        .map(|m| format!("`{m}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if members.len() > SHOWN {
        format!("{head} and {} more", members.len() - SHOWN)
    } else {
        head
    }
}

fn unavailable(what: &str, err: &CanonicalError) -> DomainError {
    DomainError::Unavailable {
        detail: format!("types registry: {what}: {err}"),
    }
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

    async fn instance_exists(&self, instance_id: &str) -> Result<bool, DomainError> {
        match self.get_instance(instance_id).await {
            Ok(_) => Ok(true),
            Err(CanonicalError::NotFound { .. }) => Ok(false),
            Err(err) => Err(unavailable("get_instance", &err)),
        }
    }

    async fn enum_members(&self, source: &str) -> Result<Option<Vec<String>>, DomainError> {
        // A dynamic enumeration names a GTS type whose registered instances are
        // its members, which is the one membership the registry can answer.
        // Anything else a deployment might mean by a source is a binding this
        // release does not have, and the caller refuses the value for it.
        // The source names a GTS type; its registered instances are the
        // members. The pattern is the source itself, so the query returns the
        // instances derived from it and nothing else.
        match self.get_type_schema(source).await {
            Ok(_) => {}
            Err(CanonicalError::NotFound { .. }) => return Ok(None),
            Err(err) => return Err(unavailable("get_type_schema", &err)),
        }
        let query = types_registry_sdk::InstanceQuery::new().with_pattern(source);
        match self.list_instances(query).await {
            Ok(instances) => Ok(Some(
                instances
                    .into_iter()
                    .map(|instance| instance.id.to_string())
                    .collect(),
            )),
            Err(err) => Err(unavailable("list_instances", &err)),
        }
    }
}

/// [`TypeValidator`] over a [`SchemaSource`] — in production, the registry.
pub struct GtsTypeValidator<S = Arc<dyn TypesRegistryClient>> {
    source: S,
}

impl<S: SchemaSource> GtsTypeValidator<S> {
    /// Validate against the types this source resolves.
    pub fn new(source: S) -> Self {
        Self { source }
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

#[async_trait]
impl<S: SchemaSource> TypeValidator for GtsTypeValidator<S> {
    async fn validate_value(
        &self,
        value_type_id: &str,
        value: &Value,
    ) -> Result<ValidationResult, DomainError> {
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
        let traits = TraitSet::from_traits(schema.effective_traits());
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-2
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-1

        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-3
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-4
        // The guards run before the schema and stop on the first fault: a value
        // over the cap or carrying a non-canonical number is refused whatever
        // shape it has, and validating it structurally would only cost time.
        if let Err(violation) = guards::check(value) {
            return Ok(ValidationResult {
                violations: vec![violation],
            });
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-4
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-3

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
        // Trait rules apply to the string leaves a trait describes. A structured
        // value carrying such a trait on the whole is checked leaf by leaf.
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-8
        if let Some(dialect) = traits.cron_dialect.as_deref() {
            for (path, text) in string_leaves(value, "value") {
                if cron::is_known(dialect) {
                    if let Err(reason) = cron::parse(text) {
                        violations.push(FieldViolation {
                            field: path,
                            code: field::VALUE_CRON_INVALID,
                            message: format!("not a cron expression: {reason}"),
                        });
                    }
                } else {
                    violations.push(FieldViolation {
                        field: path,
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
            for (path, text) in string_leaves(value, "value") {
                if let Err(e) = regex::Regex::new(text) {
                    violations.push(FieldViolation {
                        field: path,
                        code: field::VALUE_REGEX_INVALID,
                        message: format!("regular expression does not compile: {e}"),
                    });
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-9
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-10
        if let Some(source) = traits.dynamic_enum_source.as_deref() {
            let members = self.source.enum_members(source).await?;
            for (path, text) in string_leaves(value, "value") {
                match &members {
                    Some(members) if members.iter().any(|m| m == text) => {}
                    Some(members) => violations.push(FieldViolation {
                        field: path,
                        code: field::VALUE_NOT_IN_ENUM,
                        message: format!(
                            "`{text}` is not a member of `{source}`, which offers {}",
                            summarize(members)
                        ),
                    }),
                    None => violations.push(FieldViolation {
                        field: path,
                        code: field::VALUE_ENUM_SOURCE_UNKNOWN,
                        message: format!(
                            "the type draws its members from `{source}`, which this deployment \
                             does not know; the value is refused rather than admitted unchecked"
                        ),
                    }),
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-10
        // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-11
        if let Some(target_type) = traits.entity_reference.as_deref() {
            for (path, id) in string_leaves(value, "value") {
                let of_type = id.starts_with(target_type);
                let resolves = of_type && self.source.instance_exists(id).await?;
                if !resolves {
                    violations.push(FieldViolation {
                        field: path,
                        code: field::VALUE_REFERENCE_UNRESOLVED,
                        message: format!(
                            "`{id}` does not resolve to a registered instance of `{target_type}`"
                        ),
                    });
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-validate:p1:inst-tvv-val-11
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
        Ok(TraitSet::from_traits(schema.effective_traits()))
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

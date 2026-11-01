//! Type ids: comparison keys and write-time resolution through types-registry
//! (DESIGN §3.1, Type comparison; `fr-type-references`).

use std::collections::HashMap;
use std::sync::Arc;

use gts::{GTS_ID_PREFIX, GtsId, GtsIdPattern, GtsSchema, GtsTypeId};
use license_enforcement_sdk::RecipientProjectionV1;
use license_enforcement_sdk::error::reason;
use license_enforcement_sdk::models::GrantTerms;
use license_resolver_sdk::{LicenseResourceV1, LicenseSubjectV1};
use serde_json::Value;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::{CanonicalError, FieldViolation};
use toolkit_macros::domain_model;
use types_registry_sdk::{GtsTypeSchema, TypesRegistryClient};

use super::error::DomainError;

const ADMITTED_SUBJECTS_TRAIT: &str = "admitted_subjects";

/// The type id without minor versions. Two ids are the same type when their
/// keys are equal; a key that starts with another key derives from it. An id
/// that does not parse is its own key.
#[must_use]
pub fn type_key(type_id: &str) -> String {
    let Ok(id) = GtsId::try_new(type_id) else {
        return type_id.to_owned();
    };
    let mut key = GTS_ID_PREFIX.to_owned();
    for s in id.segments() {
        let raw = s.raw();
        let Some(minor) = s.ver_minor() else {
            key.push_str(raw);
            continue;
        };
        let (body, tilde) = raw.strip_suffix('~').map_or((raw, ""), |b| (b, "~"));
        key.push_str(body.strip_suffix(&format!(".{minor}")).unwrap_or(body));
        key.push_str(tilde);
    }
    key
}

/// `subject` is `base` or derives from it, without minor versions. An id that
/// does not parse covers nothing and is covered by nothing.
#[must_use]
pub fn covers(base: &GtsTypeId, subject: &GtsTypeId) -> bool {
    let parses = |t: &GtsTypeId| GtsId::try_new(t.as_ref()).is_ok();
    parses(base)
        && parses(subject)
        && type_key(subject.as_ref()).starts_with(&type_key(base.as_ref()))
}

/// Schemas resolved during one request, so that a pack of a thousand grants on
/// the same types asks types-registry once per type.
pub type Schemas = HashMap<String, Option<GtsTypeSchema>>;

/// Schema of the object that conditions address: the subject and the resource
/// of a check, each by its contract. `None` when a type did not resolve; its
/// violation is already reported.
#[must_use]
pub fn conditions_schema(terms: &GrantTerms, seen: &Schemas) -> Option<Value> {
    let resolved = |t: &GtsTypeId| seen.get(t.as_ref()).cloned().flatten();
    let subject = resolved(&terms.subject_type)?;
    let resource = resolved(&terms.resource.gts_type)?;
    Some(serde_json::json!({
        "type": "object",
        "properties": {
            "subject": subject.effective_schema(),
            "resource": resource.effective_schema(),
        },
    }))
}

/// Schema of the recipient projection, which recipient rules address.
#[must_use]
pub fn recipient_schema() -> Value {
    <RecipientProjectionV1 as GtsSchema>::gts_schema_with_refs()
}

/// Resolves type ids through types-registry at write time.
#[domain_model]
pub struct TypeResolver {
    hub: Arc<ClientHub>,
}

impl TypeResolver {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }

    async fn schema(
        &self,
        type_id: &str,
        seen: &mut Schemas,
    ) -> Result<Option<GtsTypeSchema>, DomainError> {
        if let Some(schema) = seen.get(type_id) {
            return Ok(schema.clone());
        }
        let schema = self.fetch(type_id).await?;
        seen.insert(type_id.to_owned(), schema.clone());
        Ok(schema)
    }

    async fn fetch(&self, type_id: &str) -> Result<Option<GtsTypeSchema>, DomainError> {
        // Resolved per call: types-registry commits its catalog after the
        // system-gear init phase.
        let registry = self
            .hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| DomainError::DependencyUnavailable(format!("types-registry: {e}")))?;
        match registry.get_type_schema(type_id).await {
            Ok(schema) => Ok(Some(schema)),
            Err(CanonicalError::NotFound { .. } | CanonicalError::InvalidArgument { .. }) => {
                Ok(None)
            }
            Err(e) => Err(DomainError::DependencyUnavailable(format!(
                "types-registry: {e}"
            ))),
        }
    }

    /// Fails start when a type that the evaluator names is not registered.
    ///
    /// # Errors
    ///
    /// The first missing type, or a registry fault.
    pub async fn ensure_registered(&self, types: &[GtsTypeId]) -> Result<(), DomainError> {
        for t in types {
            if self.fetch(t.as_ref()).await?.is_none() {
                return Err(DomainError::Internal(format!(
                    "type '{t}' named by the evaluator is not registered"
                )));
            }
        }
        Ok(())
    }

    /// Violations of the type ids of root terms; `field` prefixes each path.
    ///
    /// # Errors
    ///
    /// A registry fault.
    pub async fn validate_terms(
        &self,
        field: &str,
        terms: &GrantTerms,
        violations: &mut Vec<FieldViolation>,
        seen: &mut Schemas,
    ) -> Result<(), DomainError> {
        let resource_field = format!("{field}.resource.type");
        let subject_field = format!("{field}.subject_type");
        let resource = self
            .derived(
                &resource_field,
                terms.resource.gts_type.as_ref(),
                <LicenseResourceV1<()> as GtsSchema>::TYPE_ID,
                violations,
                seen,
            )
            .await?;
        let subject = self
            .derived(
                &subject_field,
                terms.subject_type.as_ref(),
                <LicenseSubjectV1<()> as GtsSchema>::TYPE_ID,
                violations,
                seen,
            )
            .await?;
        if let (Some(resource), true) = (resource, subject.is_some()) {
            let traits = resource.effective_traits();
            // The match of license-resolver, so that a grant it accepts here
            // passes the check there.
            let admitted = matches!(
                (traits.get(ADMITTED_SUBJECTS_TRAIT), GtsId::try_new(terms.subject_type.as_ref())),
                (Some(Value::Array(items)), Ok(subject))
                    if items.iter().filter_map(Value::as_str).any(|a| {
                        GtsIdPattern::try_new(a).is_ok_and(|p| subject.matches_pattern(&p))
                    })
            );
            if !admitted {
                violations.push(FieldViolation::new(
                    subject_field,
                    format!(
                        "'{}' is not in admitted_subjects of '{}'",
                        terms.subject_type, terms.resource.gts_type
                    ),
                    reason::SUBJECT_NOT_ADMITTED,
                ));
            }
        }
        Ok(())
    }

    /// Violations of the type of a subject that a grant names: it is registered,
    /// and it is `subject_type` or derives from it.
    ///
    /// # Errors
    ///
    /// A registry fault.
    pub async fn validate_subject_of(
        &self,
        field: &str,
        subject_type: &GtsTypeId,
        named: &GtsTypeId,
        violations: &mut Vec<FieldViolation>,
        seen: &mut Schemas,
    ) -> Result<(), DomainError> {
        if self.schema(named.as_ref(), seen).await?.is_none() {
            violations.push(FieldViolation::new(
                field,
                format!("type '{named}' is not registered"),
                reason::TYPE_NOT_REGISTERED,
            ));
        } else if !covers(subject_type, named) {
            violations.push(FieldViolation::new(
                field,
                "the subject type is not the subject type of the grant or derived from it",
                reason::TYPE_NOT_DERIVED,
            ));
        }
        Ok(())
    }

    async fn derived(
        &self,
        field: &str,
        type_id: &str,
        base: &str,
        violations: &mut Vec<FieldViolation>,
        seen: &mut Schemas,
    ) -> Result<Option<GtsTypeSchema>, DomainError> {
        let Some(schema) = self.schema(type_id, seen).await? else {
            violations.push(FieldViolation::new(
                field,
                format!("type '{type_id}' is not registered"),
                reason::TYPE_NOT_REGISTERED,
            ));
            return Ok(None);
        };
        // `ancestors` yields the type itself first; a base is not its own derivation.
        if !schema
            .ancestors()
            .skip(1)
            .any(|a| a.type_id.as_ref() == base)
        {
            violations.push(FieldViolation::new(
                field,
                format!("type '{type_id}' does not derive from '{base}'"),
                reason::TYPE_NOT_DERIVED,
            ));
            return Ok(None);
        }
        Ok(Some(schema))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "types_tests.rs"]
mod types_tests;

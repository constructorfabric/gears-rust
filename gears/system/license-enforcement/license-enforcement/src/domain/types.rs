//! Type ids: comparison keys and write-time resolution through types-registry
//! (DESIGN §3.1, Type comparison; `fr-type-references`).

use std::collections::HashMap;
use std::sync::Arc;

use gts::schema_modifiers::X_GTS_ABSTRACT;
use gts::{GTS_ID_PREFIX, GtsId, GtsIdPattern, GtsSchema, GtsTypeId};
use license_enforcement_sdk::BindingFilter;
use license_enforcement_sdk::error::reason;
use license_enforcement_sdk::models::{GrantTerms, SubjectRef};
use license_resolver_sdk::{LicenseResourceV1, LicenseSubjectV1, Resource, Subject, field};
use serde_json::Value;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::{CanonicalError, FieldViolation};
use toolkit_macros::domain_model;
use toolkit_odata::filter::{FieldKind, FilterField};
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    EntityField, FieldSelection, LifecycleStatus, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, ReadOptions, TypeSchema, TypesRegistryApi,
    TypesRegistryApiExt,
};

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

/// `f` matches `subject`: the same id, and the type key equal to the one of
/// the pattern, or starting with it when `f` is not exact. The store selects
/// candidates by the same rule on the key columns.
#[must_use]
pub fn filter_matches(f: &BindingFilter, subject: &SubjectRef) -> bool {
    let (want, have) = (
        type_key(f.type_pattern.as_ref()),
        type_key(subject.gts_type.as_ref()),
    );
    f.binding_id == subject.id
        && if f.exact {
            have == want
        } else {
            have.starts_with(&want)
        }
}

/// `resource` admits `subject_type`, as license-resolver matches it, so that
/// a grant it accepts here passes the check there.
///
/// # Errors
///
/// `Internal` when the trait is not a list of strings: the registered contract
/// is unusable, as license-resolver reports it.
fn admits(resource: &TypeSchema, subject_type: &GtsTypeId) -> Result<bool, DomainError> {
    let unusable = |why: &str| {
        DomainError::Internal(format!(
            "registered contract '{}' is unusable: trait `{ADMITTED_SUBJECTS_TRAIT}` {why}",
            resource.type_id
        ))
    };
    let items = match resource
        .effective_traits
        .as_ref()
        .and_then(|traits| traits.get(ADMITTED_SUBJECTS_TRAIT))
    {
        None | Some(Value::Null) => return Ok(false),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(unusable("is not an array")),
    };
    let mut admitted = Vec::with_capacity(items.len());
    for item in items {
        admitted.push(
            item.as_str()
                .ok_or_else(|| unusable("contains a non-string entry"))?,
        );
    }
    Ok(GtsId::try_new(subject_type.as_ref()).is_ok_and(|subject| {
        admitted
            .iter()
            .any(|a| GtsIdPattern::try_new(a).is_ok_and(|p| subject.matches_pattern(&p)))
    }))
}

/// Violations of `object` against `contract`, under the field root `root`.
///
/// # Errors
///
/// `Internal` when the schema does not compile or the object does not
/// serialize.
fn conforms<T: serde::Serialize>(
    root: &str,
    contract: &TypeSchema,
    object: &T,
    violations: &mut Vec<FieldViolation>,
) -> Result<(), DomainError> {
    let id = &contract.type_id;
    let schema = contract.resolved_schema.as_ref().ok_or_else(|| {
        DomainError::Internal(format!("registered contract '{id}' has no resolved schema"))
    })?;
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .build(schema)
        .map_err(|e| {
            DomainError::Internal(format!(
                "registered contract '{id}' is unusable: not a valid JSON Schema: {e}"
            ))
        })?;
    let payload = serde_json::to_value(object)
        .map_err(|e| DomainError::Internal(format!("cannot serialize the {root} object: {e}")))?;
    for e in validator
        .iter_errors(&payload)
        .take(MAX_REPORTED_VIOLATIONS)
    {
        violations.push(FieldViolation::new(
            format!("{root}{}", e.instance_path()),
            format!("{id}: {e}"),
            field::SCHEMA_MISMATCH,
        ));
    }
    Ok(())
}

fn is_abstract(schema: &TypeSchema) -> bool {
    schema
        .content
        .as_ref()
        .and_then(|content| content.get(X_GTS_ABSTRACT))
        == Some(&Value::Bool(true))
}

/// The chain without the type itself; a base is not its own derivation.
fn derives_from(type_id: &GtsTypeId, base: &str) -> bool {
    GtsId::try_new(type_id.as_ref()).is_ok_and(|id| {
        let mut chain = id.chain_ids();
        chain.pop();
        chain.iter().any(|ancestor| ancestor == base)
    })
}

/// What types-registry answers for a type id.
enum Lookup {
    Found(Box<TypeSchema>),
    Unregistered,
    Malformed(String),
}

/// Most violations reported for one object of a check-explain request.
const MAX_REPORTED_VIOLATIONS: usize = 20;

/// Schemas resolved during one request, so that a pack of a thousand grants on
/// the same types asks types-registry once per type.
pub type Schemas = HashMap<String, Option<TypeSchema>>;

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
            "subject": subject.resolved_schema,
            "resource": resource.resolved_schema,
        },
    }))
}

/// Whose read of the types registry a lookup is: the gear's own at start, or a
/// request's under the caller's context.
#[derive(Clone, Copy)]
enum Caller<'a> {
    Platform,
    Request(&'a SecurityContext),
}

/// Resolves type ids through types-registry at write time: a request's
/// lookups through `TypesRegistryApi` under the caller's context, the start
/// check through `PlatformTypesRegistryApi`. Both clients are consumed
/// contracts, resolved from the hub per call.
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
        ctx: &SecurityContext,
        type_id: &str,
        seen: &mut Schemas,
    ) -> Result<Option<TypeSchema>, DomainError> {
        if let Some(schema) = seen.get(type_id) {
            return Ok(schema.clone());
        }
        let schema = self.fetch(Caller::Request(ctx), type_id).await?;
        seen.insert(type_id.to_owned(), schema.clone());
        Ok(schema)
    }

    async fn fetch(
        &self,
        caller: Caller<'_>,
        type_id: &str,
    ) -> Result<Option<TypeSchema>, DomainError> {
        Ok(match self.lookup(caller, type_id).await? {
            Lookup::Found(schema) => Some(*schema),
            Lookup::Unregistered | Lookup::Malformed(_) => None,
        })
    }

    async fn lookup(&self, caller: Caller<'_>, type_id: &str) -> Result<Lookup, DomainError> {
        let unavailable = |e: &dyn std::fmt::Display| {
            DomainError::DependencyUnavailable(format!("types-registry: {e}"))
        };
        let id = match GtsTypeId::try_new(type_id) {
            Ok(id) => id,
            Err(e) => return Ok(Lookup::Malformed(e.to_string())),
        };
        let options =
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::ResolvedSchema,
                EntityField::EffectiveTraits,
            ])));
        let read = match caller {
            Caller::Platform => {
                let registry = self
                    .hub
                    .get::<dyn PlatformTypesRegistryApi>()
                    .map_err(|e| unavailable(&e))?;
                registry
                    .get_type_schema(&PlatformSecurityContext::outbound_marker(), &id, options)
                    .await
            }
            Caller::Request(ctx) => {
                let registry = self
                    .hub
                    .get::<dyn TypesRegistryApi>()
                    .map_err(|e| unavailable(&e))?;
                registry.get_type_schema(ctx, &id, options).await
            }
        };
        match read {
            Ok(schema) => found(type_id, schema),
            Err(CanonicalError::NotFound { .. }) => Ok(Lookup::Unregistered),
            Err(CanonicalError::InvalidArgument { detail, .. }) => Ok(Lookup::Malformed(detail)),
            // The registry refused a request's caller: its refusal, not an outage.
            Err(CanonicalError::Unauthenticated { .. }) if matches!(caller, Caller::Request(_)) => {
                Err(DomainError::Unauthenticated)
            }
            Err(CanonicalError::PermissionDenied { .. })
                if matches!(caller, Caller::Request(_)) =>
            {
                Err(DomainError::PermissionDenied)
            }
            Err(e) => Err(unavailable(&e)),
        }
    }

    /// Fails start when a type that the evaluator names is not registered.
    ///
    /// # Errors
    ///
    /// The first missing type, or a registry fault.
    pub async fn ensure_registered(&self, types: &[GtsTypeId]) -> Result<(), DomainError> {
        for t in types {
            if self.fetch(Caller::Platform, t.as_ref()).await?.is_none() {
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
        ctx: &SecurityContext,
        field: &str,
        terms: &GrantTerms,
        violations: &mut Vec<FieldViolation>,
        seen: &mut Schemas,
    ) -> Result<(), DomainError> {
        let resource_field = format!("{field}.resource.type");
        let subject_field = format!("{field}.subject_type");
        let resource = self
            .derived(
                ctx,
                &resource_field,
                terms.resource.gts_type.as_ref(),
                <LicenseResourceV1<()> as GtsSchema>::TYPE_ID,
                violations,
                seen,
            )
            .await?;
        let subject = self
            .derived(
                ctx,
                &subject_field,
                terms.subject_type.as_ref(),
                <LicenseSubjectV1<()> as GtsSchema>::TYPE_ID,
                violations,
                seen,
            )
            .await?;
        // A check names a concrete resource type and matches it exactly.
        let resource = match resource {
            Some(r) if is_abstract(&r) => {
                violations.push(FieldViolation::new(
                    &resource_field,
                    format!("'{}' is abstract", terms.resource.gts_type),
                    reason::TYPE_ABSTRACT,
                ));
                None
            }
            other => other,
        };
        if let (Some(resource), true) = (resource, subject.is_some())
            && !admits(&resource, &terms.subject_type)?
        {
            violations.push(FieldViolation::new(
                subject_field,
                format!(
                    "'{}' is not in admitted_subjects of '{}'",
                    terms.subject_type, terms.resource.gts_type
                ),
                reason::SUBJECT_NOT_ADMITTED,
            ));
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
        ctx: &SecurityContext,
        field: &str,
        subject_type: &GtsTypeId,
        named: &GtsTypeId,
        violations: &mut Vec<FieldViolation>,
        seen: &mut Schemas,
    ) -> Result<(), DomainError> {
        if self.schema(ctx, named.as_ref(), seen).await?.is_none() {
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

    /// Violations of a check-explain request against its contracts, with the
    /// fields, reasons, and error classes of the gateway of license-resolver:
    /// each type is registered, well formed, not abstract, and derived; each
    /// object conforms to its schema; the resource admits the subject type.
    ///
    /// # Errors
    ///
    /// A registry fault; `Internal` for a registered contract that is unusable
    /// or an object that does not serialize.
    pub async fn validate_check(
        &self,
        ctx: &SecurityContext,
        subject: &Subject,
        resource: &Resource,
        violations: &mut Vec<FieldViolation>,
    ) -> Result<(), DomainError> {
        let resource_contract = self
            .contract(
                ctx,
                field::RESOURCE_TYPE_FIELD,
                resource.gts_type.as_ref(),
                <LicenseResourceV1<()> as GtsSchema>::TYPE_ID,
                violations,
            )
            .await?;
        let subject_contract = self
            .contract(
                ctx,
                field::SUBJECT_TYPE_FIELD,
                subject.gts_type.as_ref(),
                <LicenseSubjectV1<()> as GtsSchema>::TYPE_ID,
                violations,
            )
            .await?;
        if let Some(contract) = &resource_contract {
            conforms(field::RESOURCE_FIELD, contract, resource, violations)?;
        }
        if let Some(contract) = &subject_contract {
            conforms(field::SUBJECT_FIELD, contract, subject, violations)?;
        }
        if let (Some(contract), Some(_)) = (&resource_contract, &subject_contract)
            && !admits(contract, &subject.gts_type)?
        {
            violations.push(FieldViolation::new(
                field::SUBJECT_TYPE_FIELD,
                format!(
                    "'{}' is not admitted by '{}'",
                    subject.gts_type, contract.type_id
                ),
                field::SUBJECT_NOT_ADMITTED,
            ));
        }
        Ok(())
    }

    /// The contract `type_id` in the slot `type_field`, or `None` after a
    /// violation.
    async fn contract(
        &self,
        ctx: &SecurityContext,
        type_field: &str,
        type_id: &str,
        base: &str,
        violations: &mut Vec<FieldViolation>,
    ) -> Result<Option<TypeSchema>, DomainError> {
        let mut violation = |description: String, reason: &str| {
            violations.push(FieldViolation::new(type_field, description, reason));
            Ok(None)
        };
        let contract = match self.lookup(Caller::Request(ctx), type_id).await? {
            Lookup::Found(contract) => *contract,
            Lookup::Unregistered => {
                return violation(
                    format!("licensing contract '{type_id}' is not registered"),
                    field::CONTRACT_NOT_REGISTERED,
                );
            }
            Lookup::Malformed(why) => {
                return violation(
                    format!("'{type_id}' is not a well-formed GTS type id: {why}"),
                    field::CONTRACT_TYPE_MALFORMED,
                );
            }
        };
        if is_abstract(&contract) {
            return violation(
                format!("licensing contract '{type_id}' is abstract"),
                field::CONTRACT_ABSTRACT,
            );
        }
        if !derives_from(&contract.type_id, base) {
            return violation(
                format!("licensing contract '{type_id}' does not derive from '{base}'"),
                field::CONTRACT_NOT_DERIVED,
            );
        }
        Ok(Some(contract))
    }

    async fn derived(
        &self,
        ctx: &SecurityContext,
        field: &str,
        type_id: &str,
        base: &str,
        violations: &mut Vec<FieldViolation>,
        seen: &mut Schemas,
    ) -> Result<Option<TypeSchema>, DomainError> {
        let Some(schema) = self.schema(ctx, type_id, seen).await? else {
            violations.push(FieldViolation::new(
                field,
                format!("type '{type_id}' is not registered"),
                reason::TYPE_NOT_REGISTERED,
            ));
            return Ok(None);
        };
        if !derives_from(&schema.type_id, base) {
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

/// The one order field of `GET /packs`: the id. `$filter` addresses the
/// members (`PackMemberMapper`), not the pack row.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackOrderField {
    Id,
}

impl FilterField for PackOrderField {
    const FIELDS: &'static [Self] = &[Self::Id];

    fn name(&self) -> &'static str {
        "id"
    }

    fn kind(&self) -> FieldKind {
        FieldKind::Uuid
    }
}

/// The lookup a registry answer is: a deleted type is no longer registered,
/// and a registered one without the selected artifacts is unusable, an
/// internal fault rather than an outage a retry cures.
fn found(type_id: &str, schema: TypeSchema) -> Result<Lookup, DomainError> {
    if schema.lifecycle_status != LifecycleStatus::Active {
        return Ok(Lookup::Unregistered);
    }
    let missing = [
        ("content", schema.content.is_none()),
        ("resolved schema", schema.resolved_schema.is_none()),
        ("effective traits", schema.effective_traits.is_none()),
    ]
    .into_iter()
    .find_map(|(field, absent)| absent.then_some(field));
    if let Some(field) = missing {
        return Err(DomainError::Internal(format!(
            "registered contract '{type_id}' is unusable: the registry returned no {field}"
        )));
    }
    Ok(Lookup::Found(Box::new(schema)))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "types_tests.rs"]
mod types_tests;

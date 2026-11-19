//! The RG type definitions resource-group consults, owned by the types registry.
//!
//! Resource-group keeps no type definitions. A group or membership references its
//! GTS type identifier through a local identity dictionary key; the definition
//! behind it is read from the
//! registry when a write needs it, before the write's transaction opens. Reads
//! and cleanup of existing rows never consult it.

use async_trait::async_trait;
use resource_group_sdk::gts::MEMBERSHIP_TYPE_V1;
use serde_json::{Value, json};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use types_registry_sdk::{GtsTypeId, PayloadValidator, TypeSchema};

use crate::domain::error::DomainError;

/// Which definitions a write accepts.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeUse {
    /// A new group or membership: the type must be Active.
    NewLink,
    /// An update or move of an existing group: its type's exact definition,
    /// deleted included (the registry retains a deleted type's content).
    ExistingGroup,
}

/// One RG type definition as admitted by the registry.
#[domain_model]
#[derive(Debug, Clone)]
pub struct RgType {
    /// The GTS type identifier.
    pub code: String,
    /// Concrete membership schema selected by the group type.
    membership_type: Option<String>,
    /// Compiled validator for the group's payload.
    group_payload: PayloadValidator,
    /// Unconditional constraints on the parent, used to attribute a rejected payload.
    placement: Option<PayloadValidator>,
}

impl RgType {
    /// Compile a registry snapshot selected with `Content`, `ResolvedSchema` and `EffectiveTraits`.
    ///
    /// # Errors
    /// Returns an error for a schema that cannot validate group payloads.
    pub fn from_schema(schema: &TypeSchema) -> Result<Self, DomainError> {
        let group_payload = compile(schema)?;
        let code = schema.type_id.as_ref().to_owned();
        // Owners lock the trait with const. Revision checks for that lock are tracked in
        // https://github.com/constructorfabric/gears-rust/issues/6064.
        let membership_type = schema
            .effective_traits
            .as_ref()
            .and_then(|values| values.get("membership_type"))
            .and_then(Value::as_str)
            .filter(|id| concrete_membership_type(id).is_some())
            .map(str::to_owned);
        Ok(Self {
            code,
            membership_type,
            group_payload,
            placement: placement_validator(schema),
        })
    }

    /// The concrete membership schema selected by this group type.
    pub(crate) fn membership_type(&self) -> Result<&str, DomainError> {
        self.membership_type.as_deref().ok_or_else(|| {
            DomainError::validation(format!(
                "Group type '{}' must declare a concrete membership_type trait",
                self.code
            ))
        })
    }

    pub(crate) fn validate_group(
        &self,
        parent_type: Option<&str>,
        metadata: Option<&Value>,
    ) -> Result<(), DomainError> {
        let mut payload = json!({ "parent_type": parent_type });
        if let Some(metadata) = metadata {
            payload["metadata"] = metadata.clone();
        }
        self.group_payload.validate(&payload).map_err(|error| {
            let message = format!("Payload does not match type '{}': {error}", self.code);
            let rejects_parent = match &error {
                types_registry_sdk::PayloadValidationError::References(issues) => issues
                    .iter()
                    .any(|issue| issue.field_path == "/parent_type"),
                _ => self
                    .placement
                    .as_ref()
                    .is_some_and(|rule| !rule.is_valid(&payload["parent_type"])),
            };
            if rejects_parent {
                DomainError::invalid_parent_type(message)
            } else {
                DomainError::validation(message)
            }
        })
    }
}

/// GTS 0.17 reports ordinary JSON Schema violations without their instance paths.
/// Check only parent rules that necessarily apply (properties at the root and in
/// `allOf`). Conditional constraints involving other fields remain payload errors.
fn placement_validator(schema: &TypeSchema) -> Option<PayloadValidator> {
    fn parent_rules(node: &Value, path: &str, rules: &mut Vec<Value>) {
        if node.get("$ref").is_some() {
            return;
        }
        if node.pointer("/properties/parent_type").is_some() {
            rules.push(json!({"$ref": format!("{path}/properties/parent_type")}));
        }
        if let Some(branches) = node.get("allOf").and_then(Value::as_array) {
            for (index, branch) in branches.iter().enumerate() {
                parent_rules(branch, &format!("{path}/allOf/{index}"), rules);
            }
        }
    }
    let resolved = schema.resolved_schema.as_ref()?;
    let id = resolved
        .get("$id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("gts://{}", schema.type_id), str::to_owned);
    let mut rules = Vec::new();
    parent_rules(resolved, &format!("{id}#"), &mut rules);
    if rules.is_empty() {
        return None;
    }

    // Keep the original resource intact so references from parent rules still
    // resolve against their original paths, definitions and dialect.
    let mut snapshot = schema.clone();
    let mut source = snapshot.resolved_schema.take()?;
    source["$id"] = Value::String(id);
    let mut diagnostic = json!({"definitions": {"group": source}, "allOf": rules});
    if let Some(dialect) = resolved.get("$schema") {
        diagnostic["$schema"] = dialect.clone();
    }
    snapshot.resolved_schema = Some(diagnostic);
    snapshot.compile_payload_validator().inspect_err(|error| {
        tracing::warn!(code = %schema.type_id, %error, "cannot attribute placement errors for group schema");
    }).ok()
}

/// A concrete membership schema: a valid GTS type strictly derived from
/// [`MEMBERSHIP_TYPE_V1`]. The base itself selects no member types.
pub(crate) fn concrete_membership_type(id: &str) -> Option<GtsTypeId> {
    if id.starts_with(MEMBERSHIP_TYPE_V1) && id != MEMBERSHIP_TYPE_V1 {
        GtsTypeId::try_new(id).ok()
    } else {
        None
    }
}

/// A concrete membership schema as admitted by the registry: which resource
/// types may join a group bound to it.
#[domain_model]
#[derive(Debug, Clone)]
pub struct MembershipSchema {
    /// The membership schema's GTS type identifier.
    pub code: String,
    /// Compiled validator for a membership payload.
    payload: PayloadValidator,
}

impl MembershipSchema {
    /// Compile a registry snapshot selected with `Content` and `ResolvedSchema`.
    ///
    /// # Errors
    /// Returns an error for a schema that cannot validate membership payloads.
    pub fn from_schema(schema: &TypeSchema) -> Result<Self, DomainError> {
        Ok(Self {
            code: schema.type_id.as_ref().to_owned(),
            payload: compile(schema)?,
        })
    }

    /// Check that the schema admits members of `resource_type`.
    pub(crate) fn validate_member(&self, resource_type: &str) -> Result<(), DomainError> {
        validate(
            &self.payload,
            &json!({ "resource_type": resource_type }),
            &self.code,
        )
    }
}

pub(crate) fn compile(schema: &TypeSchema) -> Result<PayloadValidator, DomainError> {
    use types_registry_sdk::CompilePayloadValidatorError as E;
    use types_registry_sdk::GtsCompilePayloadValidatorError as Gts;

    schema
        .compile_payload_validator()
        .map_err(|error| match error {
            // The caller named a type that has no direct instances.
            E::Upstream(Gts::Abstract(_)) => DomainError::validation(format!(
                "Type '{}' accepts no payload: {error}",
                schema.type_id
            )),
            // An incomplete snapshot or an admitted schema GTS cannot compile
            // is a registry or owner defect, not the caller's.
            other => {
                tracing::error!(code = %schema.type_id, error = %other, "type schema does not compile");
                DomainError::InternalError
            }
        })
}

pub(crate) fn validate(
    validator: &PayloadValidator,
    payload: &Value,
    code: &str,
) -> Result<(), DomainError> {
    validator.validate(payload).map_err(|error| {
        DomainError::validation(format!("Payload does not match type '{code}': {error}"))
    })
}

/// Reads RG type definitions from the types registry under the caller's
/// context. Never called inside a database transaction.
#[async_trait]
pub trait RgTypeCatalog: Send + Sync {
    /// The definition of `code` for `usage`.
    ///
    /// # Errors
    /// [`DomainError::TypeNotFound`] for an unknown type, or a deleted one for
    /// [`TypeUse::NewLink`]; [`DomainError::RegistryRefusedCaller`] when the
    /// registry refuses the caller (401/403, unchanged);
    /// [`DomainError::ServiceUnavailable`] for an unavailable registry; other
    /// registry failures are internal.
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
        usage: TypeUse,
    ) -> Result<RgType, DomainError>;

    /// The active concrete membership schema `membership_type`, compiled.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for an identifier that is not a concrete
    /// membership schema; [`DomainError::TypeNotFound`] for an unknown or
    /// deleted one; registry failures as for [`Self::get_type`].
    async fn get_membership_schema(
        &self,
        ctx: &SecurityContext,
        membership_type: &str,
    ) -> Result<MembershipSchema, DomainError>;

    /// Resolve the active group type's current membership schema and check
    /// that the candidate member type is registered, active and concrete.
    ///
    /// # Errors
    /// As [`Self::get_type`] for the group type (checked first), then
    /// [`DomainError::Validation`] for an unknown, deleted or abstract member type,
    /// then as [`Self::get_membership_schema`].
    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_type: &str,
        resource_type: &str,
    ) -> Result<MembershipSchema, DomainError>;
}

#[cfg(test)]
#[path = "type_catalog_tests.rs"]
mod type_catalog_tests;

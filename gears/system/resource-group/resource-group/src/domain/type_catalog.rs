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
}

impl RgType {
    /// Compile a registry snapshot selected with `Content` and `ResolvedSchema`.
    ///
    /// # Errors
    /// Returns an error for a schema that cannot validate group payloads.
    pub fn from_schema(schema: &TypeSchema) -> Result<Self, DomainError> {
        fn binding(schema: &Value) -> Option<&str> {
            schema
                .pointer("/properties/membership_type/const")
                .and_then(Value::as_str)
                .or_else(|| schema.get("allOf")?.as_array()?.iter().find_map(binding))
        }

        let group_payload = compile(schema)?;
        let code = schema.type_id.as_ref().to_owned();
        let membership_type = schema
            .resolved_schema
            .as_ref()
            .and_then(binding)
            .filter(|id| concrete_membership_type(id).is_some())
            .map(str::to_owned);
        Ok(Self {
            code,
            membership_type,
            group_payload,
        })
    }

    /// Resolve the binding for a new group or a row predating the binding column.
    pub(crate) fn membership_type(&self) -> Result<&str, DomainError> {
        self.membership_type.as_deref().ok_or_else(|| {
            DomainError::validation(format!(
                "Group type '{}' must fix membership_type to a concrete membership type with const",
                self.code
            ))
        })
    }

    pub(crate) fn validate_group(
        &self,
        parent_type: Option<&str>,
        membership_type: Option<&str>,
        metadata: Option<&Value>,
    ) -> Result<(), DomainError> {
        let binding = match membership_type {
            Some(stored) => stored,
            None => self.membership_type()?,
        };
        let mut payload = json!({ "parent_type": parent_type, "membership_type": binding });
        if let Some(metadata) = metadata {
            payload["metadata"] = metadata.clone();
        }
        validate(&self.group_payload, &payload, &self.code)
    }
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

    schema
        .compile_payload_validator()
        .map_err(|error| match error {
            E::MissingField { .. } | E::InvalidContent { .. } => {
                tracing::error!(code = %schema.type_id, %error, "incomplete type snapshot");
                DomainError::InternalError
            }
            other => DomainError::validation(format!(
                "Type '{}' accepts no payload: {other}",
                schema.type_id
            )),
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

    /// The rule for a new membership in a group of `group_type`: the group
    /// type must be Active, `resource_type` must be an Active registered
    /// type, and the result is the group's membership schema — the stored
    /// `binding`, or the one the group type fixes for a row predating the
    /// stored binding. Checks the member type in the same registry read; with
    /// a stored binding, the group type too.
    ///
    /// # Errors
    /// As [`Self::get_type`] for the group type (checked first), then
    /// [`DomainError::Validation`] for an unknown or deleted member type,
    /// then as [`Self::get_membership_schema`].
    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_type: &str,
        binding: Option<&str>,
        resource_type: &str,
    ) -> Result<MembershipSchema, DomainError>;
}

#[cfg(test)]
#[path = "type_catalog_tests.rs"]
mod type_catalog_tests;

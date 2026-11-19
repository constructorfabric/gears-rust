//! The RG type definitions resource-group consults, owned by the types registry.
//!
//! Resource-group keeps no type definitions. A group or membership references its
//! GTS type identifier through a local identity dictionary key; the definition
//! behind it — placement traits and the payload schema — is read from the
//! registry when a write needs it, before the write's transaction opens. Reads
//! and cleanup of existing rows never consult it.

use async_trait::async_trait;
use resource_group_sdk::gts::ResourceGroupTypeTraitsV1;
use serde_json::{Value, json};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use types_registry_sdk::TypeSchema;

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
    /// Materialized `effective_traits`.
    pub traits: ResourceGroupTypeTraitsV1,
    /// The registry snapshot the payload validator compiles from.
    schema: TypeSchema,
}

impl RgType {
    /// Assemble from a registry snapshot selected with `Content`,
    /// `ResolvedSchema` and `EffectiveTraits`.
    ///
    /// # Errors
    /// [`DomainError::InternalError`] when the snapshot misses the selected
    /// traits or they do not match the RG traits schema: admission guarantees
    /// both for an RG type, so either is a contract break.
    pub fn from_schema(schema: TypeSchema) -> Result<Self, DomainError> {
        let code = schema.type_id.as_ref().to_owned();
        let traits = schema.effective_traits.clone().ok_or_else(|| {
            tracing::error!(%code, "types-registry answered without the selected effective_traits");
            DomainError::InternalError
        })?;
        let traits = serde_json::from_value(traits).map_err(|error| {
            tracing::error!(%code, %error, "effective_traits do not match the RG traits schema");
            DomainError::InternalError
        })?;
        Ok(Self {
            code,
            traits,
            schema,
        })
    }

    /// Whether a group of this type may be placed under a group of `parent_code`.
    #[must_use]
    pub fn allows_parent(&self, parent_code: &str) -> bool {
        self.traits
            .allowed_parent_types
            .iter()
            .any(|p| p.0 == parent_code)
    }

    /// Whether `resource_type` may be a member of a group of this type.
    #[must_use]
    pub fn allows_member(&self, resource_type: &str) -> bool {
        self.traits
            .allowed_membership_types
            .iter()
            .any(|m| m.0 == resource_type)
    }

    /// Validate a group's payload with the SDK's payload validator compiled
    /// from the registry's resolved schema. The payload is resource-group's
    /// group envelope, `{"metadata": ...}` (without the key when absent); the
    /// schema itself is used as resolved, never rewritten.
    ///
    /// # Errors
    /// [`DomainError::Validation`] for a payload the type refuses or a type
    /// that accepts no payload (abstract); [`DomainError::InternalError`] for a
    /// snapshot missing the selected documents.
    pub fn validate_metadata(&self, metadata: Option<&Value>) -> Result<(), DomainError> {
        use types_registry_sdk::CompilePayloadValidatorError as E;

        let validator = self
            .schema
            .compile_payload_validator()
            .map_err(|error| match error {
                E::MissingField { .. } | E::InvalidContent { .. } => {
                    tracing::error!(code = %self.code, %error, "incomplete type snapshot");
                    DomainError::InternalError
                }
                other => DomainError::validation(format!(
                    "Type '{}' accepts no group payload: {other}",
                    self.code
                )),
            })?;
        let payload = metadata.map_or_else(|| json!({}), |m| json!({ "metadata": m }));
        validator.validate(&payload).map_err(|error| {
            DomainError::validation(format!("Metadata does not match type schema: {error}"))
        })
    }
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

    /// For a new membership: the Active group type's placement traits, after
    /// checking the member resource type — any GTS type, not necessarily an RG
    /// type — is registered and Active. One registry read for both.
    ///
    /// The returned [`RgType`] carries only `effective_traits`;
    /// [`RgType::validate_metadata`] on it is an internal error.
    ///
    /// # Errors
    /// As [`Self::get_type`] for the group type; [`DomainError::Validation`]
    /// for an unknown or deleted member resource type.
    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_code: &str,
        member_code: &str,
    ) -> Result<RgType, DomainError>;
}

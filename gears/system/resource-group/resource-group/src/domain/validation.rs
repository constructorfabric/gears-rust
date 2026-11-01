// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full
//! Shared domain validation utilities.

use crate::domain::error::DomainError;
use crate::domain::type_catalog::RgTypeDef;
use toolkit_gts::gts_id;

/// GTS type path prefix required for resource group types.
pub const RG_TYPE_PREFIX: &str = gts_id!("cf.core.rg.type.v1~");

/// Validate a GTS type code: non-empty, correct prefix, length limit.
///
/// Input is normalized (trimmed and lowercased) before validation, consistent
/// with [`resource_group_sdk::models::GtsTypePath::new`].
///
/// # Errors
///
/// Returns [`DomainError`] if the code is empty, missing the required prefix, or exceeds 1024 chars.
// @cpt-algo:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1
// @cpt-algo:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1
pub fn validate_type_code(code: &str) -> Result<(), DomainError> {
    let code = code.trim().to_lowercase();
    let code = code.as_str();
    // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-1
    if code.is_empty() {
        return Err(DomainError::validation("Type code must not be empty"));
    }
    // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-1
    // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2
    if !code.starts_with(RG_TYPE_PREFIX) {
        // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2a
        return Err(DomainError::validation(format!(
            "Type code must start with prefix '{RG_TYPE_PREFIX}', got: '{code}'"
        )));
        // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2a
    }
    // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-2
    // @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-3
    if code.chars().count() > 1024 {
        return Err(DomainError::validation(
            "Type code must not exceed 1024 characters",
        ));
    }
    // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-3
    Ok(())
}

/// Validate group metadata against the resolved schema of the group's type.
///
/// The Types Registry resolves an RG type to an `allOf` document with the
/// base type inlined, so there is no top-level `properties.metadata`; the
/// wrapper document `{"metadata": <metadata>}` is therefore validated against
/// the WHOLE resolved schema.
///
/// Passes trivially when `metadata` is `None` or the type carries no schema.
///
/// # Errors
///
/// Returns [`DomainError::validation`] when the schema fails to compile or
/// the metadata violates any schema constraint.
pub fn validate_metadata_against_def(
    metadata: Option<&serde_json::Value>,
    def: &RgTypeDef,
) -> Result<(), DomainError> {
    let (Some(metadata), Some(schema)) = (metadata, def.schema.as_ref()) else {
        return Ok(());
    };

    let validator = jsonschema::validator_for(schema)
        .map_err(|e| DomainError::validation(format!("Type metadata_schema is invalid: {e}")))?;

    let document = serde_json::json!({ "metadata": metadata });
    let errors: Vec<String> = validator
        .iter_errors(&document)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(DomainError::validation(format!(
            "Metadata does not match type schema: {}",
            errors.join("; ")
        )));
    }
    Ok(())
}
// @cpt-end:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;

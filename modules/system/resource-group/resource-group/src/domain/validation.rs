// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full
//! Shared domain validation utilities.

use crate::domain::error::DomainError;

/// GTS type path prefix required for resource group types.
pub const RG_TYPE_PREFIX: &str = "gts.x.system.rg.type.v1~";

/// Validate a GTS type code: non-empty, correct prefix, length limit.
// @cpt-algo:cpt-cf-resource-group-algo-sdk-foundation-validate-gts-type-path:p1
// @cpt-algo:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1
pub fn validate_type_code(code: &str) -> Result<(), DomainError> {
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
    if code.len() > 1024 {
        return Err(DomainError::validation(
            "Type code must not exceed 1024 characters",
        ));
    }
    // @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-3
    Ok(())
}

/// Validate that a `metadata_schema` value is a valid JSON Schema.
///
/// Attempts to compile the schema via `jsonschema::validator_for`. If the value
/// cannot be interpreted as a JSON Schema, returns a [`DomainError::validation`].
// @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7
pub fn validate_metadata_schema(schema: &serde_json::Value) -> Result<(), DomainError> {
    jsonschema::validator_for(schema).map_err(|e| {
        DomainError::validation(format!("metadata_schema is not a valid JSON Schema: {e}"))
    })?;
    Ok(())
}
// @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7

/// Validate a metadata value against a compiled JSON Schema.
///
/// Returns `Ok(())` when:
/// - `schema` is `None` (no schema = no validation, any metadata accepted)
/// - `metadata` is `None` (nothing to validate)
/// - `metadata` validates against the schema
///
/// Returns `Err` when metadata violates the schema constraints.
pub fn validate_metadata_against_schema(
    metadata: Option<&serde_json::Value>,
    schema: Option<&serde_json::Value>,
) -> Result<(), DomainError> {
    let (Some(metadata), Some(schema)) = (metadata, schema) else {
        return Ok(());
    };

    let validator = jsonschema::validator_for(schema)
        .map_err(|e| DomainError::validation(format!("Type metadata_schema is invalid: {e}")))?;

    let errors: Vec<String> = validator
        .iter_errors(metadata)
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

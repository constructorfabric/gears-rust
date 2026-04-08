// Created: 2026-04-16 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-sdk-foundation-sdk-models:p1:inst-validation-full
//! Shared domain validation utilities.

use crate::domain::error::DomainError;

/// GTS type path prefix required for resource group types.
pub const RG_TYPE_PREFIX: &str = "gts.cf.core.rg.type.v1~";

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

/// Validate that a `metadata_schema` value is a valid JSON Schema.
///
/// Attempts to compile the schema via `jsonschema::validator_for`. If the value
/// cannot be interpreted as a JSON Schema, returns a [`DomainError::validation`].
///
/// # Errors
///
/// Returns [`DomainError`] if the value is not a valid JSON Schema.
// @cpt-begin:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7
pub fn validate_metadata_schema(schema: &serde_json::Value) -> Result<(), DomainError> {
    jsonschema::validator_for(schema).map_err(|e| {
        DomainError::validation(format!("metadata_schema is not a valid JSON Schema: {e}"))
    })?;
    Ok(())
}
// @cpt-end:cpt-cf-resource-group-algo-type-mgmt-validate-type-input:p1:inst-val-input-7

/// Validate a metadata value against a resolved GTS type schema.
///
/// Uses `TypesRegistryClient` to fetch the resolved schema (with `allOf`
/// composition, `$ref` resolution, and `x-gts-traits` applied), then validates
/// the metadata against the resolved schema using `jsonschema`.
///
/// Returns `Ok(())` when:
/// - `metadata` is `None` (nothing to validate)
/// - `type_code` has no registered schema in the types registry
/// - `metadata` validates against the resolved schema
///
/// # Errors
///
/// Returns [`DomainError`] when metadata violates the schema constraints
/// or the types registry is unavailable.
pub async fn validate_metadata_via_gts(
    metadata: Option<&serde_json::Value>,
    type_code: &str,
    types_registry: &dyn types_registry_sdk::TypesRegistryClient,
) -> Result<(), DomainError> {
    let Some(metadata) = metadata else {
        return Ok(());
    };

    // Fetch the GTS entity — its content contains the resolved schema
    // including allOf composition and $ref resolution from types-registry.
    let entity = types_registry.get(type_code).await.map_err(|e| {
        DomainError::validation(format!(
            "Failed to resolve GTS type '{type_code}' for metadata validation: {e}"
        ))
    })?;

    // Extract metadata sub-schema from the GTS entity content.
    // The chained RG type schema defines a `metadata` property within
    // its `properties` object.
    let metadata_schema = entity
        .content
        .get("properties")
        .and_then(|p| p.get("metadata"));

    let Some(metadata_schema) = metadata_schema else {
        // No metadata property in the schema — any metadata accepted.
        return Ok(());
    };

    let validator = jsonschema::validator_for(metadata_schema)
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
